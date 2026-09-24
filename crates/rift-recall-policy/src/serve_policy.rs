//! The serve-policy file: the template a repository starts from, the parsed
//! overlay, and the loader that reads it without following a link. Its own
//! module because this is where fail-closed is decided: a file that is absent
//! is the default, and a file that is present and unreadable turns every lever
//! off.

use std::path::Path;

use crate::{AgentModes, DocPolicy, InterceptionPolicy, Mode, SearchPolicy};

/// The commented `.rift/policies/serve.toml` block a repository starts from.
///
/// Every value here is the shipped default spelled out, and a test asserts it
/// parses back to exactly that — so the comments cannot drift into describing a
/// policy Rift does not have. Deleting a key, a table or the whole file is the
/// same as leaving it: absent means the default. A key Rift does not know, a
/// value outside its range, or TOML that does not parse means the file does not
/// say what the operator permitted, and every lever below goes `off`.
pub const SERVE_POLICY_TEMPLATE: &str = r#"# Rift serving policy. Absent = these defaults; unreadable = every lever off.

# Declining a re-read of a range this session's journal proves the context still
# holds. `observe` reaches the decision and records it without touching the
# call; `enforce` declines it once, with a reason, and allows the repeat.
[interception]
mode = "enforce"
max_slice_bytes = 98304        # 96 KiB: the measured point a read result truncates at
min_slice_lines = 28           # under this a reason frame costs more than the read it replaces
max_inline_hash_bytes = 1048576 # 1 MiB: larger files are never hashed on the gate path
recovery_budget = 800          # tokens a post-compaction recovery map may cost
basis_ttl_days = 30            # a read-time basis snapshot is collected after this

# Per-family overrides. Each is capped by what that family's pinned rows allow:
# a family whose reason channel no recorded run has confirmed resolves to
# `observe` however this table is written. Retry telemetry never changes mode.
[interception.agents]
claude-code = "enforce"
codex = "enforce"
antigravity = "enforce"

# Bounding a search instead of reading its results. The head limit is added to
# a content search that carries none — it declines nothing and is not held to
# `mode`; answering a listing is a decline and is.
[search]
mode = "enforce"
grep_head_limit = 40
glob_top_n = 8
answer_budget_tokens = 160     # tokens an answered listing may cost

# Reading a document as Markdown. `rewrite` amends the read's path to Rift's
# sidecar where the family documents that; `redirect` declines the read once
# and names the sidecar instead.
[doc]
mode = "enforce"
formats = "all"
channel = "rewrite"
table_rows = 200               # rows kept per sheet, every sheet
max_input_bytes = 16777216     # 16 MiB: larger documents are never converted
pdf_min_chars_per_page = 200   # thinner than this, the PDF is left alone
convert_budget_ms = 250        # then the read proceeds and the worker converts
ttl_days = 30                  # an unused sidecar is collected after this
"#;

/// The per-turn injected-manifest ceiling when the policy is silent
/// Small on purpose: a manifest is a pointer at a delta, not
/// the delta — the agent pulls detail with the verbs the manifest names.
pub const DEFAULT_INJECTION_TURN_BUDGET: u32 = 400;

/// The serve-policy overlay loaded at server start: path globs never served, the
/// per-path raw-transcript allow-list, and the per-turn injection ceiling.
/// `fail_closed` is set when a policy file is present but unparseable — the
/// server then denies **all** exact retrieval rather than serve under an
/// unknown policy.
#[derive(Debug, Clone)]
pub struct ServePolicy {
    /// Path globs that are never served (not even by handle).
    pub denied_paths: Vec<String>,
    /// Path globs for which raw transcript retrieval is explicitly enabled.
    pub raw_transcript_globs: Vec<String>,
    /// Fail-closed: policy present but unparseable → deny all exact retrieval.
    pub fail_closed: bool,
    /// Tokens a single turn's injected manifest may cost. `0` disables per-turn
    /// injection fully.
    pub injection_turn_budget: u32,
    /// `[interception]` — the read-interception lever.
    pub interception: InterceptionPolicy,
    /// `[search]` — the search levers.
    pub search: SearchPolicy,
    /// `[doc]` — the document levers.
    pub doc: DocPolicy,
}

/// Where the Recall serve overlay came from and whether it can be edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServePolicySource {
    /// A valid repository-owned file was applied.
    Declared,
    /// No file exists; defaults apply.
    Default,
    /// A file exists but is invalid; fail-closed policy applies.
    Invalid,
    /// `.rift` or the policy file is a symlink/special file and was refused.
    Refused,
}

/// Exact source revision used for conflict-aware edits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServePolicyRevision {
    source_hash: Option<String>,
}

