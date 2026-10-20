//! The capture-side oracle a pull verb consults.
//!
//! Its own module because it is the one reader here that opens a projection of
//! its own, up front and read-only, rather than being handed a store: a
//! serving handler is async and the oracle it consults is not.

use std::collections::{BTreeMap, HashMap};

use rift_core::Result;
use rift_recall_types::{Author, CheckpointFacts as ServedFacts, ProvenanceOracle, TokenClasses};
use rift_store::capture::{checkpoint_window_tokens, WindowScope};
use rift_store::{quote_usage_costs, CanonicalStore, CostQuoteScope, SqliteStore};

use super::resolve::{AttrCounts, Tag};

/// The facts a pull verb's `provenance` object needs and the recall projection
/// does not hold: when a checkpoint was recorded, which session and agent
/// family took it, what the model calls that checkpoint froze cost, and the
/// subject of the commit a checkpoint anchored.
///
/// **Resolved once, up front, and never lazily.** The serving handlers are
/// `async` and the oracle they consult is not, so a lazy implementation would
/// have to block on a store inside a running runtime. It is also the cheaper
/// shape: the alternative is one round trip per manifest entry, and a manifest
/// names up to 64 files. Five queries and one `git log` answer for the whole
/// repository, whatever the manifest's size: the recorded times, the first
/// sessions, one grouped token sum over every checkpoint's usage window, and
/// the quote's two batched reads.
///
/// **A checkpoint's figures are its usage window's**, never its session's
/// totals: the calls its first session started since that session's previous
/// checkpoint, up to this one ([`rift_store::capture::checkpoint_usage_windows`]).
/// The object brackets a change by checkpoint, so a session total under a
/// checkpoint's name would read as what that change cost. The token sum and the
/// quote frame their calls through the one store-side window, which is also the
/// one `rift why` reads — a second spelling of the join is how two surfaces
/// come to disagree about what a checkpoint cost. An undated checkpoint has no
/// window and carries neither figure.
///
/// Every field is independently absent, and absence is **unknown**: a
/// projection that will not open, a query that fails, a `git log` that cannot
/// run — each leaves its own facts out and none of them fails the serve.
pub(crate) struct CaptureProvenance {
    /// Checkpoint id → what the capture projection records about it.
    by_checkpoint: HashMap<String, ServedFacts>,
    /// Commit oid → its subject line, for the commits this checkout can see.
    subjects: HashMap<String, String>,
    /// (checkpoint, path) → the attribution recorded for that file there.
    files: HashMap<(String, String), FileRecord>,
}

/// One file's attribution at one checkpoint: the per-file counts `rift why`
/// reads, and the per-hunk categories that can split a mixed file by line.
#[derive(Default)]
struct FileRecord {
    counts: Option<AttrCounts>,
    /// New-side start (0-based, the diff convention), length, and category.
    hunks: Vec<(i64, i64, String)>,
}

/// How far back the commit-subject map reads.
///
/// A bound rather than the whole history: the subject is a nicety beside the
/// oid, and a repository with a hundred thousand commits must not pay for one.
/// A delta older than this carries its commit oid and no subject, which is the
/// same honest shape a delta with no mirrored commit has.
const SUBJECT_WALK: usize = 2000;

impl CaptureProvenance {
    /// Read the capture projection and this checkout's recent history.
    ///
    /// Never an error. Everything it cannot reach is simply not known, and a
    /// pull verb answers with the recall-side facts alone.
    pub(crate) async fn read_capture_side(db: &std::path::Path, root: &std::path::Path) -> Self {
        let mut out = Self {
            by_checkpoint: HashMap::new(),
            subjects: HashMap::new(),
            files: HashMap::new(),
        };
        if let Ok(store) = SqliteStore::open_read_only(db) {
            let _ = out.read_projection(&store).await;
        }
        out.read_subjects(root);
        out
    }

    /// The aggregate reads, in one pass.
    ///
    /// A read that fails ends the pass, so everything after it is absent; the
    /// serve still answers, because the caller discards this result and the
    /// recall-side facts stand on their own.
    async fn read_projection(&mut self, store: &SqliteStore) -> Result<()> {
        self.read_recorded_times(store).await?;
        for (ck, first) in read_first_sessions(store).await? {
            let entry = self.by_checkpoint.entry(ck).or_default();
            entry.agent = first.agent;
            entry.session = Some(first.session_id);
        }
        self.read_window_tokens(store).await?;
        self.read_window_costs(store).await?;
        self.read_attribution(store).await
    }

