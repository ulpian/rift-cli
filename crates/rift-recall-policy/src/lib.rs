//! Recall serving policy: per-turn budgets, sensitive-path gates, trust
//! enforcement, and audited exact retrieval.
//!
//! The serve gate is policy-first: the default response is a
//! manifest, not content; exact content requires a `reason`, passes policy, and
//! is audited. Budgets bound over-retrieval; sensitive paths force a reason;
//! untrusted content is never served raw. Every granted exact
//! retrieval yields an [`AuditDecision::Allow`] carrying the audit record the
//! caller must emit through `rift hooks memd`.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use std::path::{Path, PathBuf};

use rift_config::EffectiveConfig;
use rift_recall_types::{OmitReason, Trust};

/// Lexically normalize a repo-relative path: drop `.`/empty segments and resolve
/// `..` against earlier segments. Returns `None` if the path escapes the repo
/// root (a `..` with nothing to pop) — an escaping request is never served. This
/// is the traversal defense: a denied glob cannot be dodged by dressing the path
/// up (`foo/../secrets/key` → `secrets/key`).
#[must_use]
pub fn normalize_rel(path: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop()?; // pop earlier segment; nothing to pop → escapes root
            }
            s => out.push(s),
        }
    }
    Some(out.join("/"))
}

/// A composition downgrade for a sensitive path that cannot be served exactly
/// within budget: hand back a handle/summary rather than deny (losing the delta)
/// or truncate (leaking a partial secret).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Downgrade {
    /// Serve the retrieval handle instead of inlining the exact bytes.
    ExactToHandle,
    /// Serve the validity-checked summary instead of inlining the source.
    InlineToSummary,
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

// --- the three active-lever tables -------------------------------------------

/// How far a lever may act.
///
/// The variants are ordered by strength (`Off < Observe < Enforce`), which is
/// what lets a cap be a `min`: a family can always be held below what the
/// table asked for, and nothing can be lifted above it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// The lever is not consulted: nothing acts and nothing is recorded.
    Off,
    /// The lever reaches its decision and records it; the agent's call runs
    /// exactly as it was written.
    #[default]
    Observe,
    /// The lever acts on the call.
    Enforce,
}

impl Mode {
    /// The spelling the policy file uses and `rift recall status` prints.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Observe => "observe",
            Self::Enforce => "enforce",
        }
    }
}

/// Which document formats the converter may touch. One accepted value today —
/// a closed set rather than a free string, so a typo is a policy that fails to
/// load rather than a silently narrowed set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Formats {
    /// Every extension the converter declares.
    #[default]
    All,
}

impl Formats {
    /// The spelling the policy file uses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
        }
    }
}

/// How a repository prefers a document read to be answered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocChannel {
    /// Amend the read's path to the sidecar. Nothing is declined and the model
    /// is asked for nothing, so this is the primary channel where a family
    /// documents an amendment at all.
    #[default]
    Rewrite,
    /// Decline the read once, naming the sidecar in the reason. The fallback
    /// when an amendment cannot be relied on, and selectable outright by a
    /// repository that would rather be told than amended.
    Redirect,
}

impl DocChannel {
    /// The spelling the policy file uses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rewrite => "rewrite",
            Self::Redirect => "redirect",
        }
    }
}

/// The slice a single decision may cover: 96 KiB, the measured point at which
/// a read result truncates in place (25,000 tokens, never written to a file),
/// so a decision never covers more than the context was going to keep. The
/// roughly 30,000 characters past which a *command's* output stops being held
/// inline bounds a different channel, and a shell read records an unbounded
/// range regardless.
pub const DEFAULT_MAX_SLICE_BYTES: u64 = 96 * 1024;
/// The shortest slice a decision may act on, in lines. A decline replaces the
/// range with a reason frame, and below some length the frame costs more than
/// the read it replaces: **111 tokens of frame against about 4 tokens a line,
/// so a decline starts paying at 28 lines**, and the floor sits exactly there.
/// It is provisional — the observe window prices the pool it forgoes and
/// confirms or moves it. A slice under the floor is still decided and recorded;
/// it is only never acted on.
///
/// It was 24 against a frame the prose called "about 88 tokens". Two things
/// moved it. The frame grew the provenance handle that tells an agent where a
/// change's story is kept; and the 88 was measured with an unusually
/// cheap-to-tokenize hash — on the renderer's own golden fixture the same frame
/// already cost 98, so 24 had drifted a line under its break-even before this.
/// The number is now **measured** rather than quoted, by
/// `the_reason_frame_pays_for_itself_at_the_floor` in `rift-recall-serve` (the
/// one crate that has both the sealed renderer and the real tokenizer), and a
/// reword that grows the frame reds that test. A floor left behind a grown
/// frame is a lever quietly declining the reads it makes more expensive.
pub const DEFAULT_MIN_SLICE_LINES: u32 = 28;
/// The largest file hashed inline on the gate path; past it the read proceeds.
pub const DEFAULT_MAX_INLINE_HASH_BYTES: u64 = 1024 * 1024;
/// Tokens a recovery map may cost after a compaction.
pub const DEFAULT_RECOVERY_BUDGET: u32 = 800;
/// How long a read-time basis snapshot is kept before `rift clean` collects
/// it. The same thirty days a converted document gets, and for the same
/// reason: losing one costs a delta frame and never an answer — the read that
/// produced it can always be made again, and the revision is either still in
/// the checkout or is a blob git holds.
pub const DEFAULT_BASIS_TTL_DAYS: u32 = 30;
/// The head limit added to a content search that carries none.
pub const DEFAULT_GREP_HEAD_LIMIT: u32 = 40;
/// How many ranked paths an answered listing carries.
pub const DEFAULT_GLOB_TOP_N: u32 = 8;
/// Tokens an answered listing may cost.
pub const DEFAULT_ANSWER_BUDGET_TOKENS: u32 = 160;
/// Rows kept per sheet when a table is converted.
pub const DEFAULT_TABLE_ROWS: u32 = 200;
/// The largest document converted at all.
pub const DEFAULT_MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;
/// Below this many characters per page a PDF is treated as having no text
/// layer, and the read is left alone.
pub const DEFAULT_PDF_MIN_CHARS_PER_PAGE: u32 = 200;
/// The deadline a conversion on the gate path is given before the read
/// proceeds and the work is handed to the detached worker.
pub const DEFAULT_CONVERT_BUDGET_MS: u32 = 250;
/// How long an unused sidecar is kept.
pub const DEFAULT_DOC_TTL_DAYS: u32 = 30;

/// Per-family interception overrides. The three families are named fields
/// rather than a map so an unknown family is a policy that fails to load
/// rather than a line that silently does nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AgentModes {
    /// Claude Code's override.
    #[serde(rename = "claude-code")]
    pub claude_code: Option<Mode>,
    /// Codex's override.
    pub codex: Option<Mode>,
    /// Antigravity's override.
    pub antigravity: Option<Mode>,
}

impl AgentModes {
    /// The override written for `family`, if any.
    #[must_use]
    pub fn for_family(&self, family: &str) -> Option<Mode> {
        match family {
            "claude-code" => self.claude_code,
            "codex" => self.codex,
            "antigravity" => self.antigravity,
            _ => None,
        }
    }
}

/// `[interception]`: the read-interception lever and its bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct InterceptionPolicy {
    /// The repository-wide mode, before any per-family override or cap.
    pub mode: Mode,
    /// The largest slice one decision may cover.
    pub max_slice_bytes: u64,
    /// The shortest slice one decision may act on, in lines.
    pub min_slice_lines: u32,
    /// The largest file hashed inline on the gate path.
    pub max_inline_hash_bytes: u64,
    /// Tokens a recovery map may cost.
    pub recovery_budget: u32,
    /// How long a read-time basis snapshot is kept.
    pub basis_ttl_days: u32,
    /// `[interception.agents]`.
    pub agents: AgentModes,
}

impl Default for InterceptionPolicy {
    fn default() -> Self {
        Self {
            mode: Mode::Enforce,
            max_slice_bytes: DEFAULT_MAX_SLICE_BYTES,
            min_slice_lines: DEFAULT_MIN_SLICE_LINES,
            max_inline_hash_bytes: DEFAULT_MAX_INLINE_HASH_BYTES,
            recovery_budget: DEFAULT_RECOVERY_BUDGET,
            basis_ttl_days: DEFAULT_BASIS_TTL_DAYS,
            agents: AgentModes::default(),
        }
    }
}