impl ServePolicyRevision {
    /// The BLAKE3 source hash, or `None` when the file was absent.
    #[must_use]
    pub fn source_hash(&self) -> Option<&str> {
        self.source_hash.as_deref()
    }
}

/// Policy, source validity, and revision as one coherent read.
#[derive(Debug, Clone)]
pub struct ServePolicySnapshot {
    /// Effective serve policy (fail-closed when invalid/refused).
    pub policy: ServePolicy,
    /// Source/validity summary.
    pub source: ServePolicySource,
    /// Revision required by [`patch_injection_turn_budget`](crate::patch_injection_turn_budget).
    pub revision: ServePolicyRevision,
}

/// The parsed `.rift/policies/serve.toml` overlay (TOML, `deny_unknown_fields`).
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ServePolicyFile {
    #[serde(default)]
    denied_paths: Vec<String>,
    #[serde(default)]
    raw_transcript_globs: Vec<String>,
    /// Absent means "the default", not "zero" — hence `Option`, so an operator
    /// who writes `injection_turn_budget = 0` gets injection **off** rather
    /// than silently getting 400.
    #[serde(default)]
    injection_turn_budget: Option<u32>,
    #[serde(default)]
    interception: InterceptionPolicy,
    #[serde(default)]
    search: SearchPolicy,
    #[serde(default)]
    doc: DocPolicy,
}

pub(crate) enum OwnedPolicy {
    Absent,
    Refused,
    Bytes(Vec<u8>),
}

impl Default for ServePolicy {
    fn default() -> Self {
        Self {
            denied_paths: Vec::new(),
            raw_transcript_globs: Vec::new(),
            fail_closed: false,
            injection_turn_budget: DEFAULT_INJECTION_TURN_BUDGET,
            interception: InterceptionPolicy::default(),
            search: SearchPolicy::default(),
            doc: DocPolicy::default(),
        }
    }
}

impl ServePolicy {
    /// Load `.rift/policies/serve.toml`. **Absent** → the permissive default (no
    /// extra denies, transcripts still off, injection at
    /// [`DEFAULT_INJECTION_TURN_BUDGET`]). **Present but unparseable** →
    /// [`ServePolicy::fail_closed`] (deny all exact retrieval, W-fail-closed).
    ///
    /// Fail-closed also means **no injection**: injection spends the agent's
    /// context without being asked, and an unreadable policy is precisely the
    /// state in which Rift does not know what the operator permitted.
    #[must_use]
    pub fn load(rift_dir: &Path) -> Self {
        load_serve_policy(rift_dir).policy
    }
}

/// Read the Recall overlay without following repository-shippable links.
#[must_use]
pub fn load_serve_policy(rift_dir: &Path) -> ServePolicySnapshot {
    let path = rift_dir.join("policies").join("serve.toml");
    match owned_policy_bytes(rift_dir, &path) {
        OwnedPolicy::Absent => ServePolicySnapshot {
            policy: ServePolicy::default(),
            source: ServePolicySource::Default,
            revision: ServePolicyRevision::default(),
        },
        OwnedPolicy::Refused => ServePolicySnapshot {
            policy: fail_closed_policy(),
            source: ServePolicySource::Refused,
            revision: ServePolicyRevision::default(),
        },
        OwnedPolicy::Bytes(bytes) => {
            let revision = ServePolicyRevision {
                source_hash: Some(blake3::hash(&bytes).to_hex().to_string()),
            };
            let parsed = std::str::from_utf8(&bytes).ok().and_then(parse_policy);
            match parsed {
                Some(policy) => ServePolicySnapshot {
                    policy,
                    source: ServePolicySource::Declared,
                    revision,
                },
                None => ServePolicySnapshot {
                    policy: fail_closed_policy(),
                    source: ServePolicySource::Invalid,
                    revision,
                },
            }
        }
    }
}