    /// WHO each checkpoint's attribution credits, per file and per hunk. Read
    /// last, so a store too old to carry the tables loses only this, and every
    /// change in it stays unknown rather than credited.
    async fn read_attribution(&mut self, store: &SqliteStore) -> Result<()> {
        for r in store
            .query(
                "SELECT a.checkpoint_id, fr.path, a.agent_added, a.human_added, \
                 a.human_modified_agent, a.confidence_milli FROM attribution a \
                 JOIN file_revision fr ON fr.file_revision_id = a.file_revision_id",
                &[],
            )
            .await?
        {
            let c = AttrCounts {
                agent_added: r.get_i64(2)?,
                human_added: r.get_i64(3)?,
                human_modified_agent: r.get_i64(4)?,
                confidence_milli: r.get_i64(5)?,
            };
            let key = (r.get_text(0)?, r.get_text(1)?);
            let record = self.files.entry(key).or_default();
            record.counts = Some(record.counts.map_or(c, |a| a.merge(c)));
        }
        for r in store
            .query(
                "SELECT h.checkpoint_id, fr.path, h.new_start, h.new_len, h.category \
                 FROM attribution_hunk h \
                 JOIN file_revision fr ON fr.file_revision_id = h.file_revision_id",
                &[],
            )
            .await?
        {
            let key = (r.get_text(0)?, r.get_text(1)?);
            let hunk = (r.get_i64(2)?, r.get_i64(3)?, r.get_text(4)?);
            self.files.entry(key).or_default().hunks.push(hunk);
        }
        Ok(())
    }

    /// WHEN each checkpoint was recorded.
    ///
    /// The unknown-time sentinel is classified here exactly as
    /// `checkpoint_facts` classifies it: `-1` is "no recorded time", not an
    /// instant in 1969.
    async fn read_recorded_times(&mut self, store: &SqliteStore) -> Result<()> {
        for r in store
            .query("SELECT checkpoint_id, created_at FROM checkpoint", &[])
            .await?
        {
            let (id, at) = (r.get_text(0)?, r.get_i64(1)?);
            self.by_checkpoint.entry(id).or_default().recorded_at_ms =
                (at != rift_shadow::CREATED_AT_UNKNOWN).then_some(at);
        }
        Ok(())
    }

    /// WHAT EACH CHECKPOINT'S CALLS COST in tokens, one grouped query over
    /// every usage window. A checkpoint with no window, or none of whose calls
    /// settled, gets no entry and keeps its tokens absent.
    async fn read_window_tokens(&mut self, store: &SqliteStore) -> Result<()> {
        let sums = checkpoint_window_tokens(store, WindowScope::Every).await?;
        for (ck, t) in sums {
            self.by_checkpoint.entry(ck).or_default().tokens = Some(TokenClasses {
                uncached_input: t.uncached_input,
                cached_input: t.cached_input,
                cache_creation: t.cache_creation,
                output: t.output,
            });
        }
        Ok(())
    }

    /// The current catalog quote of each checkpoint's window, preserving
    /// unpriced absence.
    async fn read_window_costs(&mut self, store: &SqliteStore) -> Result<()> {
        let scope = CostQuoteScope::CheckpointWindows(WindowScope::Every);
        for (ck, amount) in quote_usage_costs(store, scope, i64::MAX).await? {
            self.by_checkpoint.entry(ck).or_default().cost_micros = Some(amount);
        }
        Ok(())
    }

    /// The subject of each of the last [`SUBJECT_WALK`] commits.
    ///
    /// One `git log`, not one per delta: the alternative is a process spawn per
    /// manifest entry, which on a 64-entry manifest is the most expensive thing
    /// a pull verb would do. A NUL separates the oid from the subject and the
    /// newline separates records, so a subject containing anything at all still
    /// parses — and a subject containing a newline cannot forge a second record,
    /// because the oid half of a forged line would not be hex.
    fn read_subjects(&mut self, root: &std::path::Path) {
        let Ok(repo) = rift_git::discover(root) else {
            return;
        };
        let args = vec![
            "log".to_owned(),
            format!("-n{SUBJECT_WALK}"),
            "--format=%H%x00%s".to_owned(),
        ];
        let Ok((true, out)) = rift_git::run_git_status(&repo, &args, &[], &[]) else {
            return;
        };
        for line in String::from_utf8_lossy(&out).lines() {
            let Some((oid, subject)) = line.split_once('\0') else {
                continue;
            };
            if !subject.is_empty() && !oid.is_empty() && oid.chars().all(|c| c.is_ascii_hexdigit())
            {
                self.subjects.insert(oid.to_owned(), subject.to_owned());
            }
        }
    }
}

