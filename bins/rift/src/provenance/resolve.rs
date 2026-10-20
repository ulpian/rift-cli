//! Resolving a span against the projection: which checkpoint explains it,
//! what it recorded, and what the answer cost in reads.
//!
//! Separate from the rendering because the caching rules — one message read
//! per commit, one facts read per checkpoint, one counts read per
//! (checkpoint, path) — are the whole reason this verb is affordable, and they
//! are a property of this module rather than of any document it feeds.

use std::collections::HashMap;

use rift_core::Result;
use rift_shadow::attribution::provenance::{
    parse_checkpoint_claims, select_for_file, CheckpointCandidate, ClaimSet, Selection,
};
use rift_store::{CanonicalStore, SqliteStore, Value};

use super::blame::{is_uncommitted, Span};
use super::safe_text::Label;
use super::worktree::checkpoint_worktree;

/// Per-file authorship counts recorded for one checkpoint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ::serde::Serialize)]
pub(super) struct AttrCounts {
    /// Lines the agent added.
    pub(super) agent_added: i64,
    /// Lines the human added.
    pub(super) human_added: i64,
    /// Agent-authored lines a human later edited.
    pub(super) human_modified_agent: i64,
    /// Calibrated confidence in milli-units (0..=1000).
    pub(super) confidence_milli: i64,
}

/// What a span's authorship can honestly be called.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ::serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Tag {
    /// Only agent-added lines were recorded for this file at this checkpoint.
    Ai,
    /// Only human-added lines were recorded.
    Human,
    /// Both were recorded, or a human edited agent lines. The counts are
    /// **per file**, so this cut cannot say which of the two wrote a *single*
    /// line — saying `ai` here would be a claim the data does not support.
    Mixed,
    /// No attribution row, or no checkpoint resolved at all.
    Unknown,
}

impl AttrCounts {
    /// Two rows for one path in one checkpoint: the counts add, and the
    /// weakest confidence is the honest one to report.
    pub(super) fn merge(self, c: Self) -> Self {
        Self {
            agent_added: self.agent_added.saturating_add(c.agent_added),
            human_added: self.human_added.saturating_add(c.human_added),
            human_modified_agent: self
                .human_modified_agent
                .saturating_add(c.human_modified_agent),
            confidence_milli: self.confidence_milli.min(c.confidence_milli),
        }
    }
}

impl Tag {
    /// The tag implied by a checkpoint's per-file counts.
    pub(super) fn of(counts: Option<&AttrCounts>) -> Self {
        let Some(c) = counts else {
            return Self::Unknown;
        };
        match (
            c.agent_added > 0,
            c.human_added > 0 || c.human_modified_agent > 0,
        ) {
            (true, true) => Self::Mixed,
            (true, false) => Self::Ai,
            (false, true) => Self::Human,
            (false, false) => Self::Unknown,
        }
    }

    /// The rendered word.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Ai => "ai",
            Self::Human => "human",
            Self::Mixed => "mixed",
            Self::Unknown => "unknown",
        }
    }
}

/// One session that contributed to a checkpoint.
#[derive(Clone, Debug, PartialEq, Eq, ::serde::Serialize)]
pub(super) struct SessionRef {
    /// The session id. Unlike the two below it is **not** rendered to a
    /// terminal: only `--json` carries it, and there nothing it contains can
    /// change the document's shape (see
    /// `the_json_document_cannot_be_given_a_shape_the_renderer_did_not_build`,
    /// which also names what does survive raw — this field is not claimed to be
    /// control-free). So it is reported exactly as recorded rather than folded
    /// into the label charset, which would mangle the ids of adapters that do
    /// not use ULIDs.
    pub(super) session_id: String,
    /// The agent that ran it.
    pub(super) agent: Label,
    /// Its headline model.
    pub(super) model: Label,
}

