//! Real ingest over a real member: the hook path writes the cache and the rows
//! the loader reads, so no test fabricates evidence the production path cannot.
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use rift_core::{RepoId, Result};
use rift_shadow::hooks::run_state::RunRoot;
use rift_shadow::hooks::session_cache::{self, SessionCache};
use rift_shadow::hooks::{AdapterRegistry, AgentAdapter, AgentId, CompatNotes, RawHook};
use rift_shadow::ids;
use rift_shadow::IngestOutcome;
use rift_store::{CanonicalStore, CanonicalTable, Row, SqliteStore, Value};
use serde_json::Value as Json;

use super::{load_evidence, Candidate, Member, Pass, Reason, Verdict};
use crate::agent_queue::{BoundaryEdge, BoundarySnapshot};
use crate::capture_recovery::hold::{admit_own_hold, OwnHold};
use crate::capture_recovery::plan::Plan;
use crate::capture_recovery::test_support::Fixture;

/// A member with its hold records, a migrated in-memory projection and the
/// built-in adapters: everything one eligibility pass reads.
pub(crate) struct Rig {
    pub(crate) fixture: Fixture,
    pub(crate) store: Counting,
    pub(crate) registry: AdapterRegistry,
    pub(crate) rt: tokio::runtime::Runtime,
    pub(crate) repo_id: RepoId,
    pub(crate) sessions_dir: std::path::PathBuf,
}

/// The real store behind a query counter, so a test reads the actual call
/// and row counts a pass cost rather than the pass's own tally alone.
pub(crate) struct Counting {
    inner: SqliteStore,
    calls: AtomicU32,
    rows: AtomicU32,
    sql: Mutex<Vec<String>>,
}

type Boxed<'t, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 't>>;

impl Counting {
    pub(crate) fn new(inner: SqliteStore) -> Self {
        Self {
            inner,
            calls: AtomicU32::new(0),
            rows: AtomicU32::new(0),
            sql: Mutex::new(Vec::new()),
        }
    }
    pub(crate) fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
    pub(crate) fn rows(&self) -> u32 {
        self.rows.load(Ordering::SeqCst)
    }
    pub(crate) fn reset(&self) {
        self.calls.store(0, Ordering::SeqCst);
        self.rows.store(0, Ordering::SeqCst);
        self.sql.lock().unwrap().clear();
    }
    pub(crate) fn queries_starting_with(&self, prefix: &str) -> usize {
        self.sql
            .lock()
            .unwrap()
            .iter()
            .filter(|sql| sql.starts_with(prefix))
            .count()
    }
}

// Hand-desugared `#[async_trait]` (the bin carries no `async-trait`
// dependency and this slice adds none): one early-bound `'t` the borrows
// outlive, exactly the shape the trait's macro expansion declares.
impl CanonicalStore for Counting {
    fn register_canonical_table<'l0, 'l1, 't>(
        &'l0 self,
        table: &'l1 CanonicalTable,
    ) -> Boxed<'t, ()>
    where
        'l0: 't,
        'l1: 't,
        Self: 't,
    {
        Box::pin(self.inner.register_canonical_table(table))
    }
    fn insert<'l0, 'l1, 'l2, 'l3, 'l4, 't>(
        &'l0 self,
        table: &'l1 str,
        columns: &'l2 [&'l3 str],
        values: &'l4 [Value],
    ) -> Boxed<'t, ()>
    where
        'l0: 't,
        'l1: 't,
        'l2: 't,
        'l3: 't,
        'l4: 't,
        Self: 't,
    {
        Box::pin(self.inner.insert(table, columns, values))
    }
    fn insert_or_ignore<'l0, 'l1, 'l2, 'l3, 'l4, 't>(
        &'l0 self,
        table: &'l1 str,
        columns: &'l2 [&'l3 str],
        values: &'l4 [Value],
    ) -> Boxed<'t, bool>
    where
        'l0: 't,
        'l1: 't,
        'l2: 't,
        'l3: 't,
        'l4: 't,
        Self: 't,
    {
        Box::pin(self.inner.insert_or_ignore(table, columns, values))
    }
    fn count<'l0, 'l1, 't>(&'l0 self, table: &'l1 str) -> Boxed<'t, i64>
    where
        'l0: 't,
        'l1: 't,
        Self: 't,
    {
        Box::pin(self.inner.count(table))
    }
    fn query<'l0, 'l1, 'l2, 't>(
        &'l0 self,
        sql: &'l1 str,
        params: &'l2 [Value],
    ) -> Boxed<'t, Vec<Row>>
    where
        'l0: 't,
        'l1: 't,
        'l2: 't,
        Self: 't,
    {
        Box::pin(async move {
            let rows = self.inner.query(sql, params).await?;
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.rows
                .fetch_add(u32::try_from(rows.len()).unwrap(), Ordering::SeqCst);
            self.sql.lock().unwrap().push(sql.to_owned());
            Ok(rows)
        })
    }
    fn query_opt_i64<'l0, 'l1, 'l2, 't>(
        &'l0 self,
        sql: &'l1 str,
        params: &'l2 [Value],
    ) -> Boxed<'t, Option<i64>>
    where
        'l0: 't,
        'l1: 't,
        'l2: 't,
        Self: 't,
    {
        Box::pin(self.inner.query_opt_i64(sql, params))
    }
}

