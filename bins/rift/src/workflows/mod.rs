//! The workflow verbs' shared plane: a report **window**, the facts a window
//! yields, and `rift dispatch`.
//!
//! # Deterministic-first, and Rift runs no model
//!
//! The whole point: these verbs compute *evidence*, and the user's own
//! agent does the reasoning. So there is no narrative here, no summarizer, and
//! no call to anything. Two invocations naming the same window over the same
//! projection produce byte-identical `--json` — which is an exit criterion, not
//! an aspiration, and is why every query in this module carries a total
//! `ORDER BY` and why the window is resolved from arguments rather than from a
//! clock.
//!
//! # The window and its facts live in `rift-store`
//!
//! [`Window`], [`WindowFacts`] and the query core that fills them moved to
//! [`rift_store::window`]: they are SQL over that crate's own
//! capture schema, and `rift-canopy-data`'s cards need the same arithmetic —
//! which they could not have had from here, because `bins/rift` is a binary and
//! nothing can link it. Read that module for the two rules a reader of these
//! verbs still has to know: **an omitted bound is unbounded, never "now"**, and
//! **a checkpoint whose shard records no time is placed by its sessions or
//! reported as unresolved** — the sentinel bug that once made this verb answer
//! `checkpoints: 0` on a repository holding 103.
//!
//! What stayed is everything a *surface* owes: the JSON, the two renderers,
//! `--branch` scoping (it walks git, not SQL), and **policy**.
//! [`rift_store::window::gather`] cannot say which touched paths are sensitive
//! — that answer needs `rift_config`, and a `rift-store -> rift-config` edge
//! would invert the layering — so it returns `sensitive_paths: None` and
//! [`FactsPolicy`] here fills it in at both call sites.
//!
//! # What may reach a report
//!
//! Nothing this module reads is transcript or tool-output bytes. It reads
//! counts, identifiers, timestamps and repository paths — and even a path is
//! run through [`crate::provenance::safe_path`] before it reaches human output,
//! because a repository filename is attacker-influenceable and a terminal
//! obeys the escape sequences printed into it. JSON output is safe by
//! construction (serde escapes control characters), but the human renderer is
//! not, and "we only print paths" is exactly the reasoning that ships an OSC
//! title-setter. `a_hostile_path_cannot_reach_the_rendered_report` pins it.
//!
//! # The modules
//!
//! [`branch`](mod@branch) resolves `--branch`, which is the one answer on the
//! substrate rather than in SQL; [`dispatch`](mod@dispatch) is the `rift
//! dispatch` verb and the three renderings of one fact set;
//! [`recap`](mod@recap) is `rift recap` and its own. What is left here is what
//! all three share: the planes a verb reads, the fact set it yields, and
//! policy — the half `rift-store` deliberately cannot answer.

mod branch;
mod dispatch;
mod recap;

use std::path::Path;

use rift_core::Result;
use rift_store::SqliteStore;
use serde_json::json;

pub(crate) use branch::branch_scope;
pub(crate) use dispatch::dispatch;
pub(crate) use recap::recap;

// ---------------------------------------------------------------------------
// The query core moved to `rift-store`. It is SQL over that
// crate's own capture schema and it has two readers — these verbs and
// `rift-canopy-data`'s cards — and `bins/rift` is a binary, so nothing can link
// it. `rift-store` was already an edge of both, so the move added no
// crate-graph edge. Re-exported under the names this module has always used, so
// every caller of `crate::workflows::Window` still resolves.
//
// What did NOT move is the point of the split: policy. `sensitive_touched`
// constructs a `rift_config::EffectiveConfig`, and moving it would have added
// `rift-store -> rift-config` — storage reading project policy, on the
// workspace's most-depended-on crate. So the filter stayed here, the moved
// struct's `sensitive_paths` is an `Option` nobody in `rift-store` fills, and
// [`FactsPolicy`] below is where the two halves meet.
// ---------------------------------------------------------------------------
pub(crate) use rift_store::window::{gather as gather_facts, CheckpointFact, Window, WindowFacts};