/// Everything the projection knows about one claimed checkpoint. Memoised by
/// id: a squash claims a handful, a file has thousands of lines.
#[derive(Clone, Debug, Default)]
pub(super) struct CheckpointFacts {
    /// The capture event's recorded time from `checkpoint.created_at`.
    /// `None` for BOTH honest absences — the id is claimed but absent
    /// locally, and the row is present but carries the unknown-time sentinel
    /// (`-1`, every native checkpoint archived before the writer fix).
    /// Folding the sentinel in here is what keeps `Some(-1)` from ranking as
    /// "resolved metadata" in the squash tie-break and from rendering as if a
    /// real instant in the `--json` documents. Row presence is not lost:
    /// a sentinel-dated row still loads its file evidence and sessions below,
    /// so the two absences differ by their `files_touched`/`sessions`, which
    /// is the half selection actually ranks on first.
    pub(super) created_at_ms: Option<i64>,
    /// Every path this checkpoint recorded touching.
    pub(super) files_touched: Vec<String>,
    /// Its sessions, in `session_in_checkpoint.ordinal` order.
    pub(super) sessions: Vec<SessionRef>,
    /// The checkpoint's strategy token as the projection recorded it
    /// (`manual-commit` | `on-pr` | `scheduled` | `policy-driven`, or the
    /// `import` marker `rift import` stamps on a read-only imported
    /// transcript). `None` when the row is absent locally.
    /// Imports landed before 2026-08-29 carry `manual-commit` forever: the
    /// table is append-only, so the documents say what the row says.
    pub(super) strategy: Option<String>,
    /// The member the shard was **archived in**, from `checkpoint_worktree`
    /// `None` is UNKNOWN and stays unknown: every shard written before worktree
    /// provenance existed carries no `worktree` member and so projects no row,
    /// back-filling one is forbidden, and defaulting to `main` would turn
    /// "we do not know" into a claim about where the archive ran. Rendered
    /// through [`super::worktree::worktree_label`], never read raw by a surface.
    pub(super) worktree: Option<Label>,
}

/// A span with its answer, ready to render.
#[derive(Clone, Debug)]
pub(super) struct Resolved {
    /// The span this answers.
    pub(super) span: Span,
    /// The chosen checkpoint and why, `None` when the commit claims nothing.
    pub(super) selection: Option<Selection>,
    /// Per-file counts for the chosen checkpoint, when it has any.
    pub(super) counts: Option<AttrCounts>,
    /// The chosen checkpoint's sessions.
    pub(super) sessions: Vec<SessionRef>,
}

/// Read one checkpoint's metadata, file set and sessions.
async fn checkpoint_facts(store: &SqliteStore, id: &str) -> Result<CheckpointFacts> {
    let rows = store
        .query(
            "SELECT created_at, strategy FROM checkpoint WHERE checkpoint_id = ?",
            &[Value::text(id.to_owned())],
        )
        .await?;
    let row = match rows.first() {
        Some(r) => Some(r.get_i64(0)?),
        None => None,
    };
    let strategy = match rows.first() {
        Some(r) => Some(r.get_text(1)?),
        None => None,
    };
    let mut facts = CheckpointFacts {
        // The sentinel is CLASSIFIED here, not passed through: a pre-fix
        // native row's `-1` is "no recorded time", and letting it ride as
        // `Some(-1)` made the tie-break call it resolved metadata and the
        // documents print it as an instant. The row's file evidence still
        // loads below — an undated checkpoint that touched the file must
        // keep beating a dated one that did not (rank criterion 1 outranks
        // criterion 3).
        created_at_ms: row.filter(|&ms| ms != rift_shadow::CREATED_AT_UNKNOWN),
        strategy,
        // Its own round trip, not a join onto the row above: `checkpoint_worktree`
        // is a v14 relation and a projection that predates it must cost the
        // LABEL, never the answer.
        worktree: checkpoint_worktree(store, id).await,
        ..CheckpointFacts::default()
    };
    if row.is_none() {
        return Ok(facts);
    }
    for r in store
        .query(
            "SELECT fr.path FROM attribution a \
             JOIN file_revision fr ON fr.file_revision_id = a.file_revision_id \
             WHERE a.checkpoint_id = ?",
            &[Value::text(id.to_owned())],
        )
        .await?
    {
        facts.files_touched.push(r.get_text(0)?);
    }
    for r in store
        .query(
            "SELECT sic.session_id, COALESCE(s.agent, 'unknown'), COALESCE(s.model_primary, 'unknown') \
             FROM session_in_checkpoint sic \
             LEFT JOIN session s ON s.session_id = sic.session_id \
             WHERE sic.checkpoint_id = ? ORDER BY sic.ordinal",
            &[Value::text(id.to_owned())],
        )
        .await?
    {
        facts.sessions.push(SessionRef {
            session_id: r.get_text(0)?,
            agent: Label::of(&r.get_text(1)?),
            model: Label::of(&r.get_text(2)?),
        });
    }
    Ok(facts)
}