impl Rig {
    pub(crate) fn new() -> Self {
        Self::over(Fixture::new())
    }

    pub(crate) fn over(fixture: Fixture) -> Self {
        let store = SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&store, &rift_store::default_migrations()).unwrap();
        let plan = Plan::decode(&fixture.plan_bytes).unwrap();
        let repo_id = plan.repo_id().clone();
        let sessions_dir = RunRoot::Sessions.under(&fixture.member);
        Self {
            fixture,
            store: Counting::new(store),
            registry: AdapterRegistry::with_builtin(),
            rt: tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap(),
            repo_id,
            sessions_dir,
        }
    }

    pub(crate) fn admit(&self) -> OwnHold<'_> {
        admit_own_hold(&self.fixture.owner, &self.fixture.evidence, "fixture-op").unwrap()
    }

    /// The member as the plan states it, which is what a truthful caller asserts.
    pub(crate) fn member(&self) -> Member {
        Member::new(
            self.repo_id.clone(),
            self.fixture.owner.git_dir().to_str().unwrap(),
            self.fixture.work_tree.to_str().unwrap(),
        )
        .unwrap()
    }

    /// One native hook through the wire builder and the real ingest, into the
    /// member's current sessions tree. `at` is both the hook's instant and the
    /// receipt's clock; a caller wanting them apart passes a `RawHook`.
    pub(crate) fn ingest(&self, agent: &str, verb: &str, payload: &Json, at: i64) -> IngestOutcome {
        let raw = rift_shadow::hooks::build_raw_hook(
            agent,
            verb,
            &serde_json::to_vec(payload).unwrap(),
            at,
        )
        .unwrap();
        self.ingest_raw(&raw, agent, at + 5)
    }

    pub(crate) fn ingest_raw(&self, raw: &RawHook, label: &str, now_ms: i64) -> IngestOutcome {
        self.rt
            .block_on(rift_shadow::bridge::ingest_hook(
                raw,
                &self.registry,
                &self.store,
                &self.sessions_dir,
                label,
                self.repo_id.as_str(),
                now_ms,
                None,
            ))
            .unwrap()
    }

    pub(crate) fn snapshot(
        agent: &str,
        native: &str,
        ordinal: u32,
        edge: BoundaryEdge,
    ) -> BoundarySnapshot {
        BoundarySnapshot {
            session_id: native.into(),
            agent: agent.into(),
            ordinal,
            boundary: edge,
            head_oid: None,
            captured_at_ms: 777_000,
        }
    }

    pub(crate) fn pass(
        &self,
        hold: &OwnHold<'_>,
        jobs: &[BoundarySnapshot],
    ) -> std::result::Result<Pass, Reason> {
        self.pass_with(hold, &self.registry, jobs)
    }

    pub(crate) fn pass_with(
        &self,
        hold: &OwnHold<'_>,
        registry: &AdapterRegistry,
        jobs: &[BoundarySnapshot],
    ) -> std::result::Result<Pass, Reason> {
        let ready = hold
            .sessions_namespace()
            .map_err(|_| Reason::SessionsUnavailable)?;
        let ids: Vec<String> = jobs.iter().map(context_id).collect();
        let candidates: Vec<Candidate<'_>> = jobs
            .iter()
            .zip(&ids)
            .map(|(snapshot, context_id)| Candidate {
                snapshot,
                context_id,
            })
            .collect();
        self.rt.block_on(load_evidence(
            &self.store,
            &ready,
            registry,
            &self.member(),
            &candidates,
        ))
    }

    /// A pass asserting `member`, which need not be this rig's own.
    pub(crate) fn pass_as(
        &self,
        hold: &OwnHold<'_>,
        member: &Member,
        jobs: &[BoundarySnapshot],
    ) -> std::result::Result<Pass, Reason> {
        let ready = hold
            .sessions_namespace()
            .map_err(|_| Reason::SessionsUnavailable)?;
        let ids: Vec<String> = jobs.iter().map(context_id).collect();
        let candidates: Vec<Candidate<'_>> = jobs
            .iter()
            .zip(&ids)
            .map(|(snapshot, context_id)| Candidate {
                snapshot,
                context_id,
            })
            .collect();
        self.rt.block_on(load_evidence(
            &self.store,
            &ready,
            &self.registry,
            member,
            &candidates,
        ))
    }

    /// A cache written as the hook path writes it: the member's own work tree
    /// is stamped from the directory, exactly as production fills it.
    pub(crate) fn write_cache(&self, cache: &SessionCache) {
        session_cache::write_cache(&self.sessions_dir, cache).unwrap();
    }

    /// An active cache at `ordinal`, open or closed.
    pub(crate) fn active_cache(
        agent: &str,
        native: &str,
        ordinal: u32,
        open: bool,
    ) -> SessionCache {
        let mut cache = SessionCache::new(native, agent, "active", 5000);
        cache.current_turn_ordinal = ordinal;
        cache.turn_open = open;
        cache
    }

    /// The identity, session and turn rows a projected turn leaves, under this
    /// rig's repository unless `repo` says otherwise.
    pub(crate) fn seed_turn(&self, agent: &str, native: &str, ordinal: u32, repo: Option<&str>) {
        let session_row = ids::session_row_id(agent, native);
        self.rt.block_on(async {
            rift_store::record_capture_session_identity(
                &self.store,
                &rift_store::CaptureSessionIdentityRow {
                    session_id: session_row.clone(),
                    agent: agent.into(),
                    native_session_id: native.into(),
                    schema_version: 1,
                },
            )
            .await
            .unwrap();
            rift_store::record_session(
                &self.store,
                &rift_store::SessionRow {
                    session_id: session_row.clone(),
                    agent: agent.into(),
                    model_primary: String::new(),
                    repo_id: repo.unwrap_or(self.repo_id.as_str()).into(),
                    started_at: 1,
                    schema_version: 1,
                },
            )
            .await
            .unwrap();
            rift_store::record_turn(
                &self.store,
                &rift_store::TurnRow {
                    turn_id: ids::turn_row_id(&session_row, ordinal),
                    session_id: session_row,
                    ordinal: i64::from(ordinal),
                    started_at: 1,
                    schema_version: 1,
                },
            )
            .await
            .unwrap();
        });
    }

    /// One captured event row at `(native, ordinal)` with the stated kind,
    /// trust and event ordinal, keyed as `verb` fired by `firing`.
    pub(crate) fn seed_event(&self, event: &SeededEvent<'_>) {
        let key = rift_agent::idempotency_key(
            &AgentId::new(event.firing),
            event.native,
            event.verb,
            event.ordinal,
            event.event_ordinal,
            &rift_core::Sha256Id::parse_hex(event.sha).unwrap(),
        );
        self.rt
            .block_on(rift_store::record_captured_event(
                &self.store,
                &rift_store::CapturedEventRow {
                    idempotency_key: key.to_hex(),
                    session_id: event.native.into(),
                    turn_ordinal: i64::from(event.ordinal),
                    event_ordinal: i64::from(event.event_ordinal),
                    kind: event.kind.into(),
                    trust: event.trust.into(),
                    payload_sha256: event.sha.into(),
                    occurred_at_ms: 9,
                    schema_version: 1,
                },
            ))
            .unwrap();
    }

    /// One hook receipt filed under `owner` for `verb`, as the ingest files it.
    pub(crate) fn seed_receipt(
        &self,
        owner: &str,
        verb: &str,
        native: &str,
        ordinal: u32,
        sha: &str,
        outcome: &str,
    ) {
        self.rt
            .block_on(rift_store::record_agent_hook_event(
                &self.store,
                &rift_store::AgentHookEventRow {
                    hook_event_id: rift_shadow::bridge::hook_receipt_id(
                        owner,
                        verb,
                        native,
                        &ordinal.to_string(),
                        sha,
                    ),
                    agent: owner.into(),
                    verb: verb.into(),
                    session_id: Some(native.into()),
                    received_at: 11,
                    outcome: outcome.into(),
                    schema_version: 1,
                },
            ))
            .unwrap();
    }

    pub(crate) fn verdict(&self, hold: &OwnHold<'_>, job: &BoundarySnapshot) -> Verdict {
        let pass = self.pass(hold, std::slice::from_ref(job)).unwrap();
        pass.classified.into_iter().next().unwrap().verdict
    }
}