pub(crate) fn owned_policy_bytes(rift_dir: &Path, path: &Path) -> OwnedPolicy {
    match std::fs::symlink_metadata(rift_dir) {
        Ok(meta) if !meta.is_dir() => return OwnedPolicy::Refused,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return OwnedPolicy::Absent,
        Err(_) => return OwnedPolicy::Refused,
        _ => {}
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_symlink() || !meta.is_file() => OwnedPolicy::Refused,
        Ok(_) => std::fs::read(path).map_or(OwnedPolicy::Refused, OwnedPolicy::Bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => OwnedPolicy::Absent,
        Err(_) => OwnedPolicy::Refused,
    }
}

/// Parse and range-check one policy source. `None` is the only failure this
/// crate distinguishes, because every failure has the same answer: the file
/// does not say what the operator permitted, so nothing acts.
pub(crate) fn parse_policy(text: &str) -> Option<ServePolicy> {
    let file = toml::from_str::<ServePolicyFile>(text).ok()?;
    if !ranges_are_coherent(&file) {
        return None;
    }
    Some(policy_from_file(file))
}

/// Every bound a lever reads has to be usable on its own terms, and a slice a
/// decision may cover cannot exceed what the gate path is willing to hash. A
/// number that fails here is not clamped to something plausible: a policy Rift
/// would have to guess at is a policy it does not have.
fn ranges_are_coherent(file: &ServePolicyFile) -> bool {
    let (i, s, d) = (&file.interception, &file.search, &file.doc);
    i.max_slice_bytes > 0
        && i.min_slice_lines >= 1
        && i.max_inline_hash_bytes > 0
        && i.max_slice_bytes <= i.max_inline_hash_bytes
        && i.recovery_budget > 0
        && i.basis_ttl_days >= 1
        && s.grep_head_limit > 0
        && s.glob_top_n > 0
        && s.answer_budget_tokens > 0
        && d.table_rows > 0
        && d.max_input_bytes > 0
        && d.pdf_min_chars_per_page > 0
        && d.convert_budget_ms > 0
        && d.ttl_days >= 1
}

fn policy_from_file(file: ServePolicyFile) -> ServePolicy {
    ServePolicy {
        denied_paths: file.denied_paths,
        raw_transcript_globs: file.raw_transcript_globs,
        fail_closed: false,
        injection_turn_budget: file
            .injection_turn_budget
            .unwrap_or(DEFAULT_INJECTION_TURN_BUDGET),
        interception: file.interception,
        search: file.search,
        doc: file.doc,
    }
}

/// The policy Rift holds when it cannot read the one on disk: no exact
/// retrieval, no injection, and every active lever `off`. The bounds keep
/// their shipped values so a reader of this state is not shown zeros that look
/// like configured numbers — with every mode `off`, no lever consults them.
fn fail_closed_policy() -> ServePolicy {
    ServePolicy {
        denied_paths: Vec::new(),
        raw_transcript_globs: Vec::new(),
        fail_closed: true,
        injection_turn_budget: 0,
        interception: InterceptionPolicy {
            mode: Mode::Off,
            agents: AgentModes::default(),
            ..InterceptionPolicy::default()
        },
        search: SearchPolicy {
            mode: Mode::Off,
            ..SearchPolicy::default()
        },
        doc: DocPolicy {
            mode: Mode::Off,
            ..DocPolicy::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        resolve, FamilyCapability, DEFAULT_MAX_INLINE_HASH_BYTES, DEFAULT_MAX_INPUT_BYTES,
        DEFAULT_MAX_SLICE_BYTES, DEFAULT_MIN_SLICE_LINES,
    };

    #[test]
    fn absent_policy_is_permissive_default() {
        let dir = tempfile::tempdir().unwrap();
        let sp = ServePolicy::load(dir.path());
        assert!(!sp.fail_closed);
        assert!(sp.denied_paths.is_empty());
        assert_eq!(sp.injection_turn_budget, DEFAULT_INJECTION_TURN_BUDGET);
    }

    /// The injection ceiling is a policy knob with three distinguishable
    /// states, and the difference between two of them is the whole point:
    /// *absent* means the default, *zero* means the operator turned per-turn
    /// injection off. A plain `#[serde(default)] u32` would collapse them.
    #[test]
    fn the_injection_budget_separates_absent_from_off() {
        let write = |src: &str| {
            let dir = tempfile::tempdir().unwrap();
            let pol = dir.path().join("policies");
            std::fs::create_dir_all(&pol).unwrap();
            std::fs::write(pol.join("serve.toml"), src).unwrap();
            (ServePolicy::load(dir.path()), dir)
        };
        let (absent, _d) = write("denied_paths = []\n");
        assert_eq!(absent.injection_turn_budget, DEFAULT_INJECTION_TURN_BUDGET);
        assert_eq!(
            DEFAULT_INJECTION_TURN_BUDGET, 400,
            "the injected-manifest ceiling stays a pointer's worth of tokens"
        );

        let (off, _d) = write("injection_turn_budget = 0\n");
        assert!(!off.fail_closed, "zero is a choice, not a parse failure");
        assert_eq!(off.injection_turn_budget, 0);

        let (raised, _d) = write("injection_turn_budget = 900\n");
        assert_eq!(raised.injection_turn_budget, 900);
    }

    /// Fail-closed reaches injection too. An unreadable policy is exactly the
    /// state in which Rift does not know what the operator permitted, and
    /// injection spends the agent's context without being asked — so it stops,
    /// rather than falling back to the default ceiling.
    #[test]
    fn an_unparseable_policy_injects_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        std::fs::write(pol.join("serve.toml"), "injection_turn_budget = \"lots\"").unwrap();
        let sp = ServePolicy::load(dir.path());
        assert!(sp.fail_closed);
        assert_eq!(sp.injection_turn_budget, 0);
    }

    /// Load a policy source through the real file path, so every test below
    /// exercises the loader the hook path uses rather than a parse helper.
    fn load_src(src: &str) -> (ServePolicySnapshot, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        std::fs::write(pol.join("serve.toml"), src).unwrap();
        (load_serve_policy(dir.path()), dir)
    }

    /// Claude Code as its pinned rows stand: a confirmed reason channel and
    /// both amendments.
    const FULL: FamilyCapability = FamilyCapability {
        confirmed_reason_channel: true,
        document_sidecar_rewrite: true,
        grep_head_limit_rewrite: true,
        pinned_edit_echo: true,
    };

    /// The commented file an operator is handed has to describe the policy
    /// that actually runs. It does, because it *is* that policy: every value
    /// in it is written out, and parsing it back yields the defaults.
    #[test]
    fn the_template_parses_back_to_the_shipped_defaults() {
        let (loaded, _d) = load_src(SERVE_POLICY_TEMPLATE);
        assert_eq!(loaded.source, ServePolicySource::Declared);
        assert_eq!(
            InterceptionPolicy {
                agents: AgentModes::default(),
                ..loaded.policy.interception
            },
            InterceptionPolicy::default()
        );
        assert_eq!(loaded.policy.search, SearchPolicy::default());
        assert_eq!(loaded.policy.doc, DocPolicy::default());
        // The per-family table is spelled out in the template and empty in the
        // default, and the two must still mean the same thing.
        assert_eq!(
            loaded.policy.interception.agents.for_family("claude-code"),
            Some(Mode::Enforce)
        );
        assert_eq!(
            resolve(&loaded.policy, "claude-code", FULL),
            resolve(&ServePolicy::default(), "claude-code", FULL),
        );
        assert_eq!(DEFAULT_MAX_SLICE_BYTES, 96 * 1024);
        assert_eq!(DEFAULT_MIN_SLICE_LINES, 28);
        assert_eq!(DEFAULT_MAX_INLINE_HASH_BYTES, 1024 * 1024);
        assert_eq!(DEFAULT_MAX_INPUT_BYTES, 16 * 1024 * 1024);
    }

    /// Absent is the default, table by table and key by key — a file that
    /// mentions one lever does not silently shut the other two.
    #[test]
    fn an_absent_table_is_the_documented_default() {
        let (partial, _d) = load_src("[search]\nglob_top_n = 3\n");
        assert_eq!(partial.source, ServePolicySource::Declared);
        assert_eq!(partial.policy.search.glob_top_n, 3);
        assert_eq!(partial.policy.search.grep_head_limit, 40);
        assert_eq!(partial.policy.search.mode, Mode::Enforce);
        assert_eq!(partial.policy.interception, InterceptionPolicy::default());
        assert_eq!(partial.policy.doc, DocPolicy::default());

        let (absent, _d) = load_src("denied_paths = []\n");
        assert_eq!(absent.policy.interception, InterceptionPolicy::default());
        assert_eq!(absent.policy.search, SearchPolicy::default());
        assert_eq!(absent.policy.doc, DocPolicy::default());
    }

    /// The bounds an operator may legitimately move, moved — so the range
    /// check is a check on the range and not a check that the numbers are the
    /// shipped ones.
    #[test]
    fn the_bounds_are_the_operators_to_move_inside_their_range() {
        let (loaded, _d) = load_src(
            "[interception]\nmax_slice_bytes = 4096\nmin_slice_lines = 1\nmax_inline_hash_bytes = 4096\nrecovery_budget = 1\n\n[doc]\nttl_days = 1\nmax_input_bytes = 1\n",
        );
        assert_eq!(loaded.source, ServePolicySource::Declared);
        let i = loaded.policy.interception;
        assert_eq!(
            i.min_slice_lines, 1,
            "one line is the lowest floor there is"
        );
        assert_eq!(i.max_slice_bytes, 4096);
        assert_eq!(i.max_inline_hash_bytes, 4096, "equal is inside the bound");
        assert_eq!(i.recovery_budget, 1);
        assert_eq!(loaded.policy.doc.ttl_days, 1);
        assert_eq!(loaded.policy.doc.max_input_bytes, 1);
    }
}