/// `[search]`: the search levers and their bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SearchPolicy {
    /// The repository-wide mode, before any cap.
    pub mode: Mode,
    /// The head limit added to a content search that carries none.
    pub grep_head_limit: u32,
    /// How many ranked paths an answered listing carries.
    pub glob_top_n: u32,
    /// Tokens an answered listing may cost.
    pub answer_budget_tokens: u32,
}

impl Default for SearchPolicy {
    fn default() -> Self {
        Self {
            mode: Mode::Enforce,
            grep_head_limit: DEFAULT_GREP_HEAD_LIMIT,
            glob_top_n: DEFAULT_GLOB_TOP_N,
            answer_budget_tokens: DEFAULT_ANSWER_BUDGET_TOKENS,
        }
    }
}

/// `[doc]`: the document levers and their bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DocPolicy {
    /// The repository-wide mode, before any cap.
    pub mode: Mode,
    /// Which formats may be converted.
    pub formats: Formats,
    /// Which channel this repository prefers for a document read.
    pub channel: DocChannel,
    /// Rows kept per sheet.
    pub table_rows: u32,
    /// The largest document converted at all.
    pub max_input_bytes: u64,
    /// The characters-per-page floor below which a PDF is left alone.
    pub pdf_min_chars_per_page: u32,
    /// The gate-path conversion deadline.
    pub convert_budget_ms: u32,
    /// How long an unused sidecar is kept.
    pub ttl_days: u32,
}

impl Default for DocPolicy {
    fn default() -> Self {
        Self {
            mode: Mode::Enforce,
            formats: Formats::All,
            channel: DocChannel::Rewrite,
            table_rows: DEFAULT_TABLE_ROWS,
            max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
            pdf_min_chars_per_page: DEFAULT_PDF_MIN_CHARS_PER_PAGE,
            convert_budget_ms: DEFAULT_CONVERT_BUDGET_MS,
            ttl_days: DEFAULT_DOC_TTL_DAYS,
        }
    }
}

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
    /// Revision required by [`patch_injection_turn_budget`].
    pub revision: ServePolicyRevision,
}

/// A safe serve-policy write refusal.
#[derive(Debug, thiserror::Error)]
pub enum ServePolicyEditError {
    /// The path is a symlink/special file.
    #[error("serve policy is refused; repair .rift/policies/serve.toml and retry")]
    Refused,
    /// The existing policy is invalid.
    #[error("serve policy is invalid; repair .rift/policies/serve.toml and retry")]
    Invalid,
    /// Source changed since the view was loaded.
    #[error("serve policy changed on disk; refresh Project Settings and retry")]
    SourceConflict,
    /// Filesystem failure.
    #[error("{path}: {source}")]
    Io {
        /// Repository-owned path involved.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
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

/// Patch only the binary Project Settings injection control. `0` is off and
/// `400` is on; callers should pass [`DEFAULT_INJECTION_TURN_BUDGET`] for on.
/// Existing custom positive budgets remain untouched until the operator flips
/// the binary control.
///
/// # Errors
/// Refuses links/special files, invalid sources, revision conflicts, and I/O.
pub fn patch_injection_turn_budget(
    rift_dir: &Path,
    expected: &ServePolicyRevision,
    budget: u32,
) -> Result<ServePolicySnapshot, ServePolicyEditError> {
    edit_serve_policy(rift_dir, expected, |doc| {
        if let Some(current) = doc
            .get_mut("injection_turn_budget")
            .and_then(toml_edit::Item::as_value_mut)
        {
            let decor = current.decor().clone();
            *current = toml_edit::Value::from(i64::from(budget));
            *current.decor_mut() = decor;
        } else {
            doc.insert("injection_turn_budget", toml_edit::value(i64::from(budget)));
        }
    })
}

/// The lever tables Project Settings switches as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeverTable {
    /// `[search]`: bounding and answering searches.
    Search,
    /// `[doc]`: reading documents as Markdown.
    Doc,
}

impl LeverTable {
    /// The table's name in the policy file.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Doc => "doc",
        }
    }
}

/// Patch only a lever table's `mode`, the binary Project Settings control
/// over it: `enforce` is on (the default the table ships with) and `off` is
/// off. Every other key in the table — the bounds, the channel, the caps —
/// stays exactly as written, an absent table is created carrying only its
/// `mode`, and an explicit `observe` an operator declared stays until the
/// operator flips the control.
///
/// # Errors
/// Refuses links/special files, invalid sources, revision conflicts, and I/O.
pub fn patch_lever_mode(
    rift_dir: &Path,
    expected: &ServePolicyRevision,
    table: LeverTable,
    mode: Mode,
) -> Result<ServePolicySnapshot, ServePolicyEditError> {
    edit_serve_policy(rift_dir, expected, |doc| {
        let Some(entry) = doc
            .entry(table.as_str())
            .or_insert_with(toml_edit::table)
            .as_table_like_mut()
        else {
            // A `[search]`/`[doc]` that is not a table did not load as a
            // policy in the first place, and the prospective parse below
            // refuses the edit rather than this arm inventing one.
            return;
        };
        if let Some(current) = entry
            .get_mut("mode")
            .and_then(toml_edit::Item::as_value_mut)
        {
            let decor = current.decor().clone();
            *current = toml_edit::Value::from(mode.as_str());
            *current.decor_mut() = decor;
        } else {
            entry.insert("mode", toml_edit::value(mode.as_str()));
        }
    })
}

/// The one way the serving policy is edited in place: load, refuse what is
/// not an owned valid file, check the revision the caller read, apply `edit`
/// to the parsed document, validate the prospective text as a policy, and
/// replace the file atomically. Every patch above is one closure over this.
fn edit_serve_policy(
    rift_dir: &Path,
    expected: &ServePolicyRevision,
    edit: impl FnOnce(&mut toml_edit::DocumentMut),
) -> Result<ServePolicySnapshot, ServePolicyEditError> {
    let path = rift_dir.join("policies").join("serve.toml");
    let current = load_serve_policy(rift_dir);
    match current.source {
        ServePolicySource::Refused => return Err(ServePolicyEditError::Refused),
        ServePolicySource::Invalid => return Err(ServePolicyEditError::Invalid),
        ServePolicySource::Declared | ServePolicySource::Default => {}
    }
    if &current.revision != expected {
        return Err(ServePolicyEditError::SourceConflict);
    }
    let text = match owned_policy_bytes(rift_dir, &path) {
        OwnedPolicy::Bytes(bytes) => {
            String::from_utf8(bytes).map_err(|_| ServePolicyEditError::Invalid)?
        }
        OwnedPolicy::Absent => String::new(),
        OwnedPolicy::Refused => return Err(ServePolicyEditError::Refused),
    };
    let mut doc = if text.trim().is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        text.parse::<toml_edit::DocumentMut>()
            .map_err(|_| ServePolicyEditError::Invalid)?
    };
    edit(&mut doc);
    let prospective = doc.to_string();
    parse_policy(&prospective).ok_or(ServePolicyEditError::Invalid)?;
    atomic_policy_replace(&path, prospective.as_bytes())?;
    Ok(load_serve_policy(rift_dir))
}

enum OwnedPolicy {
    Absent,
    Refused,
    Bytes(Vec<u8>),
}

fn owned_policy_bytes(rift_dir: &Path, path: &Path) -> OwnedPolicy {
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
fn parse_policy(text: &str) -> Option<ServePolicy> {
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

// --- resolving the tables against one family ---------------------------------

/// What the pinned capability table says this family lets Rift write back.
///
/// Three booleans rather than the table itself, so this crate stays a leaf: the
/// caller reads `rift_agent::respond::CAPABILITIES` — the one place a channel
/// is asserted from a committed pin — and hands the answer down. A family Rift
/// knows nothing about arrives here as [`FamilyCapability::default`], which is
/// "no channel at all", and every lever resolves to `off`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FamilyCapability {
    /// A pinned row declines a call with a reason **and** a recorded run shows
    /// that reason reaching the model. Documentation alone is not this flag.
    pub confirmed_reason_channel: bool,
    /// The family accepts a read's path amended to a Rift-owned sidecar.
    pub document_sidecar_rewrite: bool,
    /// The family accepts a content search given a head limit it lacked.
    pub grep_head_limit_rewrite: bool,
    /// A pinned artifact quotes this family's edit result telling the model
    /// that the file it just wrote is current in its context.
    ///
    /// Without it, the read an agent makes to check its own edit is the only
    /// sight of that edit it has, so nothing is ever declined on the strength
    /// of an edit line. The flag is a fact about the family's tooling, not
    /// about its hook API, and it caps no mode: it reaches the decision
    /// itself, which is the only place that knows a decline would have rested
    /// on an edit.
    pub pinned_edit_echo: bool,
}

/// Why a lever resolved below what the tables asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapReason {
    /// No recorded run shows this family's model receiving a decline's reason,
    /// so a decline here would be indistinguishable from a tool failure — which
    /// a model is right to retry.
    UnconfirmedReasonChannel,
    /// The family documents no amendment of a tool call, so there is nothing
    /// for this lever to do.
    NoAmendmentChannel,
}