impl ProvenanceOracle for CaptureProvenance {
    fn checkpoint(&self, checkpoint_id: &str) -> Option<ServedFacts> {
        self.by_checkpoint
            .get(checkpoint_id)
            .filter(|f| !f.is_empty())
            .cloned()
    }

    fn commit_subject(&self, oid: &str) -> Option<String> {
        self.subjects.get(oid).cloned()
    }

    fn author(&self, checkpoint_id: &str, path: &str, line: Option<u32>) -> Author {
        self.files
            .get(&(checkpoint_id.to_owned(), path.to_owned()))
            .map_or(Author::Unknown, |record| record.author(line))
    }
}

/// Who the attribution recorded at `checkpoint_id` credits with `path`, or
/// with the hunk holding `line`. `rift why` asks this for its one answering
/// row so that it and recall provenance decide credit by the same rule; the
/// per-file `counts` are the ones `why` already read.
pub(super) async fn author_at(
    store: &SqliteStore,
    checkpoint_id: &str,
    path: &str,
    counts: Option<AttrCounts>,
    line: Option<usize>,
) -> Result<Author> {
    let mut record = FileRecord {
        counts,
        hunks: Vec::new(),
    };
    for r in store
        .query(
            "SELECT h.new_start, h.new_len, h.category FROM attribution_hunk h \
             JOIN file_revision fr ON fr.file_revision_id = h.file_revision_id \
             WHERE h.checkpoint_id = ? AND fr.path = ?",
            &[
                rift_store::Value::text(checkpoint_id.to_owned()),
                rift_store::Value::text(path.to_owned()),
            ],
        )
        .await?
    {
        record
            .hunks
            .push((r.get_i64(0)?, r.get_i64(1)?, r.get_text(2)?));
    }
    Ok(record.author(line.and_then(|n| u32::try_from(n).ok())))
}

impl FileRecord {
    /// The hunks covering `line` decide when there are any; otherwise the
    /// file's counts do, through the same tag `rift why` prints, so a file
    /// both wrote is never handed to one of them.
    fn author(&self, line: Option<u32>) -> Author {
        let covering: Vec<&str> = line.map_or_else(Vec::new, |n| self.categories_at(n));
        if covering.is_empty() {
            return match Tag::of(self.counts.as_ref()) {
                Tag::Ai => Author::Agent,
                Tag::Human => Author::Human,
                Tag::Mixed => Author::Mixed,
                Tag::Unknown => Author::Unknown,
            };
        }
        let agent = covering.contains(&"agent_added");
        let human = covering
            .iter()
            .any(|c| matches!(*c, "human_added" | "human_modified_agent"));
        match (agent, human) {
            (true, false) => Author::Agent,
            (false, true) => Author::Human,
            (true, true) => Author::Mixed,
            (false, false) => Author::Unknown,
        }
    }

    /// The categories of the hunks whose new side holds 1-based `line`.
    fn categories_at(&self, line: u32) -> Vec<&str> {
        let at = i64::from(line) - 1;
        self.hunks
            .iter()
            .filter(|(start, len, _)| *start <= at && at < start.saturating_add(*len))
            .map(|(_, _, category)| category.as_str())
            .collect()
    }
}

/// The session that took a checkpoint, and its agent family as the `session`
/// row records it.
struct FirstSession {
    session_id: String,
    agent: Option<String>,
}