/// A captured event to seed: the stored coordinates and the verb and agent its
/// key is derived from, which need not agree with its kind.
pub(crate) struct SeededEvent<'a> {
    pub(crate) firing: &'a str,
    pub(crate) native: &'a str,
    pub(crate) verb: &'a str,
    pub(crate) ordinal: u32,
    pub(crate) event_ordinal: u32,
    pub(crate) kind: &'a str,
    pub(crate) trust: &'a str,
    pub(crate) sha: &'a str,
}

/// The payload hash the ingest derives for `payload`.
pub(crate) fn payload_sha(payload: &Json) -> String {
    rift_shadow::sha256_hex(&[serde_json::to_vec(payload).unwrap().as_slice()])
}

/// The delivery's context identity, as the queue derives it for this job.
pub(crate) fn context_id(job: &BoundarySnapshot) -> String {
    let value = serde_json::json!({
        "schema_version": 1,
        "work_tree": "/fixture",
        "queued_at_ms": job.captured_at_ms,
        "kind": {"boundary_snapshot": serde_json::to_value(job).unwrap()},
    });
    crate::capture_queue::delivery::context_id(&value).unwrap()
}

/// A nested payload whose keys are not in lexical order, carrying the native
/// session under the field the agent's wire uses.
pub(crate) fn payload(session_field: &str, native: &str) -> Json {
    serde_json::json!({
        session_field: native,
        "zeta": {"b": [3, 1, 2], "a": {"y": true, "x": null}},
        "alpha": "last",
    })
}