/// One touched file, as [`FactsPolicy::evidence`] numbers it.
///
/// The `path` is here because this type feeds the **human** surfaces too, which
/// are the only place the ordinal→path map renders. An
/// agent-facing consumer reads `ordinal` and never `path` — a distinction the
/// type cannot enforce, so it is stated at every one of the four call sites
/// instead of trusted to a comment here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileEvidence<'a> {
    /// The pack-local citation number, 1-based (`evidence:0` reads as "none").
    pub ordinal: u32,
    /// The repository path — human surfaces only.
    pub path: &'a str,
    /// Distinct checkpoints that touched it.
    pub checkpoints: i64,
    /// Whether this repository's policy calls the path sensitive.
    pub sensitive: bool,
}

/// **The policy half of a window's facts**, which is the half `rift-store`
/// deliberately cannot answer.
///
/// [`rift_store::window::gather`] returns `sensitive_paths: None` — *nobody
/// asked policy* — and every verb here asks immediately, through
/// [`FactsPolicy::apply_policy`]. The trait exists because the facts are a
/// foreign type now and an inherent method is not available; the shape earns
/// its keep anyway, because it collects the four operations that depend on
/// repository policy where a reviewer reads them as one unit.
pub(crate) trait FactsPolicy {
    /// Ask this repository's policy which touched paths are sensitive.
    fn apply_policy(&mut self, root: &Path);

    /// The policy answer, or an empty slice when nobody has asked.
    ///
    /// **`None` reads as "no path is sensitive", which is a fail-open**, and
    /// the only thing that makes it acceptable is that no surface here can
    /// reach it: [`gather`] applies policy before returning and
    /// [`FactsPolicy::restrict_to_branch`] re-applies it after narrowing.
    /// Two guards keep that true, one per call site, and they fail the moment
    /// a call site stops asking:
    /// `data_tests::a_bounded_window_reports_the_checkpoints_whose_time_is_derived`
    /// ends by asserting a `Some(..)` answer off [`gather`], and
    /// `narrowing_re_asks_policy_instead_of_carrying_the_old_answer` covers
    /// [`FactsPolicy::restrict_to_branch`].
    fn sensitive(&self) -> &[String];

    /// The evidence-ordinal pass — the ONE place a path becomes a number.
    fn evidence(&self) -> Vec<FileEvidence<'_>>;

    /// Narrow to `ids`, then re-ask policy about the narrowed set.
    fn restrict_to_branch(&mut self, ids: &std::collections::BTreeSet<String>, root: &Path);
}

impl FactsPolicy for WindowFacts {
    fn apply_policy(&mut self, root: &Path) {
        self.sensitive_paths = Some(sensitive_touched(root, &self.files));
    }

    fn sensitive(&self) -> &[String] {
        self.sensitive_paths.as_deref().unwrap_or(&[])
    }

    /// **The evidence-ordinal pass — the ONE place a path becomes a number**,
    /// and the one order every surface of these verbs walks.
    ///
    /// The fact set is `WindowFacts::files`, which the store's own aggregation
    /// has already sorted into a total order (touch count descending, then
    /// path), so an ordinal is a deterministic function of the window — not of
    /// when a report ran, and not of what survived a pack's budget. It is
    /// assigned here, before any pack is compiled, and carried into the entry's
    /// text; a pack that degrades drops entries, it never renumbers them.
    ///
    /// Every consumer reads THIS list rather than re-deriving `index + 1`:
    /// `facts_json`, the two human renderers, and both pack compilers. A rule
    /// spelled in four places is a rule that will be right in three of them —
    /// and here "right" means an agent-facing citation and a human-facing map
    /// agreeing about which file they mean.
    ///
    /// **No content address rides with an ordinal, and that is a fact about the
    /// data rather than a shortcut.** A window aggregates every revision of a
    /// path across every checkpoint in it, so no single `content_sha256`
    /// identifies "the file"; inventing one would mean picking a revision and
    /// presenting it as the artifact.
    fn evidence(&self) -> Vec<FileEvidence<'_>> {
        let sensitive = self.sensitive();
        self.files
            .iter()
            .enumerate()
            .map(|(i, (path, checkpoints))| FileEvidence {
                ordinal: u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1),
                path: path.as_str(),
                checkpoints: *checkpoints,
                sensitive: sensitive.iter().any(|s| s == path),
            })
            .collect()
    }

    fn restrict_to_branch(&mut self, ids: &std::collections::BTreeSet<String>, root: &Path) {
        self.restrict_to(ids);
        self.apply_policy(root);
    }
}

