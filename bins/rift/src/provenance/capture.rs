//! The capture-side oracle a pull verb consults.
//!
//! Its own module because it is the one reader here that opens a projection of
//! its own, up front and read-only, rather than being handed a store: a
//! serving handler is async and the oracle it consults is not.

use std::collections::{BTreeMap, HashMap};

use rift_core::Result;
use rift_recall_types::{CheckpointFacts as ServedFacts, ProvenanceOracle, TokenClasses};
use rift_store::{CanonicalStore, SqliteStore};

/// The facts a pull verb's `provenance` object needs and the recall projection
/// does not hold: when a checkpoint was recorded, which session and agent
/// family took it, what that session's turns cost, and the subject of the
/// commit a checkpoint anchored.
///
/// **Resolved once, up front, and never lazily.** The serving handlers are
/// `async` and the oracle they consult is not, so a lazy implementation would
/// have to block on a store inside a running runtime. It is also the cheaper
/// shape: the alternative is one round trip per manifest entry, and a manifest
/// names up to 64 files. Four aggregate queries and one `git log` answer for
/// the whole repository.
///
/// **Every query here is one the provenance verbs already run** — the dated
/// checkpoint row and its sentinel classification from `checkpoint_facts`,
/// the session/agent join from the same function, and the two sums from
/// [`super::why::session_cost`], grouped by session instead of filtered to one.
/// A second
/// spelling of the same join is how two surfaces come to disagree about what a
/// turn cost.
///
/// Every field is independently absent, and absence is **unknown**: a
/// projection that will not open, a query that fails, a `git log` that cannot
/// run — each leaves its own facts out and none of them fails the serve.
pub(crate) struct CaptureProvenance {
    /// Checkpoint id → what the capture projection records about it.
    by_checkpoint: HashMap<String, ServedFacts>,
    /// Commit oid → its subject line, for the commits this checkout can see.
    subjects: HashMap<String, String>,
}

/// One session's totals, shared by every checkpoint that session appears in.
#[derive(Clone, Debug, Default)]
struct SessionTotals {
    /// The agent family that ran the session, as the `session` row records it.
    agent: Option<String>,
    /// The session's four recorded token classes.
    tokens: Option<TokenClasses>,
    /// Micro-USD over its current cost facts.
    cost_micros: Option<i64>,
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
        };
        if let Ok(store) = SqliteStore::open_read_only(db) {
            let _ = out.read_projection(&store).await;
        }
        out.read_subjects(root);
        out
    }

    /// The four aggregate reads, in one pass.
    ///
    /// A read that fails ends the pass, so everything after it is absent; the
    /// serve still answers, because the caller discards this result and the
    /// recall-side facts stand on their own.
    async fn read_projection(&mut self, store: &SqliteStore) -> Result<()> {
        self.read_recorded_times(store).await?;
        let mut totals: HashMap<String, SessionTotals> = HashMap::new();
        let first_session = read_first_sessions(store, &mut totals).await?;
        read_token_totals(store, &mut totals).await?;
        read_cost_totals(store, &mut totals).await?;
        for (ck, (_, sid)) in first_session {
            let t = totals.get(&sid).cloned().unwrap_or_default();
            let entry = self.by_checkpoint.entry(ck).or_default();
            entry.agent = t.agent;
            entry.tokens = t.tokens;
            entry.cost_micros = t.cost_micros;
            entry.session = Some(sid);
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
}

/// WHO took each checkpoint: the first session by ordinal, the same one
/// `rift why` reports. Returns checkpoint id → (ordinal, session id) and
/// records each session's agent family on the way past.
async fn read_first_sessions(
    store: &SqliteStore,
    totals: &mut HashMap<String, SessionTotals>,
) -> Result<BTreeMap<String, (i64, String)>> {
    let mut first_session: BTreeMap<String, (i64, String)> = BTreeMap::new();
    for r in store
        .query(
            "SELECT sic.checkpoint_id, sic.session_id, sic.ordinal, s.agent \
             FROM session_in_checkpoint sic \
             LEFT JOIN session s ON s.session_id = sic.session_id",
            &[],
        )
        .await?
    {
        let (ck, sid, ord) = (r.get_text(0)?, r.get_text(1)?, r.get_i64(2)?);
        totals.entry(sid.clone()).or_default().agent = r.get_opt_text(3)?;
        match first_session.get(&ck) {
            Some((seen, _)) if *seen <= ord => {}
            _ => {
                first_session.insert(ck, (ord, sid));
            }
        }
    }
    Ok(first_session)
}

/// WHAT EACH SESSION COST in tokens: `session_cost`'s join, grouped by session
/// rather than filtered to one.
async fn read_token_totals(
    store: &SqliteStore,
    totals: &mut HashMap<String, SessionTotals>,
) -> Result<()> {
    for r in store
        .query(
            "SELECT t.session_id, COALESCE(SUM(tu.uncached_input),0), \
             COALESCE(SUM(tu.cached_input),0), COALESCE(SUM(tu.cache_creation),0), \
             COALESCE(SUM(tu.output),0) \
             FROM token_usage tu \
             JOIN model_call mc ON mc.call_id = tu.call_id \
             JOIN turn t ON t.turn_id = mc.turn_id \
             GROUP BY t.session_id",
            &[],
        )
        .await?
    {
        totals.entry(r.get_text(0)?).or_default().tokens = Some(TokenClasses {
            uncached_input: r.get_i64(1)?,
            cached_input: r.get_i64(2)?,
            cache_creation: r.get_i64(3)?,
            output: r.get_i64(4)?,
        });
    }
    Ok(())
}

/// …and in money: the same non-superseded, open-valid predicate `session_cost`
/// applies, grouped the same way. Tokens without a priced bill stay tokens, so
/// a session with no fact rows carries no `cost_micros` rather than a zero.
async fn read_cost_totals(
    store: &SqliteStore,
    totals: &mut HashMap<String, SessionTotals>,
) -> Result<()> {
    for r in store
        .query(
            "SELECT t.session_id, COALESCE(SUM(cf.amount_micros),0) \
             FROM cost_fact cf \
             JOIN model_call mc ON mc.call_id = cf.call_id \
             JOIN turn t ON t.turn_id = mc.turn_id \
             WHERE cf.valid_to IS NULL \
             AND NOT EXISTS (SELECT 1 FROM cost_fact s WHERE s.supersedes_id = cf.cost_fact_id) \
             GROUP BY t.session_id",
            &[],
        )
        .await?
    {
        totals.entry(r.get_text(0)?).or_default().cost_micros = Some(r.get_i64(1)?);
    }
    Ok(())
}