/// An adapter for `agent` that declares exactly `verbs` and maps nothing: what
/// the receipt preflight sees, with no event history behind it.
pub(crate) struct Declaring {
    agent: AgentId,
    verbs: &'static [&'static str],
}

impl Declaring {
    pub(crate) fn registry(agent: &str, verbs: Vec<String>) -> AdapterRegistry {
        let leaked: Vec<&'static str> = verbs
            .into_iter()
            .map(|verb| &*Box::leak(verb.into_boxed_str()))
            .collect();
        let mut registry = AdapterRegistry::default();
        registry.register(Box::new(Self {
            agent: AgentId::new(agent),
            verbs: Box::leak(leaked.into_boxed_slice()),
        }));
        registry
    }
}

impl AgentAdapter for Declaring {
    fn agent_id(&self) -> AgentId {
        self.agent.clone()
    }
    fn map(
        &self,
        verb: &str,
        _native: &Json,
    ) -> std::result::Result<
        Vec<rift_shadow::hooks::CanonicalEvent>,
        rift_shadow::hooks::AdapterError,
    > {
        Err(rift_shadow::hooks::AdapterError::UnknownVerb(
            verb.to_owned(),
        ))
    }
    fn verbs(&self) -> &'static [&'static str] {
        self.verbs
    }
    fn compat_notes(&self) -> CompatNotes {
        CompatNotes {
            agent: self.agent.clone(),
            lifecycle_gaps: &[],
            surfaces_token_usage: false,
            best_effort: true,
            note: "test adapter: declares verbs, maps nothing",
        }
    }
}