/// The two data planes a workflow verb reads, resolved once.
///
/// **A seam, and it exists because of what its absence cost.** Every query
/// below used to reach the recall ledger through the retired per-path
/// derivation directly, a machine-global path computed at the query — so no
/// test could drive this module against a store it seeded, and none did. Eleven queries over ten tables were executed by nothing, which
/// is how a verb that answered zero on a real repository passed 2,375 tests.
/// Resolving the path at the verb and passing it down is the whole difference.
pub(crate) struct Planes<'a> {
    /// The capture projection.
    pub capture: &'a SqliteStore,
    /// The worktree root — policy, and the recall path's derivation.
    pub root: &'a Path,
    /// Recall's ledger, when this repository has one. `None` is an absent
    /// fact, never a zero, and is **never created** by a read-only report.
    pub recall_db: Option<std::path::PathBuf>,
}

impl<'a> Planes<'a> {
    /// Resolve the planes for a repository — **both keyed by the family**, so
    /// every member reads the family's one store.
    ///
    /// The recall path was the retired per-PATH derivation, called here on a
    /// product read. From the main work tree that answers the family's own
    /// recall id (the family adopts main's), so nothing looked wrong;
    /// from a **linked member** it named a store nothing writes, and
    /// `dispatch`, `recap`, `review` and `investigate` reported
    /// `available: false` over a ledger that was there all along. That is the
    /// double identity the family model exists to remove, surviving in the one
    /// plane that was never routed through it.
    ///
    /// # The refusal is the family layer's own words, and it cannot be reached
    ///
    /// An unresolvable family is a typed refusal — outside a repository, no
    /// manifest with main unprovable, a manifest this reader cannot trust —
    /// propagated verbatim from [`crate::family::recall_db`], never a silently
    /// empty store. In the product it is **unreachable**, and that is a fact
    /// about the routing rather than a hope: all four verbs run under
    /// `projection_read`, which opens the CAPTURE projection through the same
    /// `resolve_at` first, so a family this call could fail on has already
    /// refused the verb with the same message. `a_verb_refuses_before_the
    /// _recall_plane_is_ever_resolved` pins that ordering.
    ///
    /// `None` keeps its old meaning and only its old meaning: the family HAS
    /// no ledger yet. That is an absent fact, never a zero, and this read-only
    /// path still never creates one.
    ///
    /// # Errors
    /// [`rift_core::RiftError`] from [`crate::family::recall_db`].
    pub(crate) fn open(capture: &'a SqliteStore, root: &'a Path) -> Result<Self> {
        let recall_db = crate::family::recall_db(root)?;
        Ok(Self {
            capture,
            root,
            recall_db: recall_db.exists().then_some(recall_db),
        })
    }
}

/// Recall's savings in the words both window verbs print: the avoided figure
/// labelled an estimated upper bound beside the injected spend, or — when the
/// repository has no recall ledger — a sentence saying so, never a zero.
///
/// One function for `dispatch` and `recap` because two spellings of one fact
/// drift, and the label on the bound is the part that must never be lost.
pub(super) fn recall_savings_text(facts: &WindowFacts) -> String {
    match facts.savings {
        Some(sv) => format!(
            "{} tokens avoided (estimated upper bound), {} injected",
            sv.avoided_upper_bound, sv.injected
        ),
        None => "no ledger in this repository".to_owned(),
    }
}