/// The per-file authorship counts one checkpoint recorded for `path`.
async fn file_counts(store: &SqliteStore, id: &str, path: &str) -> Result<Option<AttrCounts>> {
    let rows = store
        .query(
            "SELECT a.agent_added, a.human_added, a.human_modified_agent, a.confidence_milli \
             FROM attribution a \
             JOIN file_revision fr ON fr.file_revision_id = a.file_revision_id \
             WHERE a.checkpoint_id = ? AND fr.path = ?",
            &[Value::text(id.to_owned()), Value::text(path.to_owned())],
        )
        .await?;
    let mut acc: Option<AttrCounts> = None;
    for r in &rows {
        // A path can have several revisions inside one checkpoint; the counts
        // add, and the weakest confidence is the honest one to report.
        let c = AttrCounts {
            agent_added: r.get_i64(0)?,
            human_added: r.get_i64(1)?,
            human_modified_agent: r.get_i64(2)?,
            confidence_milli: r.get_i64(3)?,
        };
        acc = Some(acc.map_or(c, |a| a.merge(c)));
    }
    Ok(acc)
}

/// How much work [`resolve`] actually did.
///
/// Every field counts a **read that happened**, incremented at the call site
/// that performs it. That distinction is the point: the first version of the
/// caching test asserted `facts.len() == 1`, which is the size of the returned
/// map, and a map has one entry per distinct key whether it was filled once or
/// three hundred times — so defeating `fact_cache` or `count_cache` left the
/// suite green while the work grew a hundredfold. It is the same "counts cache
/// entries, not work" defect this module's own tests caught in the summary one
/// level up, recurring inside the test written to fix it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ::serde::Serialize)]
pub(super) struct Reads {
    /// Commit messages read.
    pub(super) messages: usize,
    /// `checkpoint` / `attribution` / `session` reads for checkpoint metadata.
    pub(super) facts: usize,
    /// Per-file attribution reads, one per distinct (checkpoint, path).
    pub(super) counts: usize,
}

/// Everything [`resolve`] produced.
pub(super) struct Resolution {
    /// One answer per span, in span order.
    pub(super) rows: Vec<Resolved>,
    /// Claims by commit oid — the renderer reads this so a `ClaimSet` is
    /// serialized once per commit rather than once per span.
    pub(super) claims: HashMap<String, ClaimSet>,
    /// Checkpoint metadata by id, likewise serialized once.
    pub(super) facts: HashMap<String, CheckpointFacts>,
    /// What it cost, in reads.
    pub(super) reads: Reads,
}

/// Resolve every span against the projection.
///
/// `read_message` is injected rather than called directly so that "one message
/// read per **commit**, not per line" is a testable property of this function
/// instead of a sentence in a doc comment: a test counts the calls. The two
/// projection reads are counted in [`Reads`] for the same reason, and by the
/// same rule — a number that only a *narrower* code path can change.
pub(super) async fn resolve<F>(
    store: &SqliteStore,
    spans: Vec<Span>,
    mut read_message: F,
) -> Result<Resolution>
where
    F: FnMut(&str) -> Result<ClaimSet>,
{
    let mut memo = Memo::default();
    let mut rows: Vec<Resolved> = Vec::new();
    for span in spans {
        if is_uncommitted(&span.commit) {
            rows.push(Resolved {
                span,
                selection: None,
                counts: None,
                sessions: Vec::new(),
            });
            continue;
        }
        // Both the claims and the CANDIDATE LIST are memoised per commit. The
        // candidate list is a pure function of the commit — `select_for_file`
        // takes the path as a separate argument — so rebuilding it per span
        // would clone every checkpoint's whole file set once per span, which
        // for a many-span file is the per-line cost this verb is meant to
        // avoid wearing a different hat.
        if !memo.candidates.contains_key(&span.commit) {
            let set = read_message(&span.commit)?;
            memo.reads.messages += 1;
            let candidates = commit_candidates(&mut memo, store, &span.commit, &set).await?;
            memo.claims.insert(span.commit.clone(), set);
            memo.candidates.insert(span.commit.clone(), candidates);
        }
        let selection = memo
            .candidates
            .get(&span.commit)
            .and_then(|c| select_for_file(c, &span.path));
        let (counts, sessions) =
            span_answer(&mut memo, store, selection.as_ref(), &span.path).await?;
        rows.push(Resolved {
            span,
            selection,
            counts,
            sessions,
        });
    }
    Ok(Resolution {
        rows,
        claims: memo.claims,
        facts: memo.facts,
        reads: memo.reads,
    })
}