impl CapReason {
    /// A short phrase for `rift recall status` and `rift doctor`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnconfirmedReasonChannel => "reason channel not confirmed by a recorded run",
            Self::NoAmendmentChannel => "no native document/search amendment channel",
        }
    }
}

/// One family's resolved levers: what each one may actually do, with the
/// numbers it reads. Every field is a decision already taken — the hook path
/// reads this and nothing else, so the rules live here rather than at each
/// call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Effective {
    /// The policy on disk could not be read, so every lever below is `off`.
    pub fail_closed: bool,

    /// Whether a decline may rest on the window's own edit at all, carried
    /// through from [`FamilyCapability::pinned_edit_echo`]. Not a mode and not
    /// capped by one: `enforce` on a family without it still declines the
    /// re-reads a *read* covers, and never the ones only an edit covers.
    pub pinned_edit_echo: bool,

    /// Declining an unchanged re-read.
    pub read_deny: Mode,
    /// What the tables asked for before the cap.
    pub read_deny_requested: Mode,
    /// Why `read_deny` sits below `read_deny_requested`, when it does.
    pub read_deny_cap: Option<CapReason>,
    /// The largest slice one decision may cover.
    pub max_slice_bytes: u64,
    /// The shortest slice one decision may act on, in lines. A shorter one is
    /// still decided and recorded, and never declined.
    pub min_slice_lines: u32,
    /// The largest file hashed inline on the gate path.
    pub max_inline_hash_bytes: u64,
    /// Tokens a recovery map may cost.
    pub recovery_budget: u32,

    /// Adding a head limit to a content search that carries none.
    pub grep_cap: Mode,
    /// Why `grep_cap` is `off` while `[search]` is not.
    pub grep_cap_off: Option<CapReason>,
    /// Answering a listing with the ranked top-N instead of running it.
    pub search_answer: Mode,
    /// Why `search_answer` sits below `[search] mode`, when it does.
    pub search_answer_cap: Option<CapReason>,
    /// The head limit added.
    pub grep_head_limit: u32,
    /// How many ranked paths an answer carries.
    pub glob_top_n: u32,
    /// Tokens an answer may cost.
    pub answer_budget_tokens: u32,

    /// Amending a document read's path to the sidecar.
    pub doc_rewrite: Mode,
    /// Why `doc_rewrite` is `off` while `[doc]` is not.
    pub doc_rewrite_off: Option<CapReason>,
    /// Declining a document read once with the sidecar named in the reason:
    /// the primary channel under `channel = "redirect"`, and under
    /// `channel = "rewrite"` the fallback a verified dropped amendment arms.
    pub doc_redirect: Mode,
    /// Why `doc_redirect` sits below `[doc] mode`, when it does.
    pub doc_redirect_cap: Option<CapReason>,
    /// Which channel this repository chose.
    pub doc_channel: DocChannel,
    /// Which formats may be converted.
    pub doc_formats: Formats,
    /// Rows kept per sheet.
    pub table_rows: u32,
    /// The largest document converted at all.
    pub max_input_bytes: u64,
    /// The characters-per-page floor below which a PDF is left alone.
    pub pdf_min_chars_per_page: u32,
    /// The gate-path conversion deadline.
    pub convert_budget_ms: u32,
    /// How long an unused sidecar is kept.
    pub ttl_days: u32,
}

/// Resolve the three tables against one family and what that family's pinned
/// rows allow.
///
/// Pure and allocation-free: the caller loads [`ServePolicy`] **from the file**
/// — `PreToolUse` opens no store — and calls this. Three rules do all the work.
///
/// 1. **A cap is a `min`.** A per-family override, the pinned capability and
///    the measured caveats can only hold a lever below what `[…] mode` asked
///    for. Nothing here lifts anything.
/// 2. **Anything that declines a call needs a confirmed reason channel**, and
///    ships at `observe` until its own window says otherwise (GOAL §2.10). A
///    repository that writes `enforce` gets it, capped by rule 1.
/// 3. **The two amendments are the exception the owner decided** (O14, O16):
///    they decline nothing, add nothing to the model's context and ask the
///    model for nothing, so on a family whose rows document the amendment they
///    act whenever their table is not `off`. `off` is still off.
#[must_use]
pub fn resolve(policy: &ServePolicy, family: &str, capability: FamilyCapability) -> Effective {
    let interception = &policy.interception;
    let search = &policy.search;
    let doc = &policy.doc;

    let override_mode = interception.agents.for_family(family);
    let requested = override_mode.unwrap_or(interception.mode);
    let (read_deny, read_deny_cap) = cap_decline(requested, capability);
    let (search_answer, search_answer_cap) = cap_decline(search.mode, capability);
    let (doc_redirect, doc_redirect_cap) = cap_decline(doc.mode, capability);

    // Rule 3. `channel = "redirect"` is a repository saying it would rather be
    // told than amended, so the amendment stands down and the decline above is
    // the primary channel.
    let (doc_rewrite, doc_rewrite_off) =
        if doc.mode == Mode::Off || doc.channel == DocChannel::Redirect {
            // A shut table and a repository that asked to be told instead are
            // both "no amendment wanted", which is not the same as a family
            // that cannot take one — hence no reason to report.
            (Mode::Off, None)
        } else if capability.document_sidecar_rewrite {
            // The table's own mode: an amendment acts in `enforce` and is
            // recorded without acting in `observe`, like every other lever.
            (doc.mode, None)
        } else {
            (Mode::Off, Some(CapReason::NoAmendmentChannel))
        };
    let (grep_cap, grep_cap_off) = if search.mode == Mode::Off {
        (Mode::Off, None)
    } else if capability.grep_head_limit_rewrite {
        (search.mode, None)
    } else {
        (Mode::Off, Some(CapReason::NoAmendmentChannel))
    };

    Effective {
        fail_closed: policy.fail_closed,
        pinned_edit_echo: capability.pinned_edit_echo,
        read_deny,
        read_deny_requested: requested,
        read_deny_cap,
        max_slice_bytes: interception.max_slice_bytes,
        min_slice_lines: interception.min_slice_lines,
        max_inline_hash_bytes: interception.max_inline_hash_bytes,
        recovery_budget: interception.recovery_budget,
        grep_cap,
        grep_cap_off,
        search_answer,
        search_answer_cap,
        grep_head_limit: search.grep_head_limit,
        glob_top_n: search.glob_top_n,
        answer_budget_tokens: search.answer_budget_tokens,
        doc_rewrite,
        doc_rewrite_off,
        doc_redirect,
        doc_redirect_cap,
        doc_channel: doc.channel,
        doc_formats: doc.formats,
        table_rows: doc.table_rows,
        max_input_bytes: doc.max_input_bytes,
        pdf_min_chars_per_page: doc.pdf_min_chars_per_page,
        convert_budget_ms: doc.convert_budget_ms,
        ttl_days: doc.ttl_days,
    }
}

/// Rules 1 and 2 for one lever that declines a call.
fn cap_decline(requested: Mode, capability: FamilyCapability) -> (Mode, Option<CapReason>) {
    if requested != Mode::Enforce {
        // Nothing to cap: `off` and `observe` decline nothing either way.
        return (requested, None);
    }
    if !capability.confirmed_reason_channel {
        return (Mode::Observe, Some(CapReason::UnconfirmedReasonChannel));
    }
    (Mode::Enforce, None)
}