/// The window as the report echoes it back.
///
/// A free function rather than a method because the JSON stayed here: nothing
/// in `rift-store` formats a document. The echo reports **both** what the
/// caller typed and the bound it resolved to, which is why the moved type
/// exposes each half separately.
pub(crate) fn window_json(window: &Window) -> serde_json::Value {
    json!({
        "since": window.since_text(),
        "until": window.until_text(),
        "since_ms": window.since_ms(),
        "until_ms": window.until_ms(),
    })
}

/// Gather every fact `rift dispatch` reports, **then apply this repository's
/// policy**.
///
/// The two halves are separated by crate: [`rift_store::window::gather`] runs
/// the SQL and hands back `sensitive_paths: None`, and this wrapper is the
/// first of the two places (the other is
/// [`FactsPolicy::restrict_to_branch`]) where the answer is filled in. Every
/// verb here calls this rather than the store's function directly, which is
/// what makes "a fact set that reached a renderer has been through policy" true
/// by construction rather than by review.
///
/// # Errors
/// Propagates projection read errors. A missing optional source (recall's
/// ledger) is an absent fact, not an error.
pub(crate) async fn gather(planes: &Planes<'_>, window: &Window) -> Result<WindowFacts> {
    let mut facts = gather_facts(planes.capture, planes.recall_db.as_deref(), window).await?;
    facts.apply_policy(planes.root);
    Ok(facts)
}

/// The touched paths this repository's policy calls sensitive.
///
/// Policy failure is not silence: a repository whose config cannot be read
/// falls back to the secure default, whose built-in sensitive set is unioned in
/// and non-droppable, so the answer degrades toward flagging more rather than
/// less.
fn sensitive_touched(root: &Path, files: &[(String, i64)]) -> Vec<String> {
    let cfg = rift_config::EffectiveConfig::load(&root.join(".rift"))
        .or_else(|_| rift_config::EffectiveConfig::secure_default());
    let Ok(cfg) = cfg else { return Vec::new() };
    files
        .iter()
        .filter(|(p, _)| cfg.is_sensitive_path(p))
        .map(|(p, _)| p.clone())
        .collect()
}

// The rest of the fact types are named only by the fixtures in this crate's
// tests — production here reads them through `WindowFacts`'s fields. Re-exported
// under `cfg(test)` so an unused import cannot ride into a shipped build.
#[cfg(test)]
pub(crate) use rift_store::window::{AttributedFile, AttributionRollup, Savings, TokenClasses};

// The two verbs' inner seams and their contract versions, named by the data
// tests through this module's path. Production reaches each of them inside its
// own module, so these re-exports are test-only for the same reason as the
// block above: an unused import must not ride into a shipped build.
#[cfg(test)]
pub(crate) use dispatch::{dispatch_in, DISPATCH_SCHEMA_VERSION};
#[cfg(test)]
pub(crate) use recap::{recap_in, RECAP_SCHEMA_VERSION};