/// The memo tables one [`resolve`] pass fills.
///
/// One value rather than four `&mut` arguments threaded through two steps: a
/// cache and the counter that proves the cache worked belong together, and
/// apart they are two things to remember to keep in step.
#[derive(Default)]
struct Memo {
    claims: HashMap<String, ClaimSet>,
    candidates: HashMap<String, Vec<CheckpointCandidate>>,
    facts: HashMap<String, CheckpointFacts>,
    counts: HashMap<(String, String), Option<AttrCounts>>,
    reads: Reads,
}

/// One commit's candidate checkpoints: the UNION of what the commit SAYS and
/// what the projection RECORDED — trailer claims first (in trailer order),
/// then every checkpoint whose recorded anchor (`checkpoint_commit`, written
/// by the bridge from the shard's own metadata) is this commit.
///
/// The union exists because the two can diverge and were MEASURED diverging:
/// the predicted trailer is derived before the commit from an archive that
/// carries no transcript, while the landed shard of a transcript-carrying
/// session includes its transcript and digests in the id — so on exactly the
/// sessions with the richest provenance, the trailer names an id that never
/// lands and a trailer-only walk answers `unknown` forever. The anchored
/// checkpoint is not a guess: the shard itself recorded which commit it
/// captured. Where trailer and anchor agree the union is a set-union no-op;
/// where they disagree, rank (file evidence, then resolved metadata, then
/// created-at) picks among REAL rows instead of phantoms.
///
/// This union is the PERMANENT answer, not a stopgap until the trailer learns
/// the transcript — the trailer is a claim by definition, and
/// `prepare-commit-msg` keeps the 100 ms git budget.
async fn commit_candidates(
    memo: &mut Memo,
    store: &SqliteStore,
    commit: &str,
    set: &ClaimSet,
) -> Result<Vec<CheckpointCandidate>> {
    let anchored: Vec<String> = store
        .query(
            "SELECT checkpoint_id FROM checkpoint_commit \
             WHERE commit_oid = ? ORDER BY checkpoint_id",
            &[Value::text(commit.to_owned())],
        )
        .await?
        .iter()
        .filter_map(|r| r.get_text(0).ok())
        .collect();
    let mut candidates = Vec::with_capacity(set.claims.len());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let ids = set
        .claims
        .iter()
        .map(|c| &c.checkpoint_id)
        .chain(anchored.iter());
    for (ordinal, id) in ids.enumerate() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if !memo.facts.contains_key(id) {
            let f = checkpoint_facts(store, id).await?;
            memo.reads.facts += 1;
            memo.facts.insert(id.clone(), f);
        }
        let facts = memo.facts.get(id).cloned().unwrap_or_default();
        candidates.push(CheckpointCandidate {
            checkpoint_id: id.clone(),
            ordinal,
            created_at_ms: facts.created_at_ms,
            files_touched: facts.files_touched,
        });
    }
    Ok(candidates)
}

/// One span's per-file counts and the chosen checkpoint's sessions.
///
/// Memoised by (checkpoint, path) rather than by span: a file's spans
/// routinely share both, and the read is the per-line cost this verb exists to
/// beat. A span nothing was selected for asks nothing.
async fn span_answer(
    memo: &mut Memo,
    store: &SqliteStore,
    selection: Option<&Selection>,
    path: &str,
) -> Result<(Option<AttrCounts>, Vec<SessionRef>)> {
    let Some(sel) = selection else {
        return Ok((None, Vec::new()));
    };
    let key = (sel.checkpoint_id.clone(), path.to_owned());
    if !memo.counts.contains_key(&key) {
        let c = file_counts(store, &sel.checkpoint_id, path).await?;
        memo.reads.counts += 1;
        memo.counts.insert(key.clone(), c);
    }
    let sessions = memo
        .facts
        .get(&sel.checkpoint_id)
        .map(|f| f.sessions.clone())
        .unwrap_or_default();
    Ok((memo.counts.get(&key).copied().flatten(), sessions))
}