fn atomic_policy_replace(path: &Path, bytes: &[u8]) -> Result<(), ServePolicyEditError> {
    let Some(parent) = path.parent() else {
        return Err(ServePolicyEditError::Invalid);
    };
    std::fs::create_dir_all(parent).map_err(|source| ServePolicyEditError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let tmp = parent.join(format!(".serve.toml.rift-tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|source| ServePolicyEditError::Io {
        path: tmp.clone(),
        source,
    })?;
    #[cfg(windows)]
    if path.exists() {
        std::fs::remove_file(path).map_err(|source| ServePolicyEditError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }
    std::fs::rename(&tmp, path).map_err(|source| ServePolicyEditError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// The serve-policy semantics version ([`PolicyEngine::version`]): part of the
/// pack identity, so any semantic policy change invalidates pack keys.
pub const SERVE_POLICY_VERSION: u32 = 1;

/// Default per-turn tool-call ceiling.
pub const DEFAULT_MAX_TOOL_CALLS: u32 = 4;
/// Default per-turn returned-token ceiling.
pub const DEFAULT_MAX_TOKENS_PER_TURN: u32 = 2000;

/// A per-turn budget bounding over-retrieval.
#[derive(Clone, Copy, Debug)]
pub struct TurnBudget {
    max_tool_calls: u32,
    max_tokens: u32,
    calls_used: u32,
    tokens_used: u32,
}

impl Default for TurnBudget {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_TOOL_CALLS, DEFAULT_MAX_TOKENS_PER_TURN)
    }
}

impl TurnBudget {
    /// A budget with explicit ceilings.
    #[must_use]
    pub fn new(max_tool_calls: u32, max_tokens: u32) -> Self {
        Self {
            max_tool_calls,
            max_tokens,
            calls_used: 0,
            tokens_used: 0,
        }
    }

    /// Try to account for one tool call. Returns `false` if the ceiling is hit.
    #[must_use]
    pub fn try_call(&mut self) -> bool {
        if self.calls_used >= self.max_tool_calls {
            return false;
        }
        self.calls_used = self.calls_used.saturating_add(1);
        true
    }

    /// Try to account for `tokens` returned. Returns `false` if it would exceed
    /// the ceiling (and accounts nothing). Uses saturating arithmetic so a
    /// hostile/huge `tokens` can never wrap the counter under the ceiling
    /// and slip through as if it fit.
    #[must_use]
    pub fn try_tokens(&mut self, tokens: u32) -> bool {
        if self.tokens_used.saturating_add(tokens) > self.max_tokens {
            return false;
        }
        self.tokens_used = self.tokens_used.saturating_add(tokens);
        true
    }

    /// Remaining tokens this turn.
    #[must_use]
    pub fn tokens_remaining(&self) -> u32 {
        self.max_tokens.saturating_sub(self.tokens_used)
    }

    /// Remaining tool calls this turn.
    #[must_use]
    pub fn calls_remaining(&self) -> u32 {
        self.max_tool_calls.saturating_sub(self.calls_used)
    }

    /// Anchor a new turn: zero the call/token draw-down while keeping
    /// the ceilings. A `rift_known_state` call resets the budget so
    /// every subsequent call in the turn draws from a fresh allowance.
    pub fn anchor(&mut self) {
        self.calls_used = 0;
        self.tokens_used = 0;
    }
}

/// The decision for an exact-retrieval request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuditDecision {
    /// Allowed — emit this audit record (`memd.exact_retrieved`).
    Allow {
        /// The path retrieved.
        path: String,
        /// The supplied reason.
        reason: String,
        /// Whether the path was sensitive.
        sensitive: bool,
    },
    /// Denied — surface this omit reason; no content is served.
    Deny(OmitReason),
}

/// How aggressively the pack compiler may compress a path's content. Sensitive
/// paths force [`Optimization::Conservative`] — safety overrides cost: a
/// sensitive file is served whole or by handle, never as a lossy summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Optimization {
    /// Normal compaction (summaries/truncation allowed).
    Normal,
    /// Conservative: no lossy summary substitution for this path.
    Conservative,
}

/// Errors loading a non-empty reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("an exact-retrieval reason must be non-empty")]
pub struct EmptyReason;

/// A reason for exact retrieval that is **type-guaranteed** non-empty: the
/// only constructor rejects blank/whitespace, so "no reason" is unrepresentable
/// at the `rift_retrieve_exact` boundary and the audit event always carries one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NonEmptyReason(String);

impl NonEmptyReason {
    /// Construct from a string, rejecting blank/whitespace.
    ///
    /// # Errors
    /// [`EmptyReason`] if `s` is empty or only whitespace.
    pub fn new(s: impl Into<String>) -> Result<Self, EmptyReason> {
        let s = s.into();
        if s.trim().is_empty() {
            return Err(EmptyReason);
        }
        Ok(Self(s))
    }

    /// The reason text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The serving policy engine, parameterized by the effective project config plus
/// the recall-serving policy (denied paths + raw-transcript allow-list). The
/// extra lists default empty — **fail-closed**: no denied paths beyond the
/// config's sensitive handling, and raw transcripts disabled.
pub struct PolicyEngine<'a> {
    config: &'a EffectiveConfig,
    denied_paths: Vec<String>,
    raw_transcript_globs: Vec<String>,
    fail_closed: bool,
}

impl<'a> PolicyEngine<'a> {
    /// Wrap an effective config (no extra denied paths; transcripts disabled).
    #[must_use]
    pub fn new(config: &'a EffectiveConfig) -> Self {
        Self {
            config,
            denied_paths: Vec::new(),
            raw_transcript_globs: Vec::new(),
            fail_closed: false,
        }
    }

    /// Apply a loaded [`ServePolicy`] overlay (`.rift/policies/serve.toml`): its
    /// denied globs, transcript allow-list, and fail-closed flag.
    #[must_use]
    pub fn with_serve_policy(mut self, sp: ServePolicy) -> Self {
        self.denied_paths = sp.denied_paths;
        self.raw_transcript_globs = sp.raw_transcript_globs;
        self.fail_closed = sp.fail_closed;
        self
    }

    /// Add path globs that are **never** served (not even by handle).
    #[must_use]
    pub fn with_denied_paths(mut self, globs: Vec<String>) -> Self {
        self.denied_paths = globs;
        self
    }

    /// Add path globs for which raw transcript retrieval is explicitly enabled
    /// (still trust-checked + audited). Empty = transcripts off (the default).
    #[must_use]
    pub fn with_raw_transcript_globs(mut self, globs: Vec<String>) -> Self {
        self.raw_transcript_globs = globs;
        self
    }

    /// Whether the engine is fail-closed (an unparseable policy denies all exact).
    #[must_use]
    pub fn is_fail_closed(&self) -> bool {
        self.fail_closed
    }

    /// The policy semantics version (an input to a pack's cache key).
    /// Bumped whenever a change to serve-policy semantics could alter what a
    /// pack may contain, so a policy change can never re-serve a stale cache.
    #[must_use]
    pub fn version(&self) -> u32 {
        SERVE_POLICY_VERSION
    }

    /// Compose a [`Downgrade`] for a sensitive path whose exact bytes will not fit
    /// the remaining budget: hand back a handle rather than deny or truncate.
    /// `None` means serve exact as usual (not sensitive, or it fits).
    #[must_use]
    pub fn downgrade_for(&self, path: &str, exact_fits_budget: bool) -> Option<Downgrade> {
        if !exact_fits_budget && self.exact_source_required(path) {
            Some(Downgrade::ExactToHandle)
        } else {
            None
        }
    }

    /// Whether a model is allowed (deny wins; empty allow = allow-all).
    #[must_use]
    pub fn model_allowed(&self, model: &str) -> bool {
        self.config.is_model_allowed(model)
    }

    /// Whether a path is on the deny-list (never served, not even by handle). The
    /// path is canonicalized first, so a traversal-dressed request cannot dodge a
    /// denied glob; a path that escapes the repo root is treated as denied.
    #[must_use]
    pub fn is_denied_path(&self, path: &str) -> bool {
        match normalize_rel(path) {
            Some(p) => self
                .denied_paths
                .iter()
                .any(|g| rift_config::glob_match(g, &p)),
            None => true, // an escaping path (`../…`) is never served
        }
    }

    /// Whether raw transcript retrieval is enabled for `path` (canonicalized).
    #[must_use]
    pub fn raw_transcript_allowed(&self, path: &str) -> bool {
        let Some(p) = normalize_rel(path) else {
            return false;
        };
        self.raw_transcript_globs
            .iter()
            .any(|g| rift_config::glob_match(g, &p))
    }