#[cfg(test)]
mod data_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// **The invariant the evidence walk rests on**, pinned rather than assumed:
    /// the policy pass derives `sensitive_paths` by FILTERING `files`, so every
    /// sensitive path has a position in `files` and therefore an ordinal.
    ///
    /// It matters because the pack compilers select their critical facts by
    /// walking [`FactsPolicy::evidence`] and keeping the sensitive ones. If a
    /// sensitive path could exist outside `files`, that walk would drop a
    /// CRITICAL fact silently — and "the pack got quieter" is the failure mode
    /// this program keeps finding. A hand-built fixture can still break the
    /// shape, which is why the fixtures in `evidence.rs` and `review.rs` were
    /// corrected to match production rather than the walk being taught to
    /// tolerate the difference.
    #[test]
    fn every_sensitive_path_is_one_of_the_touched_files_and_has_an_ordinal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut f = WindowFacts {
            attributed: vec![
                AttributedFile {
                    checkpoint_id: "c1".into(),
                    path: ".env".into(),
                    agent_added: 1,
                    human_added: 0,
                    human_modified_agent: 0,
                    agent_removed_by_human: 0,
                    confidence_milli: 900,
                },
                AttributedFile {
                    checkpoint_id: "c1".into(),
                    path: "src/a.rs".into(),
                    agent_added: 1,
                    human_added: 0,
                    human_modified_agent: 0,
                    agent_removed_by_human: 0,
                    confidence_milli: 900,
                },
                AttributedFile {
                    checkpoint_id: "c2".into(),
                    path: "src/a.rs".into(),
                    agent_added: 1,
                    human_added: 0,
                    human_modified_agent: 0,
                    agent_removed_by_human: 0,
                    confidence_milli: 900,
                },
            ],
            ..WindowFacts::default()
        };
        // The aggregation lives in `rift-store` now and the policy pass here,
        // so the fixture drives the seam the way production does.
        f.restrict_to_branch(
            &["c1".to_owned(), "c2".to_owned()].into_iter().collect(),
            dir.path(),
        );
        // `.env` is one of the built-in non-droppable sensitive paths, so this
        // holds under the secure default that an empty `.rift` falls back to.
        assert_eq!(f.sensitive_paths, Some(vec![".env".to_owned()]));
        let ev = f.evidence();
        // Touch count first, then path: `src/a.rs` (2) is 1, `.env` (1) is 2.
        assert_eq!(ev.len(), 2);
        assert_eq!(
            (ev[0].ordinal, ev[0].path, ev[0].sensitive),
            (1, "src/a.rs", false)
        );
        assert_eq!(
            (ev[1].ordinal, ev[1].path, ev[1].sensitive),
            (2, ".env", true)
        );
        for p in f.sensitive() {
            assert!(
                ev.iter().any(|e| e.path == p && e.sensitive),
                "a sensitive path with no ordinal would drop a CRITICAL fact: {p}"
            );
        }
    }

    /// **The `--branch` call site asks policy again**, and this is the only
    /// thing that makes it do so.
    ///
    /// `rift_store::window::WindowFacts::restrict_to` narrows the
    /// checkpoint-anchored facts and clears `sensitive_paths` to `None` —
    /// deliberately, because the old answer described a wider file set. If
    /// [`FactsPolicy::restrict_to_branch`] ever stops re-applying policy, the
    /// field stays `None`, `sensitive()` returns an empty slice, and **every
    /// sensitive flag on a `--branch`-scoped report silently disappears**: the
    /// pack gets quieter, which is this program's signature failure. Nothing
    /// else in the tree would notice.
    #[test]
    fn narrowing_re_asks_policy_instead_of_carrying_the_old_answer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut f = WindowFacts {
            attributed: vec![
                AttributedFile {
                    checkpoint_id: "c1".into(),
                    path: ".env".into(),
                    agent_added: 1,
                    human_added: 0,
                    human_modified_agent: 0,
                    agent_removed_by_human: 0,
                    confidence_milli: 900,
                },
                AttributedFile {
                    checkpoint_id: "c2".into(),
                    path: "src/only-on-the-other-branch.rs".into(),
                    agent_added: 1,
                    human_added: 0,
                    human_modified_agent: 0,
                    agent_removed_by_human: 0,
                    confidence_milli: 900,
                },
            ],
            ..WindowFacts::default()
        };
        f.restrict_to_branch(&["c1".to_owned()].into_iter().collect(), dir.path());

        assert_eq!(f.files, vec![(".env".to_owned(), 1)]);
        assert_eq!(
            f.sensitive_paths,
            Some(vec![".env".to_owned()]),
            "a narrowed report must carry a policy answer about the narrowed set"
        );
        assert!(
            f.evidence().iter().any(|e| e.sensitive),
            "and the evidence walk must still see it, or the pack loses a CRITICAL fact"
        );
    }
}