/// Read one commit's message and parse every checkpoint it claims.
pub(crate) fn commit_claims(repo: &rift_git::RepoPaths, oid: &str) -> Result<ClaimSet> {
    // The oid comes from git's own porcelain, but validating it here is what
    // makes "no argument injection" a property of this call site rather than of
    // git's output format.
    rift_git::validate_oid(oid)?;
    let args = vec![
        "log".to_owned(),
        "-1".to_owned(),
        "--format=%B".to_owned(),
        oid.to_owned(),
        "--".to_owned(),
    ];
    let (ok, out) = rift_git::run_git_status(repo, &args, &[], &[])?;
    if !ok {
        return Ok(ClaimSet::default());
    }
    Ok(parse_checkpoint_claims(&String::from_utf8_lossy(&out)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::blame::{spans, BlamedLine};
    use crate::provenance::tests::oid;

    fn recovery_capture(agent: bool) -> rift_shadow::attribution::CommitCapture {
        use rift_git::{MemoryBlobReader, TreeEntry};
        use rift_shadow::attribution::{capture_commit, HunkAttribution, LineAttribution};
        let reader = MemoryBlobReader::new()
            .with_blob("old", b"one\nBASE\n")
            .with_blob("new", b"one\nCHANGED\n")
            .with_tree(
                "parent",
                vec![TreeEntry {
                    path: "f.rs".into(),
                    blob_oid: "old".into(),
                }],
            )
            .with_tree(
                "commit",
                vec![TreeEntry {
                    path: "f.rs".into(),
                    blob_oid: "new".into(),
                }],
            )
            .with_parent("commit", "parent");
        let mut capture = capture_commit(&reader, "commit").unwrap();
        let f = &mut capture.files[0];
        f.attribution = if agent {
            LineAttribution {
                agent_lines: 1,
                ..Default::default()
            }
        } else {
            LineAttribution {
                unknown_lines: 1,
                ..Default::default()
            }
        };
        f.hunk_categories = vec![HunkAttribution {
            hunk_ordinal: 0,
            new_start: 1,
            new_len: 1,
            category: if agent { "agent_added" } else { "unknown" }.into(),
            line_count: 1,
        }];
        f.method_version = if agent {
            "turn-replay@1"
        } else {
            "commit-diff@1"
        }
        .into();
        capture
    }

    #[test]
    fn recovery_conflict_is_refused_while_the_public_reader_retains_historical_counts() {
        runtime().block_on(async {
            let s = SqliteStore::open_in_memory().unwrap();
            s.create_canonical_schema().unwrap();
            let original = recovery_capture(true);
            rift_shadow::persist_commit_capture(&s, &original, Some("fixed-checkpoint"))
                .await.unwrap();
            let before = file_counts(&s, "fixed-checkpoint", "f.rs").await.unwrap();
            assert_eq!(before.unwrap().agent_added, 1);
            assert_eq!(Tag::of(before.as_ref()), Tag::Ai);
            rift_shadow::persist_commit_capture(&s, &original, Some("fixed-checkpoint"))
                .await.unwrap();
            assert_eq!(file_counts(&s, "fixed-checkpoint", "f.rs").await.unwrap(), before);
            assert_eq!(s.count("attribution_hunk").await.unwrap(), 1);
            let retry = rift_shadow::persist_commit_capture(
                &s, &recovery_capture(false), Some("fixed-checkpoint"),
            ).await;
            let counts = file_counts(&s, "fixed-checkpoint", "f.rs").await.unwrap();
            let details = s.query("SELECT category, line_count FROM attribution_hunk ORDER BY category", &[])
                .await.unwrap();
            let tag = Tag::of(counts.as_ref());
            eprintln!("public recovery: result={retry:?}; counts={counts:?}; tag={tag:?}; details={details:?}");
            assert!(retry.is_err(), "a changed recomputation must be visibly refused");
            assert_eq!(counts, before, "refusal does not repair historical counts");
            assert_eq!(tag, Tag::Ai, "the original historical public result is retained");
            assert_eq!(details.len(), 1, "no conflicting Unknown category appended");
            assert_eq!(details[0].get_text(0).unwrap(), "agent_added");
        });
    }

    /// A projection holding two checkpoints: a sentinel-dated one that
    /// RECORDED touching `f.rs`, and a real-dated one that did not.
    async fn sentinel_and_dated_store() -> SqliteStore {
        let s = rift_store::SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&s, &rift_store::default_migrations()).unwrap();
        let t = |v: &str| Value::text(v.to_owned());
        let cols = [
            "checkpoint_id",
            "strategy",
            "created_at",
            "merkle_root",
            "schema_version",
        ];
        s.insert(
            "checkpoint",
            &cols,
            &[
                t("csent"),
                t("manual-commit"),
                Value::Int(rift_shadow::CREATED_AT_UNKNOWN),
                t("00"),
                Value::Int(1),
            ],
        )
        .await
        .unwrap();
        rift_store::record_file_revision(
            &s,
            &rift_store::FileRevisionRow {
                file_revision_id: "fr1".into(),
                path: "f.rs".into(),
                blob_oid: "b1".into(),
                content_sha256: "c1".into(),
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        rift_store::record_attribution(
            &s,
            &rift_store::AttributionRow {
                checkpoint_id: "csent".into(),
                file_revision_id: "fr1".into(),
                agent_added: 2,
                human_added: 0,
                human_modified_agent: 0,
                agent_removed_by_human: 0,
                confidence_milli: 900,
                method_version: "m1".into(),
                supersedes_pk: None,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        s.insert(
            "checkpoint",
            &cols,
            &[
                t("creal"),
                t("manual-commit"),
                Value::Int(1_000),
                t("00"),
                Value::Int(1),
            ],
        )
        .await
        .unwrap();
        s
    }

    /// A single-threaded runtime for the store-backed tests here.
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
    }

    /// **The unknown-time sentinel classifies as "no recorded time" at the
    /// facts read**: a `checkpoint.created_at = -1` row — every
    /// native checkpoint archived before the writer fix — must not ride into
    /// selection as `Some(-1)`, where it ranked as *resolved metadata* older
    /// than everything and rendered in the `--json` documents as if `-1`
    /// were an instant. The classification must NOT cost the row its file
    /// evidence.
    #[test]
    fn a_sentinel_dated_row_classifies_as_undated_but_keeps_its_file_evidence() {
        runtime().block_on(async {
            let s = sentinel_and_dated_store().await;
            let sent = checkpoint_facts(&s, "csent").await.unwrap();
            assert_eq!(sent.created_at_ms, None, "the sentinel is not a time");
            assert_eq!(
                sent.files_touched,
                vec!["f.rs".to_owned()],
                "classification must not cost the row its evidence"
            );
            let real = checkpoint_facts(&s, "creal").await.unwrap();
            assert_eq!(real.created_at_ms, Some(1_000));
        });
    }

    /// The other honest absence: a claimed id with no local row reads as
    /// undated too, and carries no file evidence to tell it apart by.
    #[test]
    fn a_checkpoint_this_machine_has_never_seen_is_undated_and_evidence_free() {
        runtime().block_on(async {
            let s = sentinel_and_dated_store().await;
            let absent = checkpoint_facts(&s, "nowhere").await.unwrap();
            assert_eq!(absent.created_at_ms, None);
            assert!(absent.files_touched.is_empty());
        });
    }

    /// Composed into selection exactly as `resolve` composes it: the undated
    /// toucher beats the dated non-toucher on file evidence, because rank
    /// criterion 1 outranks criterion 3 — and SAYS it is unresolved, instead
    /// of `Some(-1)`'s old false "resolved" claim.
    #[test]
    fn an_undated_toucher_outranks_a_dated_checkpoint_that_never_saw_the_file() {
        runtime().block_on(async {
            let s = sentinel_and_dated_store().await;
            let sent = checkpoint_facts(&s, "csent").await.unwrap();
            let real = checkpoint_facts(&s, "creal").await.unwrap();
            let candidates = [
                CheckpointCandidate {
                    checkpoint_id: "csent".into(),
                    ordinal: 1,
                    created_at_ms: sent.created_at_ms,
                    files_touched: sent.files_touched.clone(),
                },
                CheckpointCandidate {
                    checkpoint_id: "creal".into(),
                    ordinal: 0,
                    created_at_ms: real.created_at_ms,
                    files_touched: real.files_touched.clone(),
                },
            ];
            let sel = select_for_file(&candidates, "f.rs").unwrap();
            assert_eq!(
                sel.checkpoint_id, "csent",
                "file evidence outranks the clock"
            );
            assert!(!sel.resolved, "a sentinel-dated winner is unresolved");
        });
    }

    #[test]
    fn a_tag_is_never_stronger_than_the_counts_behind_it() {
        assert_eq!(Tag::of(None), Tag::Unknown);
        assert_eq!(Tag::of(Some(&AttrCounts::default())), Tag::Unknown);
        let ai = AttrCounts {
            agent_added: 4,
            ..AttrCounts::default()
        };
        assert_eq!(Tag::of(Some(&ai)), Tag::Ai);
        let human = AttrCounts {
            human_added: 4,
            ..AttrCounts::default()
        };
        assert_eq!(Tag::of(Some(&human)), Tag::Human);
        // Both present, or a human edit over agent lines: per-file counts
        // cannot name the author of a single line.
        for c in [
            AttrCounts {
                agent_added: 4,
                human_added: 1,
                ..AttrCounts::default()
            },
            AttrCounts {
                agent_added: 4,
                human_modified_agent: 1,
                ..AttrCounts::default()
            },
        ] {
            assert_eq!(Tag::of(Some(&c)), Tag::Mixed);
        }
    }

    /// The claim this verb exists to make good on, stated so it can fail **at
    /// every level**.
    ///
    /// 300 lines alternating between 3 commits is the worst case for span
    /// collapsing — every line is its own span — so if any read were per line
    /// this would do 300 of them. It does 3 message reads, 1 checkpoint read
    /// and 1 per-file attribution read.
    ///
    /// The first cut of this test asserted `facts.len() == 1`, and that assertion
    /// **cannot fail**: it is the size of the returned map, which has one entry
    /// per distinct key whether it was filled once or three hundred times. Only
    /// the candidate cache was really pinned; defeating `fact_cache` or
    /// `count_cache` left the suite green. The counts below are incremented at
    /// the read sites themselves, so each cache has a number that a mutation to
    /// it moves — verified by mutating all three.
    #[test]
    fn every_cache_this_verb_relies_on_has_a_number_that_can_move() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = rift_store::open_capture_projection(dir.path().join("projection.db"))
            .expect("open projection");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let ckpt: String = std::iter::repeat_n('d', 64).collect();
        let lines: Vec<BlamedLine> = (0..300)
            .map(|i| BlamedLine {
                commit: oid(['a', 'b', 'c'][i % 3]),
                path: "src/lib.rs".to_owned(),
            })
            .collect();
        let sp = spans(&lines);
        assert_eq!(sp.len(), 300, "no two adjacent lines share a commit");

        let calls = std::cell::Cell::new(0usize);
        let r = rt
            .block_on(resolve(&store, sp, |_oid| {
                calls.set(calls.get() + 1);
                Ok(parse_checkpoint_claims(&format!(
                    "s\n\nRift-Checkpoint: {ckpt}\n"
                )))
            }))
            .expect("resolve");

        assert_eq!(r.rows.len(), 300);
        assert_eq!(
            r.reads,
            Reads {
                messages: 3,
                facts: 1,
                counts: 1,
            },
            "300 spans, 3 commits, 1 checkpoint, 1 path"
        );
        // The injected closure counts the same reads from outside the function,
        // so the two disagreeing would mean the counter, not the cache, moved.
        assert_eq!(calls.get(), r.reads.messages);
        // The maps are still returned for the renderer, and their SIZES are
        // exactly what the old assertion measured — kept here to show they do
        // not distinguish a working cache from a broken one.
        assert_eq!(r.claims.len(), 3);
        assert_eq!(r.facts.len(), 1);
        // Every span still gets an answer: caching is not skipping.
        assert!(r.rows.iter().all(|x| x.selection.is_some()));
    }
}