    /// The compaction policy for a path (Conservative for sensitive paths).
    #[must_use]
    pub fn optimization_for(&self, path: &str) -> Optimization {
        if self.is_sensitive(path) {
            Optimization::Conservative
        } else {
            Optimization::Normal
        }
    }

    /// Whether a path's content must be served *exact-or-not-at-all* (no lossy
    /// summary substitution). True for sensitive paths.
    #[must_use]
    pub fn exact_source_required(&self, path: &str) -> bool {
        self.is_sensitive(path)
    }

    /// Whether a path is sensitive, after canonicalizing traversal away. A path
    /// that escapes the repo root is treated as sensitive (fail closed).
    fn is_sensitive(&self, path: &str) -> bool {
        match normalize_rel(path) {
            Some(p) => self.config.is_sensitive_path(&p),
            None => true,
        }
    }

    /// Decide an exact-retrieval request. Precedence is most-restrictive first:
    /// denied path → tool-output-untrusted (never raw) → transcript-while-disabled
    /// → reason mandatory (sensitive paths surface a specific reason) → allow. A
    /// grant returns the audit record to emit.
    #[must_use]
    pub fn decide_exact(&self, path: &str, trust: Trust, reason: Option<&str>) -> AuditDecision {
        // 0. fail closed: an unparseable serve policy denies all exact retrieval.
        if self.fail_closed {
            return AuditDecision::Deny(OmitReason::PolicyDeniedExact);
        }
        // 1. denied path: nothing is served, not even a handle. Canonicalized, so
        //    a traversal-dressed path cannot reach a denied target.
        if self.is_denied_path(path) {
            return AuditDecision::Deny(OmitReason::PolicyDeniedExact);
        }
        // 2. arbitrary tool output is never served as raw exact text.
        if matches!(trust, Trust::ToolOutputUntrusted) {
            return AuditDecision::Deny(OmitReason::PolicyDeniedExact);
        }
        // 3. transcripts are off unless explicitly enabled for this path.
        if matches!(trust, Trust::TranscriptUntrusted) && !self.raw_transcript_allowed(path) {
            return AuditDecision::Deny(OmitReason::PolicyDeniedExact);
        }
        // 4. reason is mandatory; a sensitive path surfaces a specific reason.
        let sensitive = self.is_sensitive(path);
        match reason {
            None | Some("") => {
                if sensitive {
                    AuditDecision::Deny(OmitReason::SensitivePathRequiresReason)
                } else {
                    AuditDecision::Deny(OmitReason::PolicyDeniedExact)
                }
            }
            Some(r) => AuditDecision::Allow {
                path: path.to_string(),
                reason: r.to_string(),
                sensitive,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> EffectiveConfig {
        // sensitive default includes **/.env*, **/*secret* etc.
        EffectiveConfig::secure_default().unwrap()
    }

    #[test]
    fn budget_caps_calls_and_tokens() {
        let mut b = TurnBudget::new(2, 100);
        assert!(b.try_call());
        assert!(b.try_call());
        assert!(!b.try_call(), "third call exceeds ceiling");
        assert!(b.try_tokens(60));
        assert!(!b.try_tokens(60), "would exceed token ceiling");
        assert!(b.try_tokens(40));
        assert_eq!(b.tokens_remaining(), 0);
    }

    #[test]
    fn token_counter_never_overflows_under_the_ceiling() {
        // A hostile huge `tokens` must be refused, not wrap the counter below the
        // ceiling and slip through (the saturating-arithmetic fix).
        let mut b = TurnBudget::new(4, 100);
        assert!(b.try_tokens(50));
        assert!(
            !b.try_tokens(u32::MAX),
            "u32::MAX must not wrap under ceiling"
        );
        assert_eq!(
            b.tokens_remaining(),
            50,
            "the refused charge accounts nothing"
        );
    }

    #[test]
    fn anchor_resets_draw_down_but_keeps_ceilings() {
        let mut b = TurnBudget::new(2, 100);
        assert!(b.try_call());
        assert!(b.try_tokens(90));
        assert_eq!(b.calls_remaining(), 1);
        assert_eq!(b.tokens_remaining(), 10);
        b.anchor(); // a known_state call anchors a fresh turn
        assert_eq!(
            b.calls_remaining(),
            2,
            "ceiling preserved, draw-down zeroed"
        );
        assert_eq!(b.tokens_remaining(), 100);
    }

    #[test]
    fn exact_requires_reason() {
        let cfg = config();
        let p = PolicyEngine::new(&cfg);
        assert_eq!(
            p.decide_exact("src/main.rs", Trust::CodeFact, None),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact)
        );
        assert!(matches!(
            p.decide_exact("src/main.rs", Trust::CodeFact, Some("debugging the crash")),
            AuditDecision::Allow { .. }
        ));
    }

    #[test]
    fn sensitive_path_requires_reason_specifically() {
        let cfg = config();
        let p = PolicyEngine::new(&cfg);
        assert_eq!(
            p.decide_exact("config/.env", Trust::CodeFact, None),
            AuditDecision::Deny(OmitReason::SensitivePathRequiresReason)
        );
        // with a reason, allowed but flagged sensitive (audited)
        match p.decide_exact(
            "config/.env",
            Trust::CodeFact,
            Some("inspecting prod config"),
        ) {
            AuditDecision::Allow { sensitive, .. } => assert!(sensitive),
            other => panic!("expected allow, got {other:?}"),
        }
    }

    #[test]
    fn untrusted_content_is_never_exact() {
        let cfg = config();
        let p = PolicyEngine::new(&cfg);
        assert_eq!(
            p.decide_exact("tool.out", Trust::ToolOutputUntrusted, Some("any reason")),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact)
        );
        assert_eq!(
            p.decide_exact("chat", Trust::TranscriptUntrusted, Some("any reason")),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact)
        );
    }

    #[test]
    fn model_allow_deny_passthrough() {
        let cfg = config();
        let p = PolicyEngine::new(&cfg);
        // secure default has empty allow → allow-all
        assert!(p.model_allowed("claude-opus-4-8"));
    }