/// WHO took each checkpoint: the first session by ordinal, the same one
/// `rift why` reports and the one whose usage window the figures frame. A tie
/// on ordinal breaks by session id, as the window does.
async fn read_first_sessions(store: &SqliteStore) -> Result<BTreeMap<String, FirstSession>> {
    let mut first: BTreeMap<String, (i64, FirstSession)> = BTreeMap::new();
    for r in store
        .query(
            "SELECT sic.checkpoint_id, sic.session_id, sic.ordinal, s.agent \
             FROM session_in_checkpoint sic \
             LEFT JOIN session s ON s.session_id = sic.session_id",
            &[],
        )
        .await?
    {
        let (ck, session_id, ordinal) = (r.get_text(0)?, r.get_text(1)?, r.get_i64(2)?);
        let earlier = |(seen, held): &(i64, FirstSession)| {
            (*seen, held.session_id.as_str()) <= (ordinal, session_id.as_str())
        };
        if first.get(&ck).is_some_and(earlier) {
            continue;
        }
        let agent = r.get_opt_text(3)?;
        first.insert(ck, (ordinal, FirstSession { session_id, agent }));
    }
    Ok(first.into_iter().map(|(ck, (_, s))| (ck, s)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An agent hunk and a between-turns hunk in one file: each line goes to
    /// the hunk that holds it, and a line in neither falls back to the file's
    /// tag, which for a file both wrote names no one.
    #[test]
    fn provenance_author_follows_the_hunk_that_holds_the_line() {
        let record = FileRecord {
            counts: Some(AttrCounts {
                agent_added: 2,
                human_added: 1,
                human_modified_agent: 0,
                confidence_milli: 1000,
            }),
            hunks: vec![
                (1, 2, "agent_added".to_owned()),
                (5, 1, "human_added".to_owned()),
            ],
        };
        assert_eq!(record.author(Some(2)), Author::Agent);
        assert_eq!(record.author(Some(3)), Author::Agent);
        assert_eq!(record.author(Some(6)), Author::Human);
        assert_eq!(record.author(Some(1)), Author::Mixed);
        assert_eq!(record.author(None), Author::Mixed);
        let human_only = FileRecord {
            counts: Some(AttrCounts {
                human_added: 2,
                ..AttrCounts::default()
            }),
            hunks: Vec::new(),
        };
        assert_eq!(human_only.author(Some(1)), Author::Human);
        assert_eq!(FileRecord::default().author(Some(1)), Author::Unknown);
    }

    async fn quote_fixture(s: &SqliteStore) {
        let turn = rift_store::TurnRow {
            turn_id: "turn".into(),
            session_id: "session".into(),
            ordinal: 1,
            started_at: 0,
            schema_version: 1,
        };
        rift_store::record_turn(s, &turn).await.unwrap();
        rift_store::record_model_call(
            s,
            &rift_store::ModelCallRow {
                call_id: "call".into(),
                turn_id: turn.turn_id,
                model: "fixture-model".into(),
                started_at: 10,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        rift_store::record_token_usage(
            s,
            &rift_store::TokenUsageRow {
                call_id: "call".into(),
                uncached_input: 1000,
                cached_input: 0,
                cache_creation: 0,
                output: 0,
                counter_method: "provider_reported".into(),
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        for (prefix, input, belief) in [("old", 1_000_000, 1), ("new", 2_000_000, 2)] {
            for class in ["uncached_input", "cached_input", "cache_creation", "output"] {
                rift_store::record_price(
                    s,
                    &rift_store::PriceTableRow {
                        price_id: format!("{prefix}-{class}"),
                        model: "fixture-model".into(),
                        token_class: class.into(),
                        unit_price_micros: input,
                        currency: "USD".into(),
                        valid_from: 0,
                        valid_to: None,
                        recorded_at: belief,
                        supersedes_id: None,
                        schema_version: 1,
                    },
                )
                .await
                .unwrap();
            }
        }
        for id in ["orphan-old", "orphan-new"] {
            rift_store::record_cost_fact(
                s,
                &rift_store::CostFact {
                    cost_fact_id: id.into(),
                    call_id: "call".into(),
                    amount_micros: 9999,
                    valid_from: 0,
                    valid_to: None,
                    recorded_at: 1,
                    supersedes_id: None,
                    schema_version: 1,
                },
            )
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn capture_provenance_and_why_quote_the_latest_catalog_without_summing_orphan_bookings() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.create_canonical_schema().unwrap();
        quote_fixture(&store).await;
        checkpoint_of(&store, "ck", 10, "session").await;
        let mut before = Vec::new();
        for table in rift_store::canonical_tables() {
            before.push((table.name, store.count(table.name).await.unwrap()));
        }
        let served = projected(&store).await.checkpoint("ck").unwrap();
        assert_eq!(served.cost_micros, Some(2000));
        let why = crate::provenance::why::checkpoint_cost(&store, "ck")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(why.amount_micros, Some(2000));
        assert_eq!(why.uncached_input, 1000);
        for (table, count) in before {
            assert_eq!(store.count(table).await.unwrap(), count);
        }
    }

    /// Every class priced at two micro-USD per token, believed from instant 1.
    async fn flat_prices(s: &SqliteStore) {
        for class in ["uncached_input", "cached_input", "cache_creation", "output"] {
            rift_store::record_price(
                s,
                &rift_store::PriceTableRow {
                    price_id: format!("flat-{class}"),
                    model: "fixture-model".into(),
                    token_class: class.into(),
                    unit_price_micros: 2_000_000,
                    currency: "USD".into(),
                    valid_from: 0,
                    valid_to: None,
                    recorded_at: 1,
                    supersedes_id: None,
                    schema_version: 1,
                },
            )
            .await
            .unwrap();
        }
    }

    /// One settled call of `session`, alone in its turn, starting at `at`.
    async fn settled_call(s: &SqliteStore, session: &str, call: &str, at: i64, input: i64) {
        let turn = format!("turn-{call}");
        let row = rift_store::TurnRow {
            turn_id: turn.clone(),
            session_id: session.into(),
            ordinal: at,
            started_at: at,
            schema_version: 1,
        };
        rift_store::record_turn(s, &row).await.unwrap();
        let mc = rift_store::ModelCallRow {
            call_id: call.into(),
            turn_id: turn,
            model: "fixture-model".into(),
            started_at: at,
            schema_version: 1,
        };
        rift_store::record_model_call(s, &mc).await.unwrap();
        let usage = rift_store::TokenUsageRow {
            call_id: call.into(),
            uncached_input: input,
            cached_input: 0,
            cache_creation: 0,
            output: 0,
            counter_method: "provider_reported".into(),
            schema_version: 1,
        };
        rift_store::record_token_usage(s, &usage).await.unwrap();
    }

    /// A checkpoint recorded at `at` that archives `session` first.
    async fn checkpoint_of(s: &SqliteStore, id: &str, at: i64, session: &str) {
        s.insert(
            "checkpoint",
            &[
                "checkpoint_id",
                "strategy",
                "created_at",
                "merkle_root",
                "schema_version",
            ],
            &[
                rift_store::Value::text(id.to_owned()),
                rift_store::Value::text("manual-commit".to_owned()),
                rift_store::Value::Int(at),
                rift_store::Value::text("00".to_owned()),
                rift_store::Value::Int(1),
            ],
        )
        .await
        .unwrap();
        let sic = rift_store::SessionInCheckpointRow {
            checkpoint_id: id.into(),
            session_id: session.into(),
            ordinal: 0,
            schema_version: 1,
        };
        rift_store::record_session_in_checkpoint(s, &sic)
            .await
            .unwrap();
    }

    /// One session, two dated checkpoints and an undated one, with calls
    /// before, between and after them.
    async fn two_checkpoint_fixture() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        flat_prices(&s).await;
        for (call, at, input) in [
            ("a", 50, 1_000),
            ("b", 100, 1_000),
            ("c", 150, 10_000),
            ("d", 200, 10_000),
            ("e", 300, 100_000),
        ] {
            settled_call(&s, "session", call, at, input).await;
        }
        checkpoint_of(&s, "ck-1", 100, "session").await;
        checkpoint_of(&s, "ck-2", 200, "session").await;
        checkpoint_of(&s, "ck-undated", rift_shadow::CREATED_AT_UNKNOWN, "session").await;
        s
    }

    async fn projected(s: &SqliteStore) -> CaptureProvenance {
        let mut out = CaptureProvenance {
            by_checkpoint: HashMap::new(),
            subjects: HashMap::new(),
            files: HashMap::new(),
        };
        out.read_projection(s).await.unwrap();
        out
    }

    #[tokio::test]
    async fn a_checkpoint_carries_only_the_calls_since_its_sessions_previous_checkpoint() {
        let s = two_checkpoint_fixture().await;
        let oracle = projected(&s).await;
        let first = oracle.checkpoint("ck-1").unwrap();
        assert_eq!(first.tokens.map(|t| t.uncached_input), Some(2_000));
        assert_eq!(first.cost_micros, Some(4_000));
        let second = oracle.checkpoint("ck-2").unwrap();
        assert_eq!(second.tokens.map(|t| t.uncached_input), Some(20_000));
        assert_eq!(second.cost_micros, Some(40_000));
        assert_eq!(second.session.as_deref(), Some("session"));
    }

    #[tokio::test]
    async fn an_undated_checkpoint_carries_no_tokens_and_no_cost() {
        let s = two_checkpoint_fixture().await;
        let undated = projected(&s).await.checkpoint("ck-undated").unwrap();
        assert_eq!(undated.session.as_deref(), Some("session"));
        assert_eq!(undated.recorded_at_ms, None);
        assert!(undated.tokens.is_none(), "{:?}", undated.tokens);
        assert_eq!(undated.cost_micros, None);
    }
}