    #[test]
    fn denied_path_is_refused_even_with_reason() {
        let cfg = config();
        let p = PolicyEngine::new(&cfg).with_denied_paths(vec!["**/vault/**".to_string()]);
        assert!(p.is_denied_path("app/vault/key"));
        assert_eq!(
            p.decide_exact("app/vault/key", Trust::CodeFact, Some("debugging")),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact),
            "a denied path is refused even with a reason"
        );
    }

    #[test]
    fn transcript_off_by_default_on_unless_enabled() {
        let cfg = config();
        let off = PolicyEngine::new(&cfg);
        assert_eq!(
            off.decide_exact("log/x", Trust::TranscriptUntrusted, Some("audit")),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact),
            "transcripts are off by default"
        );
        let on = PolicyEngine::new(&cfg).with_raw_transcript_globs(vec!["log/**".to_string()]);
        assert!(matches!(
            on.decide_exact("log/x", Trust::TranscriptUntrusted, Some("audit")),
            AuditDecision::Allow { .. }
        ));
    }

    #[test]
    fn sensitive_paths_force_conservative_and_exact_source() {
        let cfg = config();
        let p = PolicyEngine::new(&cfg);
        assert_eq!(
            p.optimization_for("config/.env"),
            Optimization::Conservative
        );
        assert!(p.exact_source_required("config/.env"));
        assert_eq!(p.optimization_for("src/main.rs"), Optimization::Normal);
        assert!(!p.exact_source_required("src/main.rs"));
    }

    #[test]
    fn non_empty_reason_rejects_blank() {
        assert_eq!(NonEmptyReason::new("   ").unwrap_err(), EmptyReason);
        assert_eq!(
            NonEmptyReason::new("debugging").unwrap().as_str(),
            "debugging"
        );
    }

    #[test]
    fn normalize_resolves_dots_and_rejects_escapes() {
        assert_eq!(normalize_rel("a/./b").as_deref(), Some("a/b"));
        assert_eq!(normalize_rel("a/b/../c").as_deref(), Some("a/c"));
        assert_eq!(
            normalize_rel("foo/../secrets/key").as_deref(),
            Some("secrets/key")
        );
        assert_eq!(normalize_rel("../etc/passwd"), None, "escapes root");
        assert_eq!(normalize_rel("a/../../x"), None, "escapes root");
    }

    #[test]
    fn serve_policy_loads_denied_and_transcript_globs() {
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        std::fs::write(
            pol.join("serve.toml"),
            "denied_paths = [\"**/vault/**\"]\nraw_transcript_globs = [\"logs/**\"]\n",
        )
        .unwrap();
        let sp = ServePolicy::load(dir.path());
        assert!(!sp.fail_closed);
        assert_eq!(sp.denied_paths, vec!["**/vault/**".to_string()]);
        assert_eq!(sp.raw_transcript_globs, vec!["logs/**".to_string()]);

        let cfg = config();
        let p = PolicyEngine::new(&cfg).with_serve_policy(sp);
        assert!(p.is_denied_path("app/vault/key"));
        assert!(p.raw_transcript_allowed("logs/session.jsonl"));
    }

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

    #[test]
    fn binary_injection_patch_preserves_comments_and_resets_to_400() {
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        let path = pol.join("serve.toml");
        std::fs::write(
            &path,
            "# keep\ndenied_paths = [\"private/**\"]\ninjection_turn_budget = 777 # custom\n",
        )
        .unwrap();
        let loaded = load_serve_policy(dir.path());
        assert_eq!(loaded.policy.injection_turn_budget, 777);
        let off = patch_injection_turn_budget(dir.path(), &loaded.revision, 0).unwrap();
        assert_eq!(off.policy.injection_turn_budget, 0);
        let on =
            patch_injection_turn_budget(dir.path(), &off.revision, DEFAULT_INJECTION_TURN_BUDGET)
                .unwrap();
        assert_eq!(on.policy.injection_turn_budget, 400);
        let source = std::fs::read_to_string(path).unwrap();
        assert!(source.starts_with("# keep\n"));
        assert!(source.contains("denied_paths = [\"private/**\"]"));
        assert!(source.contains("injection_turn_budget = 400 # custom"));
    }

    #[test]
    fn injection_patch_refuses_a_stale_revision() {
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        let path = pol.join("serve.toml");
        std::fs::write(&path, "injection_turn_budget = 400\n").unwrap();
        let loaded = load_serve_policy(dir.path());
        std::fs::write(&path, "injection_turn_budget = 12\n").unwrap();
        let error = patch_injection_turn_budget(dir.path(), &loaded.revision, 0).unwrap_err();
        assert!(matches!(error, ServePolicyEditError::SourceConflict));
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "injection_turn_budget = 12\n"
        );
    }

    /// The binary control over a lever table touches its `mode` and nothing
    /// else: the bounds beside it and the comments around it survive, an
    /// absent table is created with only its `mode`, and switching back on
    /// writes the default the table ships with.
    #[test]
    fn lever_mode_patch_creates_an_absent_table_and_keeps_the_rest_of_a_present_one() {
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        let path = pol.join("serve.toml");
        std::fs::write(
            &path,
            "# keep\ninjection_turn_budget = 400\n\n[search]\nmode = \"enforce\" # lever\n\
             grep_head_limit = 12\n",
        )
        .unwrap();
        let loaded = load_serve_policy(dir.path());
        assert_eq!(loaded.policy.search.grep_head_limit, 12);

        let search_off =
            patch_lever_mode(dir.path(), &loaded.revision, LeverTable::Search, Mode::Off).unwrap();
        assert_eq!(search_off.policy.search.mode, Mode::Off);
        assert_eq!(search_off.policy.search.grep_head_limit, 12);
        assert_eq!(
            search_off.policy.doc.mode,
            Mode::Enforce,
            "the other table is untouched"
        );

        let doc_off =
            patch_lever_mode(dir.path(), &search_off.revision, LeverTable::Doc, Mode::Off).unwrap();
        assert_eq!(doc_off.policy.doc.mode, Mode::Off);
        assert_eq!(doc_off.policy.doc.table_rows, DEFAULT_TABLE_ROWS);
        assert_eq!(doc_off.policy.injection_turn_budget, 400);

        let doc_on = patch_lever_mode(
            dir.path(),
            &doc_off.revision,
            LeverTable::Doc,
            Mode::Enforce,
        )
        .unwrap();
        assert_eq!(doc_on.policy.doc.mode, Mode::Enforce);

        let source = std::fs::read_to_string(path).unwrap();
        assert!(source.starts_with("# keep\n"), "{source}");
        assert!(source.contains("mode = \"off\" # lever"), "{source}");
        assert!(source.contains("grep_head_limit = 12"), "{source}");
        assert!(source.contains("[doc]\nmode = \"enforce\""), "{source}");
    }

    /// `off` written by the control is `off` for every family: the levers a
    /// repository switched off resolve to nothing, whatever the family's
    /// channels could carry.
    #[test]
    fn a_lever_switched_off_resolves_off_for_every_family() {
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        let loaded = load_serve_policy(dir.path());
        let snapshot =
            patch_lever_mode(dir.path(), &loaded.revision, LeverTable::Doc, Mode::Off).unwrap();
        let snapshot = patch_lever_mode(
            dir.path(),
            &snapshot.revision,
            LeverTable::Search,
            Mode::Off,
        )
        .unwrap();
        let everything = FamilyCapability {
            confirmed_reason_channel: true,
            document_sidecar_rewrite: true,
            grep_head_limit_rewrite: true,
            ..FamilyCapability::default()
        };
        let effective = resolve(&snapshot.policy, "claude-code", everything);
        assert_eq!(effective.doc_rewrite, Mode::Off);
        assert_eq!(effective.doc_redirect, Mode::Off);
        assert_eq!(effective.search_answer, Mode::Off);
        assert_eq!(effective.grep_cap, Mode::Off);
    }

    #[test]
    fn lever_mode_patch_refuses_a_stale_revision() {
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        let path = pol.join("serve.toml");
        std::fs::write(&path, "[doc]\nmode = \"enforce\"\n").unwrap();
        let loaded = load_serve_policy(dir.path());
        std::fs::write(&path, "[doc]\nmode = \"observe\"\n").unwrap();
        let error =
            patch_lever_mode(dir.path(), &loaded.revision, LeverTable::Doc, Mode::Off).unwrap_err();
        assert!(matches!(error, ServePolicyEditError::SourceConflict));
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "[doc]\nmode = \"observe\"\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_serve_policy_is_refused() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        let target = dir.path().join("outside.toml");
        std::fs::write(&target, "injection_turn_budget = 9\n").unwrap();
        symlink(&target, pol.join("serve.toml")).unwrap();
        let loaded = load_serve_policy(dir.path());
        assert_eq!(loaded.source, ServePolicySource::Refused);
        assert!(loaded.policy.fail_closed);
        let error = patch_injection_turn_budget(dir.path(), &loaded.revision, 0).unwrap_err();
        assert!(matches!(error, ServePolicyEditError::Refused));
        assert_eq!(
            std::fs::read_to_string(target).unwrap(),
            "injection_turn_budget = 9\n"
        );
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

    #[test]
    fn unparseable_policy_fails_closed_and_denies_all_exact() {
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        // an unknown field / broken TOML → fail closed
        std::fs::write(pol.join("serve.toml"), "denied_paths = not-a-list").unwrap();
        let sp = ServePolicy::load(dir.path());
        assert!(sp.fail_closed);

        let cfg = config();
        let p = PolicyEngine::new(&cfg).with_serve_policy(sp);
        assert!(p.is_fail_closed());
        // even a trusted path WITH a reason is denied under an unparseable policy
        assert_eq!(
            p.decide_exact("src/main.rs", Trust::CodeFact, Some("debugging")),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact)
        );
    }

    #[test]
    fn traversal_dressed_path_cannot_dodge_a_denied_glob() {
        let cfg = config();
        let p = PolicyEngine::new(&cfg).with_denied_paths(vec!["secrets/**".to_string()]);
        // a dressed-up path canonicalizes to the denied target
        assert!(p.is_denied_path("app/../secrets/key"));
        assert_eq!(
            p.decide_exact("app/../secrets/key", Trust::CodeFact, Some("r")),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact),
            "a denied path cannot be reached by dressing it up"
        );
        // an escaping path is never served
        assert_eq!(
            p.decide_exact("../../etc/passwd", Trust::CodeFact, Some("r")),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact)
        );
    }

    #[test]
    fn sensitive_over_budget_downgrades_to_handle() {
        let cfg = config();
        let p = PolicyEngine::new(&cfg);
        // a sensitive path whose exact bytes will not fit → downgrade to a handle
        assert_eq!(
            p.downgrade_for("config/.env", false),
            Some(Downgrade::ExactToHandle)
        );
        // fits budget → serve exact as usual (no downgrade)
        assert_eq!(p.downgrade_for("config/.env", true), None);
        // not sensitive → no downgrade regardless of budget
        assert_eq!(p.downgrade_for("src/main.rs", false), None);
    }

    // --- the three active-lever tables ---------------------------------------

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
    /// Codex as its pinned rows stand: a documented decline no run has
    /// confirmed, and no amendment at all.
    const NO_CHANNEL: FamilyCapability = FamilyCapability {
        confirmed_reason_channel: false,
        document_sidecar_rewrite: false,
        grep_head_limit_rewrite: false,
        pinned_edit_echo: false,
    };
    /// Antigravity as its pinned rows stand: confirmed, amends nothing, and
    /// its page says nothing about what its edit tools return.
    const CONFIRMED_ONLY: FamilyCapability = FamilyCapability {
        confirmed_reason_channel: true,
        document_sidecar_rewrite: false,
        grep_head_limit_rewrite: false,
        pinned_edit_echo: false,
    };

    fn every_lever(e: &Effective) -> [Mode; 5] {
        [
            e.read_deny,
            e.grep_cap,
            e.search_answer,
            e.doc_rewrite,
            e.doc_redirect,
        ]
    }

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

    /// The edit-echo fact travels from the pinned capability to the resolved
    /// levers untouched, and it caps nothing on the way: it is not a mode, and
    /// a family without it still enforces everything a *read* covers. What it
    /// governs is one arm of the decision, which is where the fact belongs.
    #[test]
    fn the_edit_echo_fact_is_carried_through_and_caps_no_lever() {
        let (loaded, _d) = load_src(
            "[interception]\nmode = \"enforce\"\n\n[interception.agents]\nantigravity = \"enforce\"\n",
        );
        assert_eq!(loaded.source, ServePolicySource::Declared);

        let pinned = resolve(&loaded.policy, "claude-code", FULL);
        assert!(pinned.pinned_edit_echo);
        assert_eq!(pinned.read_deny, Mode::Enforce);

        let unpinned = resolve(&loaded.policy, "antigravity", CONFIRMED_ONLY);
        assert!(
            !unpinned.pinned_edit_echo,
            "nothing pinned says what this family's edit tools return"
        );
        assert_eq!(
            unpinned.read_deny,
            Mode::Enforce,
            "a named family still enforces the re-reads a read covers"
        );
        assert_eq!(unpinned.read_deny_cap, None);
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

    /// The binary injection control cannot smuggle a policy past the range
    /// check: an unreadable file is repaired, never patched.
    #[test]
    fn the_injection_patch_refuses_a_policy_outside_its_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        let path = pol.join("serve.toml");
        std::fs::write(&path, "[doc]\nttl_days = 0\n").unwrap();
        let loaded = load_serve_policy(dir.path());
        assert_eq!(loaded.source, ServePolicySource::Invalid);
        let error = patch_injection_turn_budget(dir.path(), &loaded.revision, 0).unwrap_err();
        assert!(matches!(error, ServePolicyEditError::Invalid));
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "[doc]\nttl_days = 0\n",
            "a refused patch writes nothing"
        );
    }

    /// The spellings the file and the status line share, and the strength
    /// order a cap relies on.
    #[test]
    fn the_spellings_and_the_strength_order_are_the_ones_the_file_uses() {
        assert_eq!(Mode::Off.as_str(), "off");
        assert_eq!(Mode::Observe.as_str(), "observe");
        assert_eq!(Mode::Enforce.as_str(), "enforce");
        assert!(Mode::Off < Mode::Observe && Mode::Observe < Mode::Enforce);
        assert_eq!(Mode::default(), Mode::Observe);
        assert_eq!(Formats::All.as_str(), "all");
        assert_eq!(Formats::default(), Formats::All);
        assert_eq!(DocChannel::Rewrite.as_str(), "rewrite");
        assert_eq!(DocChannel::Redirect.as_str(), "redirect");
        assert_eq!(DocChannel::default(), DocChannel::Rewrite);
        for reason in [
            CapReason::UnconfirmedReasonChannel,
            CapReason::NoAmendmentChannel,
        ] {
            assert!(!reason.as_str().is_empty());
        }
    }
    /// **Nothing acts under a policy Rift could not read.** An unknown key, a
    /// value the tables do not accept, a family nobody pinned, a number
    /// outside its range and TOML that does not parse are one state with one
    /// answer: every active lever `off`, on every family, whatever that
    /// family's rows would have allowed.
    #[test]
    fn an_unreadable_policy_turns_every_active_lever_off() {
        for src in [
            "[interception]\nmode = \"enforce\"\nunknown_key = 1\n",
            "[interception]\nmode = \"aggressive\"\n",
            "[interception.agents]\ncursor = \"enforce\"\n",
            "[interception]\nmax_slice_bytes = 2097152\n", // exceeds the hash bound
            "[interception]\nmin_slice_lines = 0\n",       // a floor of zero is no floor
            "[interception]\nmin_slice_lines = -1\n",      // and a negative one is not a count
            "[interception]\nrecovery_budget = 0\n",
            "[search]\ngrep_head_limit = 0\n",
            "[search]\nglob_top_n = 0\n",
            "[search]\nanswer_budget_tokens = 0\n",
            "[doc]\nformats = \"office\"\n",
            "[doc]\nchannel = \"ignore\"\n",
            "[doc]\ntable_rows = 0\n",
            "[doc]\nmax_input_bytes = 0\n",
            "[doc]\npdf_min_chars_per_page = 0\n",
            "[doc]\nconvert_budget_ms = 0\n",
            "[doc]\nttl_days = 0\n",
            "[doc\nmode = \"enforce\"\n",
        ] {
            let (loaded, _d) = load_src(src);
            assert_eq!(
                loaded.source,
                ServePolicySource::Invalid,
                "expected {src:?} to be unreadable"
            );
            assert!(loaded.policy.fail_closed);
            for capability in [FULL, CONFIRMED_ONLY, NO_CHANNEL] {
                for family in ["claude-code", "codex", "antigravity"] {
                    let e = resolve(&loaded.policy, family, capability);
                    assert_eq!(
                        every_lever(&e),
                        [Mode::Off; 5],
                        "{src:?} left a lever live for {family}"
                    );
                    assert!(e.fail_closed);
                }
            }
        }
    }

    /// A policy file is a request, not a grant. Whatever `[interception]` and
    /// `[interception.agents]` ask for, a family reaches `enforce` only where
    /// a recorded run shows its model receiving the reason — otherwise the
    /// decision is still reached and recorded, and the call runs.
    #[test]
    fn a_per_family_override_cannot_exceed_the_pinned_capability() {
        let (loaded, _d) = load_src(
            "[interception]\nmode = \"enforce\"\n\n[interception.agents]\ncodex = \"enforce\"\nclaude-code = \"enforce\"\n",
        );
        let policy = &loaded.policy;

        let confirmed = resolve(policy, "claude-code", FULL);
        assert_eq!(confirmed.read_deny, Mode::Enforce);
        assert_eq!(confirmed.read_deny_cap, None);

        let unconfirmed = resolve(policy, "codex", NO_CHANNEL);
        assert_eq!(unconfirmed.read_deny_requested, Mode::Enforce);
        assert_eq!(
            unconfirmed.read_deny,
            Mode::Observe,
            "asked for, not granted"
        );
        assert_eq!(
            unconfirmed.read_deny_cap,
            Some(CapReason::UnconfirmedReasonChannel)
        );
        // The same family, the same file, with the channel confirmed: the
        // override is the only thing that was ever in question.
        assert_eq!(
            resolve(policy, "codex", CONFIRMED_ONLY).read_deny,
            Mode::Enforce
        );

        // A cap only ever holds a lever down. An override below the table's
        // mode is honoured as written, and lowering it needs no capability.
        let (lowered, _d) = load_src(
            "[interception]\nmode = \"enforce\"\n\n[interception.agents]\nclaude-code = \"off\"\n",
        );
        let e = resolve(&lowered.policy, "claude-code", FULL);
        assert_eq!(e.read_deny, Mode::Off);
        assert_eq!(e.read_deny_cap, None);
        let unnamed = resolve(&lowered.policy, "codex", FULL);
        assert_eq!(
            unnamed.read_deny_requested,
            Mode::Enforce,
            "an override names one family and speaks for no other: the table's \
             own mode is what the unnamed family is still asked for"
        );
        assert_eq!(
            unnamed.read_deny,
            Mode::Enforce,
            "retry telemetry never changes the requested mode"
        );
    }

    /// The shipped defaults actively use every pinned native capability.
    #[test]
    fn the_defaults_enforce_every_confirmed_lever_and_act_on_the_two_amendments() {
        let policy = ServePolicy::default();
        let e = resolve(&policy, "claude-code", FULL);
        assert_eq!(e.read_deny, Mode::Enforce);
        assert_eq!(e.search_answer, Mode::Enforce);
        assert_eq!(e.doc_redirect, Mode::Enforce);
        assert_eq!(e.grep_cap, Mode::Enforce);
        assert_eq!(e.doc_rewrite, Mode::Enforce);
        assert_eq!(e.doc_channel, DocChannel::Rewrite);
        assert_eq!(e.doc_formats, Formats::All);
        assert_eq!(e.grep_head_limit, 40);
        assert_eq!(e.glob_top_n, 8);
        assert_eq!(e.answer_budget_tokens, 160);
        assert_eq!(e.recovery_budget, 800);
        assert_eq!(e.table_rows, 200);
        assert_eq!(e.pdf_min_chars_per_page, 200);
        assert_eq!(e.convert_budget_ms, 250);
        assert_eq!(e.ttl_days, 30);
        assert!(!e.fail_closed);
    }
    /// ★ Retry-after-deny is telemetry, never a mode cap. All three supported
    /// families receive the same repository-wide enforcement request, while an
    /// existing explicit `observe` or `off` remains authoritative.
    #[test]
    fn retry_telemetry_never_demotes_a_supported_family() {
        let (wide, _d) = load_src("[interception]\nmode = \"enforce\"\n");
        for family in ["claude-code", "codex", "antigravity"] {
            let e = resolve(&wide.policy, family, CONFIRMED_ONLY);
            assert_eq!(e.read_deny_requested, Mode::Enforce, "{family} was asked");
            assert_eq!(e.read_deny, Mode::Enforce, "{family} was demoted");
            assert_eq!(e.read_deny_cap, None);

            let (observed, _d) = load_src(&format!(
                "[interception]\nmode = \"enforce\"\n\n[interception.agents]\n{family} = \"observe\"\n"
            ));
            let e = resolve(&observed.policy, family, CONFIRMED_ONLY);
            assert_eq!(e.read_deny, Mode::Observe, "{family} observe was ignored");
            assert_eq!(e.read_deny_cap, None);

            let (off, _d) = load_src(&format!(
                "[interception]\nmode = \"enforce\"\n\n[interception.agents]\n{family} = \"off\"\n"
            ));
            assert_eq!(
                resolve(&off.policy, family, CONFIRMED_ONLY).read_deny,
                Mode::Off
            );

            // Enforcement still requires the pinned reason channel.
            assert_eq!(
                resolve(&wide.policy, family, NO_CHANNEL).read_deny_cap,
                Some(CapReason::UnconfirmedReasonChannel)
            );
        }
    }

    /// An amendment reaches a family that documents one and no other; a family
    /// that amends nothing is told so rather than left looking merely idle.
    #[test]
    fn the_two_amendments_act_only_where_a_family_documents_them() {
        let policy = ServePolicy::default();
        let e = resolve(&policy, "codex", NO_CHANNEL);
        assert_eq!(e.grep_cap, Mode::Off);
        assert_eq!(e.grep_cap_off, Some(CapReason::NoAmendmentChannel));
        assert_eq!(e.doc_rewrite, Mode::Off);
        assert_eq!(e.doc_rewrite_off, Some(CapReason::NoAmendmentChannel));

        // `off` is off: an amendment is not exempt from the table being shut.
        let (shut, _d) = load_src("[search]\nmode = \"off\"\n\n[doc]\nmode = \"off\"\n");
        let e = resolve(&shut.policy, "claude-code", FULL);
        assert_eq!(
            every_lever(&e),
            [Mode::Enforce, Mode::Off, Mode::Off, Mode::Off, Mode::Off]
        );
        assert_eq!(e.grep_cap_off, None, "shut is not a missing channel");
        assert_eq!(e.doc_rewrite_off, None);
    }

    /// A repository that chooses `redirect` is choosing to be told rather than
    /// amended: the amendment stands down, and the decline it prefers is still
    /// held to the same evidence as every other decline.
    #[test]
    fn the_redirect_channel_stands_the_amendment_down() {
        let (redirect, _d) = load_src("[doc]\nmode = \"enforce\"\nchannel = \"redirect\"\n");
        let e = resolve(&redirect.policy, "claude-code", FULL);
        assert_eq!(e.doc_channel, DocChannel::Redirect);
        assert_eq!(e.doc_rewrite, Mode::Off);
        assert_eq!(e.doc_rewrite_off, None);
        assert_eq!(e.doc_redirect, Mode::Enforce);

        let e = resolve(&redirect.policy, "codex", NO_CHANNEL);
        assert_eq!(e.doc_redirect, Mode::Observe);
        assert_eq!(
            e.doc_redirect_cap,
            Some(CapReason::UnconfirmedReasonChannel)
        );

        // Under `rewrite` the decline is the fallback, not gone: a dropped
        // amendment has somewhere to fall back to, at the mode its table set.
        let (rewrite, _d) = load_src("[doc]\nmode = \"enforce\"\n");
        let e = resolve(&rewrite.policy, "claude-code", FULL);
        assert_eq!(e.doc_rewrite, Mode::Enforce);
        assert_eq!(e.doc_redirect, Mode::Enforce);
    }

    /// An amendment is a lever like any other: `observe` records it without
    /// acting. A repository that writes `observe` under `[doc]` or `[search]`
    /// must not find its reads amended anyway.
    #[test]
    fn observe_reaches_the_two_amendments_without_acting() {
        let (doc, _d) = load_src("[doc]\nmode = \"observe\"\n");
        let e = resolve(&doc.policy, "claude-code", FULL);
        assert_eq!(e.doc_rewrite, Mode::Observe);
        assert_eq!(e.doc_rewrite_off, None);
        let (search, _d) = load_src("[search]\nmode = \"observe\"\n");
        let e = resolve(&search.policy, "claude-code", FULL);
        assert_eq!(e.grep_cap, Mode::Observe);
        assert_eq!(e.grep_cap_off, None);
        // A family without the channel still cannot be amended in any mode.
        let e = resolve(&search.policy, "codex", NO_CHANNEL);
        assert_eq!(e.grep_cap, Mode::Off);
        assert_eq!(e.grep_cap_off, Some(CapReason::NoAmendmentChannel));
    }

    /// A family Rift has pinned nothing for is not a family it guesses about:
    /// its overrides are not its own, and it gets no lever.
    #[test]
    fn a_family_with_no_pinned_rows_gets_no_lever() {
        let (loaded, _d) = load_src(
            "[interception]\nmode = \"enforce\"\n\n[search]\nmode = \"enforce\"\n\n[doc]\nmode = \"enforce\"\n",
        );
        let e = resolve(&loaded.policy, "some-editor", FamilyCapability::default());
        assert_eq!(
            every_lever(&e),
            [
                Mode::Observe,
                Mode::Off,
                Mode::Observe,
                Mode::Off,
                Mode::Observe
            ],
            "it records; it never acts"
        );
        assert_eq!(
            loaded.policy.interception.agents.for_family("some-editor"),
            None
        );
    }

    /// A policy Rift refuses to read at all — a link where the file should be
    /// — is the unreadable case too, not a permissive one.
    #[cfg(unix)]
    #[test]
    fn a_refused_policy_turns_every_active_lever_off_as_well() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        let target = dir.path().join("outside.toml");
        std::fs::write(&target, "[interception]\nmode = \"enforce\"\n").unwrap();
        symlink(&target, pol.join("serve.toml")).unwrap();
        let loaded = load_serve_policy(dir.path());
        assert_eq!(loaded.source, ServePolicySource::Refused);
        let e = resolve(&loaded.policy, "claude-code", FULL);
        assert_eq!(every_lever(&e), [Mode::Off; 5]);
    }
}
