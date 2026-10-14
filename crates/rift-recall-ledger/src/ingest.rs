//! Ingest: substrate artifacts → file-read facts and the recall graph.
//!
//! Two layers share this module:
//! * [`read_facts_for`]/[`read_facts_all`] join `tool_calls.jsonl` (path@hash
//!   read facts) with `token_estimates` into [`crate::FileRead`]s — the in-memory
//!   metric slice.
//! * [`ingest`] projects a checkpoint's artifacts into the durable recall graph
//!   (`graph_node`/`graph_edge`/`n_*`), content-addressed and idempotent.
//!   The keystone is **FileRead extraction**: a tool
//!   call that read a file records the file's `path@hash` *as captured at read
//!   time*, so a later turn can be told it is `known_unchanged`.

mod resume;
mod serve_audit;
mod window_journal;

use std::collections::{BTreeMap, BTreeSet};

use rift_core::Result;
use rift_recall_substrate::parse::{self, ParseIssue};
use rift_recall_substrate::{CheckpointShard, SubstrateReader};
use rift_recall_types::checkpoint::{CheckpointMetadata, CostEstimate, ToolCallRecord};
use rift_store::{CanonicalStore, Value};

use crate::graph::{file_read_id, file_revision_id, sha256_id, Built, INGEST_TS};
use crate::FileRead;
use resume::{record_resume_points, resume_frontier, ResumeFrontier};

/// The read facts of a walk, with the paths of the ones the graph projects.
#[derive(Debug, Default)]
pub(crate) struct ReadFacts {
    /// Every read-shaped call carrying a path and a content hash, in walk
    /// order.
    pub(crate) reads: Vec<FileRead>,
    /// The paths of the reads whose tool call carries an identity — exactly
    /// the reads the graph projects. A pass that reads no shard recovers its
    /// paths from the projection, so both kinds of pass must name the same set:
    /// a path seen only in an anonymous call would otherwise be hashed by one
    /// and not the other.
    pub(crate) projected_paths: BTreeSet<String>,
    /// Parse issues met along the way.
    pub(crate) issues: Vec<ParseIssue>,
}

/// Extract the file-read facts from a single checkpoint's artifacts, plus any
/// parse issues. Only read-shaped tool calls carrying both a path and a content
/// hash become `FileRead`s; tokens are taken inline or joined from
/// `token_estimates` by `tool_use_id` (defaulting to 0).
///
/// # Errors
/// Propagates [`SubstrateReader::read_artifact`] read errors.
pub fn read_facts_for(
    reader: &impl SubstrateReader,
    checkpoint_id: &str,
) -> Result<(Vec<FileRead>, Vec<ParseIssue>)> {
    let mut facts = ReadFacts::default();
    collect_read_facts(reader, checkpoint_id, &mut facts)?;
    Ok((facts.reads, facts.issues))
}

/// Extract file-read facts across every checkpoint of a reader, in walk order.
///
/// # Errors
/// Propagates substrate read errors.
pub fn read_facts_all(reader: &impl SubstrateReader) -> Result<(Vec<FileRead>, Vec<ParseIssue>)> {
    let facts = read_facts_of(reader, &reader.checkpoints()?)?;
    Ok((facts.reads, facts.issues))
}

/// The read facts of `shards`, in their order. Taking the shards rather than
/// walking them again lets a caller read the very snapshot its ingest plan
/// holds.
///
/// # Errors
/// Propagates substrate read errors.
pub(crate) fn read_facts_of(
    reader: &impl SubstrateReader,
    shards: &[CheckpointShard],
) -> Result<ReadFacts> {
    let mut facts = ReadFacts::default();
    for shard in shards {
        collect_read_facts(reader, &shard.checkpoint_id, &mut facts)?;
    }
    Ok(facts)
}

/// The distinct repo-relative paths that appear in `reads` (the paths the
/// worktree hasher needs for the avoidable-context metric).
#[must_use]
pub fn read_paths(reads: &[FileRead]) -> std::collections::BTreeSet<String> {
    reads.iter().map(|r| r.path.clone()).collect()
}

/// The paths the projection holds a read for: what [`ReadFacts::projected_paths`]
/// names for the walk the projection was built from.
///
/// # Errors
/// Propagates store read errors.
pub(crate) async fn projected_read_paths(store: &impl CanonicalStore) -> Result<BTreeSet<String>> {
    let rows = store
        .query(
            "SELECT DISTINCT r.path FROM n_file_revision r \
             JOIN n_file_read k ON k.file_revision = r.node_id",
            &[],
        )
        .await?;
    Ok(rows.iter().filter_map(|r| r.get_text(0).ok()).collect())
}

/// Append one checkpoint's read facts to `facts`.
fn collect_read_facts(
    reader: &impl SubstrateReader,
    checkpoint_id: &str,
    facts: &mut ReadFacts,
) -> Result<()> {
    let tool_calls = match reader.read_artifact(checkpoint_id, "tool_calls.jsonl")? {
        Some(bytes) => {
            let out = parse::parse_tool_calls(&bytes, checkpoint_id);
            facts.issues.extend(out.issues);
            out.value
        }
        None => Vec::new(),
    };
    let tokens = match reader.read_artifact(checkpoint_id, "token_estimates.json")? {
        Some(bytes) => {
            let out = parse::parse_token_estimates(&bytes, checkpoint_id);
            facts.issues.extend(out.issues);
            out.value
        }
        None => Vec::new(),
    };
    let tok_by_id: BTreeMap<&str, u32> = tokens
        .iter()
        .map(|t| (t.tool_use_id.as_str(), t.input_tokens))
        .collect();

    for tc in &tool_calls {
        if !tc.is_read() {
            continue;
        }
        let (Some(path), Some(hash)) = (tc.path.as_ref(), tc.content_sha256.as_ref()) else {
            continue;
        };
        let input_tokens = tc
            .input_tokens
            .or_else(|| tok_by_id.get(tc.tool_use_id.as_str()).copied())
            .unwrap_or(0);
        // The graph's rule, kept in step with `push_tool_call_rows`: a call
        // with no identity becomes no node, so its path is not a projected one.
        if !tc.tool_use_id.is_empty() {
            facts.projected_paths.insert(path.clone());
        }
        facts.reads.push(FileRead {
            path: path.clone(),
            content_sha256: hash.clone(),
            input_tokens,
        });
    }
    Ok(())
}

// =========================================================================
// Graph ingest
// =========================================================================

/// What an ingest run produced (counts + skip-and-report issues for `doctor`).
#[derive(Debug, Default, Clone)]
pub struct IngestReport {
    /// Checkpoints walked (processed + skipped).
    pub checkpoints: usize,
    /// Checkpoints skipped as already projected (incremental ingest — their
    /// artifacts were never re-read).
    pub skipped: usize,
    /// Node rows built.
    pub nodes: usize,
    /// Edge rows built.
    pub edges: usize,
    /// Changed hunks persisted between consecutive revisions.
    pub hunks: usize,
    /// Parse issues encountered (never aborts the batch).
    pub issues: Vec<ParseIssue>,
    /// Dialect streams this run resumed from a [`rift_store::StreamCursor`]
    /// (the one shared resume path).
    pub resumed: usize,
    /// Checkpoints whose containment rows were written by the
    /// backfill rather than by their own ingest — the count is non-zero on the
    /// first pass after this projection learns about branch containment, and
    /// on every pass for a shard whose header records no anchor commit.
    pub containment_backfilled: usize,
    /// Dialect streams that took a **full pass** instead: no prior watermark
    /// (the cold case), or a recorded tip the walk no longer holds at its
    /// recorded position (the stream was rewritten under this index). Both are
    /// safe — the content-addressed seen-set is the backstop — but only one of
    /// them is normal, which is why the count is reported rather than
    /// swallowed.
    pub full_pass: usize,
    /// The full passes taken because the stream was rewritten under this
    /// index: the recorded tip is gone, or its recorded position now holds
    /// another checkpoint. `full_pass - rewritten` is the cold starts.
    pub rewritten: usize,
    /// Frozen window rows a window inherited from the window that started
    /// it: valid, read, and not projected, because the window-row relation's
    /// kinds are sealed by its migration. Counted so a rebuild says how many
    /// valid rows its projection does not hold.
    pub window_rows_inherited: usize,
    /// Frozen window rows that record a decline resting on another live
    /// window of the repository: valid, read, and not projected, for the same
    /// reason, until a reader of the projection needs them.
    pub window_rows_family_held: usize,
    /// Frozen window rows that record lines a terminal result was found to
    /// carry: valid, read, and not projected, for the same reason.
    pub window_rows_matched: usize,
}

/// Caller-supplied context for a production ingest run (the completeness
/// completeness). Everything here is optional — a default runs exactly like the
/// bare [`ingest`] — but a production caller passes the identity plane's
/// session→developer join so the developer scope axis is populated instead of
/// always-null.
#[derive(Debug, Default, Clone)]
pub struct IngestOptions {
    /// `session_id → hashed developer id` (from the capture projection's
    /// `session_binding` rows — never a raw name/email).
    pub developer_by_session: BTreeMap<String, String>,
    /// Fallback hashed developer id when no session binding matches (the
    /// current developer on a single-operator machine).
    pub default_developer: Option<String>,
    /// Fallback token estimates keyed by `content_sha256`, consulted
    /// only when a read fact carries no inline or `token_estimates.json`
    /// figure — the caller resolves them from bytes it can still reach and
    /// verify (worktree match, blob-oid map, capture projection). A hash with
    /// no entry ingests as 0 tokens plus a counted
    /// `IssueKind::TokenUnresolved` — measured or absent, never a guess.
    pub token_estimate_by_hash: BTreeMap<String, u32>,
}

/// What every read row of one checkpoint shares: the shard, whose ordinal
/// orders its reads; the primary session, which made the calls; and the two
/// token sources a read falls back to when it carries no inline figure.
struct ReadContext<'a> {
    shard: &'a CheckpointShard,
    primary: &'a str,
    /// The shard's `token_estimates.json`, keyed by `tool_use_id`.
    tok_by_id: BTreeMap<&'a str, u32>,
    /// The caller's resolved-bytes fallback, keyed by `content_sha256`.
    token_estimate_by_hash: &'a BTreeMap<String, u32>,
}

impl IngestOptions {
    /// The hashed developer id for a checkpoint: an explicit id captured in its
    /// metadata wins, then the primary session's binding, then the fallback.
    fn developer_for<'a>(
        &'a self,
        meta: &'a CheckpointMetadata,
        sessions: &[String],
    ) -> Option<&'a str> {
        meta.developer
            .as_deref()
            .or_else(|| {
                sessions
                    .iter()
                    .find_map(|s| self.developer_by_session.get(s))
                    .map(String::as_str)
            })
            .or(self.default_developer.as_deref())
    }
}

impl ReadContext<'_> {
    /// Token precedence: the fact's inline figure, then the shard's
    /// `token_estimates.json`, then the caller's resolved-bytes fallback.
    /// Still nothing ⇒ `None`, which the read ingests as 0 and counts.
    fn resolved_tokens(&self, tc: &ToolCallRecord, hash: &str) -> Option<u32> {
        tc.input_tokens
            .or_else(|| self.tok_by_id.get(tc.tool_use_id.as_str()).copied())
            .or_else(|| self.token_estimate_by_hash.get(hash).copied())
    }

    /// The read's position across the whole walk: the shard's ordinal, then
    /// the call's index within the shard — saturating, never wrapping.
    fn read_ordinal(&self, read_idx: usize) -> i64 {
        i64::try_from(
            self.shard
                .ordinal
                .saturating_mul(1_000_000)
                .saturating_add(read_idx as u64),
        )
        .unwrap_or(i64::MAX)
    }
}

/// Screen a shard header's `commit` before it becomes an append-only row.
///
/// The bytes are untrusted — a shard can arrive in a bundle from another
/// machine — and this value is compared against a real ancestry set, so the
/// only shapes worth storing are hex object ids. Deliberately not pinned to 40
/// or 64 characters: git is mid-transition between hash algorithms and the
/// fixtures this projection is built from carry short ids. An id that is not a
/// real commit simply never matches an ancestry set, which is the safe
/// direction; an id carrying a newline or a megabyte is refused outright.
fn screened_commit_oid(commit: Option<&str>) -> Option<String> {
    let oid = commit?.trim();
    (!oid.is_empty() && oid.len() <= 64 && oid.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| oid.to_ascii_lowercase())
}

/// Push whichever of the two containment rows this header supports.
/// Neither is invented: a header with no `commit` gets no anchor row, and a
/// shard with no `worktree` member gets no member row.
fn push_containment_rows(b: &mut Built, cid: &str, meta: &CheckpointMetadata) {
    if let Some(oid) = screened_commit_oid(meta.commit.as_deref()) {
        b.checkpoint_commit(cid, &oid);
    }
    if let Some(member) = meta.worktree.as_ref() {
        b.checkpoint_worktree(cid, member);
    }
}

/// Build the graph rows for one checkpoint — **pure** (reads artifacts, no DB
/// writes). The cascade: Checkpoint → Sessions → ToolCalls → **FileRead
/// extraction** → TokenUsage, with the typed edges. Each step below pushes its
/// rows in cascade order, and the artifacts are read in the order their parse
/// issues are reported. Beside the rows come the counts of inherited window
/// runs, family-held declines and matched lines it read and did not project.
fn build_rows(
    reader: &impl SubstrateReader,
    shard: &CheckpointShard,
    opts: &IngestOptions,
) -> Result<(Built, (usize, usize, usize))> {
    let mut b = Built::default();
    let cid = shard.checkpoint_id.as_str();

    let meta = read_parsed(
        reader,
        cid,
        "metadata.json",
        parse::parse_metadata,
        &mut b.issues,
    )?
    .unwrap_or_default();
    let sessions = checkpoint_sessions(cid, &meta);
    let primary = sessions.first().cloned().unwrap_or_else(|| cid.to_string());
    let developer = opts.developer_for(&meta, &sessions);
    push_checkpoint_rows(&mut b, shard, &meta, developer);
    push_session_rows(&mut b, cid, &sessions, opts, developer);

    // --- Tool calls → FileRead extraction (the keystone) ---
    let tool_calls = read_parsed(
        reader,
        cid,
        "tool_calls.jsonl",
        parse::parse_tool_calls,
        &mut b.issues,
    )?
    .unwrap_or_default();
    let tokens = read_parsed(
        reader,
        cid,
        "token_estimates.json",
        parse::parse_token_estimates,
        &mut b.issues,
    )?
    .unwrap_or_default();
    let ctx = ReadContext {
        shard,
        primary: &primary,
        tok_by_id: tokens
            .iter()
            .map(|t| (t.tool_use_id.as_str(), t.input_tokens))
            .collect(),
        token_estimate_by_hash: &opts.token_estimate_by_hash,
    };
    let unresolved_tokens = push_tool_call_rows(&mut b, &ctx, &tool_calls);
    push_unresolved_tokens_issue(&mut b, cid, unresolved_tokens);

    // --- TokenUsage from the per-checkpoint cost estimate ---
    let cost = read_parsed(
        reader,
        cid,
        "cost_estimate.json",
        parse::parse_cost_estimate,
        &mut b.issues,
    )?;
    if let Some(cost) = cost {
        push_token_usage_rows(&mut b, cid, &primary, &cost);
    }
    b.serves = serve_audit::read_serves(reader, cid, &mut b.issues)?;
    let window_rows = window_journal::read_window_rows(reader, cid, &mut b.issues)?;
    b.inserts.extend(window_rows.inserts);
    Ok((
        b,
        (
            window_rows.inherited,
            window_rows.family_held,
            window_rows.matched,
        ),
    ))
}

/// Read one artifact of a checkpoint and parse it, appending its parse issues
/// to `issues`. An absent artifact is `None` — absence is not an issue, and
/// the caller decides whether it means "empty" or "no rows at all".
fn read_parsed<T>(
    reader: &impl SubstrateReader,
    cid: &str,
    artifact: &str,
    parser: fn(&[u8], &str) -> parse::Outcome<T>,
    issues: &mut Vec<ParseIssue>,
) -> Result<Option<T>> {
    let Some(bytes) = reader.read_artifact(cid, artifact)? else {
        return Ok(None);
    };
    let outcome = parser(&bytes, cid);
    issues.extend(outcome.issues);
    Ok(Some(outcome.value))
}

/// The checkpoint's sessions, or one synthesized session keyed to the
/// checkpoint when its header names none — every tool call needs a session to
/// hang from.
fn checkpoint_sessions(cid: &str, meta: &CheckpointMetadata) -> Vec<String> {
    if meta.sessions.is_empty() {
        vec![cid.to_string()]
    } else {
        meta.sessions.clone()
    }
}

/// Push the Checkpoint node, its payload (developer axis populated) and its
/// containment rows.
fn push_checkpoint_rows(
    b: &mut Built,
    shard: &CheckpointShard,
    meta: &CheckpointMetadata,
    developer: Option<&str>,
) {
    let cid = shard.checkpoint_id.as_str();
    b.node(cid, "Checkpoint", cid);
    b.checkpoint(
        cid,
        shard.dialect.label(),
        i64::try_from(shard.ordinal).unwrap_or(i64::MAX),
        shard.prev.as_deref(),
        meta.strategy.as_deref(),
        meta.merkle_root.as_deref(),
        developer,
    );
    // The containment facts, recall-side (the predicate never
    // joins a capture table): the anchor commit, and the member the shard was
    // archived in. Both absent-when-unknown, both screened before they enter a
    // relation nothing can correct.
    push_containment_rows(b, cid, meta);
}

/// Push one Session node per session, each carrying its own developer binding
/// (the checkpoint's developer when it has none), and its
/// Session→created→Checkpoint edge.
fn push_session_rows(
    b: &mut Built,
    cid: &str,
    sessions: &[String],
    opts: &IngestOptions,
    developer: Option<&str>,
) {
    for sid in sessions {
        b.node(sid, "Session", sid);
        b.session(
            sid,
            opts.developer_by_session
                .get(sid)
                .map(String::as_str)
                .or(developer),
        );
        b.edge(sid, cid, "created"); // Session→created→Checkpoint
    }
}

/// Push a ToolCall node, payload and Session→called→ToolCall edge for every
/// tool call with an identity, in capture order, and the FileRead rows of each
/// read among them. Returns how many reads resolved no token figure.
fn push_tool_call_rows(
    b: &mut Built,
    ctx: &ReadContext<'_>,
    tool_calls: &[ToolCallRecord],
) -> usize {
    let cid = ctx.shard.checkpoint_id.as_str();
    let mut unresolved_tokens = 0usize;
    for (i, tc) in tool_calls.iter().enumerate() {
        if tc.tool_use_id.is_empty() {
            continue; // a tool call needs an identity to be a node
        }
        let tc_id = tc.tool_use_id.as_str();
        b.node(tc_id, "ToolCall", tc_id);
        b.tool_call(tc_id, cid, tc_id, tc.tool_name.as_deref());
        b.edge(ctx.primary, tc_id, "called"); // Session→called→ToolCall

        if !tc.is_read() {
            continue;
        }
        let (Some(path), Some(hash)) = (tc.path.as_ref(), tc.content_sha256.as_ref()) else {
            continue;
        };
        if push_file_read_rows(b, ctx, i, tc, path, hash).is_none() {
            unresolved_tokens += 1;
        }
    }
    unresolved_tokens
}

/// Push the FileRevision and FileRead rows of one read — the keystone — and
/// return the token figure it resolved, `None` when no source had one.
fn push_file_read_rows(
    b: &mut Built,
    ctx: &ReadContext<'_>,
    read_idx: usize,
    tc: &ToolCallRecord,
    path: &str,
    hash: &str,
) -> Option<u32> {
    let cid = ctx.shard.checkpoint_id.as_str();
    let tc_id = tc.tool_use_id.as_str();
    // FileRevision: path@hash captured AT READ TIME (not the worktree's).
    let fr = file_revision_id(path, hash);
    b.node(&fr, "FileRevision", &fr);
    b.file_revision(&fr, path, hash, hash, 0);
    // FileRead: the irreducible (tool_call, revision, range) triple.
    let resolved = ctx.resolved_tokens(tc, hash);
    let input_tokens = i64::from(resolved.unwrap_or(0));
    let read_ordinal = ctx.read_ordinal(read_idx);
    let rid = file_read_id(tc_id, &fr);
    b.node(&rid, "FileRead", &rid);
    b.file_read(
        &rid,
        cid,
        tc_id,
        &fr,
        input_tokens,
        read_ordinal,
        None,
        None,
    );
    b.edge(tc_id, &fr, "read"); // ToolCall→read→FileRevision
    b.lineage(&fr, "tool_calls.jsonl", "mechanical", None);
    resolved
}

/// Summarize a checkpoint's token-less reads into ONE issue — never a guess,
/// and never an issue flood that drowns real corruption on a healthy substrate
/// that simply predates token capture.
fn push_unresolved_tokens_issue(b: &mut Built, cid: &str, unresolved_tokens: usize) {
    if unresolved_tokens == 0 {
        return;
    }
    b.issues.push(ParseIssue {
        checkpoint_id: cid.to_owned(),
        artifact: "tool_calls.jsonl",
        locator: parse::Locator::WholeFile,
        kind: parse::IssueKind::TokenUnresolved,
        detail: format!("{unresolved_tokens} read(s) with no resolvable token estimate"),
    });
}

/// Push the TokenUsage node for the checkpoint's four-class cost estimate and
/// its Session→spent→TokenUsage edge from the primary session.
fn push_token_usage_rows(b: &mut Built, cid: &str, primary: &str, cost: &CostEstimate) {
    let tu = sha256_id("rift.tu.v1", &[cid.as_bytes()]);
    b.node(&tu, "TokenUsage", &tu);
    b.token_usage(
        &tu,
        cid,
        cid,
        i64::try_from(cost.uncached_input_tokens).unwrap_or(i64::MAX),
        i64::try_from(cost.cached_input_tokens).unwrap_or(i64::MAX),
        i64::try_from(cost.cache_write_tokens).unwrap_or(i64::MAX),
        i64::try_from(cost.output_tokens).unwrap_or(i64::MAX),
    );
    b.edge(primary, &tu, "spent"); // Session→spent→TokenUsage
}

/// Apply built rows through the append-only DAO. The ONLY DB writer: every row
/// is `INSERT OR IGNORE`, so applying overlapping/duplicate `Built`s converges
/// (set-union). FKs are comment-only, so insert order is irrelevant. A
/// checkpoint's frozen serves go last, through the serve relations' one
/// appender, which is insert-or-ignore on their own keys.
///
/// # Errors
/// Propagates store errors (a duplicate primary key is **not** an error).
pub(crate) async fn apply_rows(store: &impl CanonicalStore, built: &Built) -> Result<()> {
    for ins in &built.inserts {
        store
            .insert_or_ignore(ins.table, &ins.columns, &ins.values)
            .await?;
    }
    serve_audit::append_serves(store, &built.serves).await
}

/// Ingest every checkpoint of `reader` into the recall graph `store` with
/// default [`IngestOptions`]. See [`ingest_with`].
///
/// # Errors
/// Propagates substrate read and store write errors.
pub async fn ingest(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
) -> Result<IngestReport> {
    ingest_with(store, reader, &IngestOptions::default()).await
}

/// Ingest `reader`'s checkpoints into the recall graph `store` — **incremental
/// and idempotent**. A checkpoint already projected (content-addressed presence
/// in `n_checkpoint`) is skipped without re-reading its artifacts, so re-ingest
/// is cheap, not just a set-union no-op; the run then appends a durable
/// `ingest_watermark` row per dialect (branch + last checkpoint id, with that
/// checkpoint's position in the walk beside it) and
/// persists changed hunks between consecutive revisions of each read path
/// (through the previously library-only [`crate::hunks::persist_hunks`]).
///
/// # Errors
/// Propagates substrate read and store write errors.
pub async fn ingest_with(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    opts: &IngestOptions,
) -> Result<IngestReport> {
    let plan = plan_ingest(store, reader).await?;
    ingest_planned(store, reader, plan, opts).await
}

/// Decide what an ingest will project, before it projects anything.
///
/// Split from [`ingest_planned`] so a caller can ask whether anything is new
/// and build its options only then, and so the answer and the ingest that acts
/// on it read one walk: a shard that lands in between is the next pass's,
/// never projected under options built for a pass that expected nothing.
///
/// # Errors
/// Propagates substrate read and store read errors.
pub(crate) async fn plan_ingest(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
) -> Result<IngestPlan> {
    // The durable seen-set: checkpoints are content-addressed, so presence in
    // the projection is proof of prior ingest (robust even if walk ordinals
    // reshuffle as new shards sort in between old ones).
    let seen: BTreeSet<String> = store
        .query("SELECT node_id FROM n_checkpoint", &[])
        .await?
        .iter()
        .filter_map(|r| r.get_text(0).ok())
        .collect();
    let mut report = IngestReport::default();
    // The resume frontier, taken through the ONE cursor path.
    let shards = reader.checkpoints()?;
    let frontier = resume_frontier(store, &shards, &mut report).await?;
    Ok(IngestPlan {
        shards,
        seen,
        frontier,
        report,
    })
}

/// Project what `plan` found new, then write the watermarks, the containment
/// backfill and the changed hunks. See [`ingest_with`].
///
/// # Errors
/// Propagates substrate read and store write errors.
pub(crate) async fn ingest_planned(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    plan: IngestPlan,
    opts: &IngestOptions,
) -> Result<IngestReport> {
    let mut report = plan.report.clone();
    // Last shard per dialect (by walk ordinal) → the durable watermark row.
    let mut tips: BTreeMap<&'static str, (u64, String)> = BTreeMap::new();
    for shard in &plan.shards {
        report.checkpoints += 1;
        let branch = shard.dialect.checkpoint_ref();
        let tip = tips.entry(branch).or_insert((0, String::new()));
        if shard.ordinal >= tip.0 {
            *tip = (shard.ordinal, shard.checkpoint_id.clone());
        }
        if !plan.projects(shard) {
            report.skipped += 1;
            continue;
        }
        let (built, (inherited, family_held, matched)) = build_rows(reader, shard, opts)?;
        report.window_rows_inherited += inherited;
        report.window_rows_family_held += family_held;
        report.window_rows_matched += matched;
        report.nodes += built.nodes;
        report.edges += built.edges;
        report.issues.extend(built.issues.iter().cloned());
        apply_rows(store, &built).await?;
    }

    // The containment facts for shards this index projected BEFORE the branch
    // axis existed (see `backfill_containment_rows`).
    report.containment_backfilled = backfill_containment_rows(store, reader, &plan.shards).await?;

    // Durable incremental-ingest evidence: one append-only row per dialect,
    // and the stream position its tip held.
    record_resume_points(store, &plan.shards, &tips).await?;

    // Changed hunks between consecutive revisions of each read path.
    report.hunks = persist_read_hunks(store, reader).await?;
    Ok(report)
}

/// Append one `ingest_watermark` row; a row already there is kept.
///
/// # Errors
/// Propagates store write errors.
pub(crate) async fn record_watermark(
    store: &impl CanonicalStore,
    watermark_id: &str,
    branch: &str,
    tip: &str,
) -> Result<()> {
    store
        .insert_or_ignore(
            "ingest_watermark",
            &[
                "watermark_id",
                "branch",
                "branch_tip",
                "ingested_at",
                "schema_version",
            ],
            &[
                Value::text(watermark_id),
                Value::text(branch),
                Value::text(tip),
                Value::Int(INGEST_TS),
                Value::Int(1),
            ],
        )
        .await?;
    Ok(())
}

/// Write the containment rows for checkpoints this index projected
/// **before** it knew to want them.
///
/// The incremental ingest is the reason this exists. A checkpoint already
/// present in `n_checkpoint` is skipped without re-reading a single artifact —
/// that is the watermark's whole point — so the shards a live projection
/// already holds would never acquire an anchor commit, and every one of them
/// would sit permanently out of branch scope: a serving surface that went dark
/// on upgrade, quietly, on exactly the repositories with the most history.
///
/// So: for each shard the projection already holds but has no anchor row for,
/// read `metadata.json` — one small blob, not the shard — and write whichever
/// rows its header supports. A shard that gets an anchor is never looked at
/// again. A shard whose header genuinely records no commit is re-examined once
/// per pass, and that is the honest price of refusing to invent a row: such a
/// shard has no anchor to find, is out of scope on every serve for exactly that
/// reason, and says so in the count this returns.
async fn backfill_containment_rows(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    shards: &[CheckpointShard],
) -> Result<usize> {
    let ids = |rows: &[rift_store::Row]| -> BTreeSet<String> {
        rows.iter().filter_map(|r| r.get_text(0).ok()).collect()
    };
    let projected = ids(&store.query("SELECT node_id FROM n_checkpoint", &[]).await?);
    let anchored = ids(&store
        .query("SELECT node_id FROM n_checkpoint_commit", &[])
        .await?);

    let mut backfilled = 0usize;
    for shard in shards {
        let cid = shard.checkpoint_id.as_str();
        if !projected.contains(cid) || anchored.contains(cid) {
            continue; // never ingested (the main loop owns it), or already anchored
        }
        let Some(bytes) = reader.read_artifact(cid, "metadata.json")? else {
            continue;
        };
        let meta = parse::parse_metadata(&bytes, cid).value;
        let mut b = Built::default();
        push_containment_rows(&mut b, cid, &meta);
        if b.inserts.is_empty() {
            continue;
        }
        apply_rows(store, &b).await?;
        backfilled += 1;
    }
    Ok(backfilled)
}

/// One ingest's reading of the walk: the shards, and which of them it will
/// project. Built by [`plan_ingest`], spent by [`ingest_planned`].
#[derive(Debug)]
pub(crate) struct IngestPlan {
    shards: Vec<CheckpointShard>,
    seen: BTreeSet<String>,
    frontier: ResumeFrontier,
    /// The resume counts the plan took, carried into the ingest's report.
    report: IngestReport,
}

impl IngestPlan {
    /// The walk this plan read.
    pub(crate) fn shards(&self) -> &[CheckpointShard] {
        &self.shards
    }

    /// Whether the ingest will project any checkpoint at all.
    pub(crate) fn projects_any(&self) -> bool {
        self.shards.iter().any(|shard| self.projects(shard))
    }

    /// Two independent reasons to skip a shard, in cost order. The cursor
    /// answers without touching the projection; the seen-set is the backstop
    /// that still catches a run which applied rows and died before its
    /// watermark.
    fn projects(&self, shard: &CheckpointShard) -> bool {
        self.frontier
            .admits(shard.dialect.checkpoint_ref(), &shard.checkpoint_id)
            && !self.seen.contains(&shard.checkpoint_id)
    }
}

/// Resolve the bytes behind a `content_sha256`: a direct substrate lookup first
/// (in-memory/test substrates key blobs by content hash), then the
/// content→blob-oid map for a real git object store. `None` = unresolvable —
/// the caller skips, never fabricates.
async fn resolve_content(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    content_sha256: &str,
) -> Option<Vec<u8>> {
    if let Ok(Some(bytes)) = reader.blob(content_sha256) {
        return Some(bytes);
    }
    if let Ok(Some(oid)) = crate::blob_map::blob_oid_for(store, content_sha256).await {
        if let Ok(Some(bytes)) = reader.blob(&oid) {
            return Some(bytes);
        }
    }
    None
}

/// Persist changed hunks for every **consecutive distinct** revision pair of
/// each read path, in read order. Content-addressed and idempotent (a re-run
/// adds no rows). A pair whose bytes cannot be resolved (blob not in the map
/// yet) is skipped — recomputed on a later run once the map catches up.
///
/// # Errors
/// Propagates store read/write errors.
pub async fn persist_read_hunks(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
) -> Result<usize> {
    let rows = store
        .query(
            "SELECT fr.path, fr.content_sha256, r.checkpoint_id \
             FROM n_file_read r JOIN n_file_revision fr ON fr.node_id = r.file_revision \
             ORDER BY fr.path ASC, r.read_ordinal ASC, r.node_id ASC",
            &[],
        )
        .await?;
    let mut total = 0usize;
    let mut prev: Option<(String, String)> = None;
    for row in &rows {
        let path = row.get_text(0)?;
        let hash = row.get_text(1)?;
        let checkpoint = row.get_text(2)?;
        if let Some((prev_path, prev_hash)) = &prev {
            if *prev_path == path && *prev_hash != hash {
                let old = resolve_content(store, reader, prev_hash).await;
                let new = resolve_content(store, reader, &hash).await;
                if let (Some(old), Some(new)) = (old, new) {
                    let old_rev = file_revision_id(&path, prev_hash);
                    let new_rev = file_revision_id(&path, &hash);
                    total += crate::hunks::persist_hunks(
                        store,
                        &checkpoint,
                        &old_rev,
                        &new_rev,
                        &old,
                        &new,
                    )
                    .await?;
                }
            }
        }
        prev = Some((path, hash));
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate};
    use rift_store::{recall_migrations, SqliteStore};

    pub(super) fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(f)
    }

    pub(super) fn recall_store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&s, &recall_migrations()).unwrap();
        s
    }

    pub(super) fn shard(id: &str, ord: u64) -> CheckpointShard {
        CheckpointShard {
            checkpoint_id: id.into(),
            dialect: Dialect::Rift,
            prev: None,
            ordinal: ord,
        }
    }

    fn count(store: &SqliteStore, table: &str) -> i64 {
        block(store.count(table)).unwrap()
    }

    #[test]
    fn joins_tool_calls_with_token_estimates() {
        let tool_calls =
            b"{\"id\":\"t1\",\"name\":\"Read\",\"file_path\":\"a.rs\",\"sha256\":\"h1\"}\n";
        let tokens = b"{\"id\":\"t1\",\"input\":120}\n";
        let sub = MemorySubstrate::local()
            .with_artifact("ab12", "tool_calls.jsonl", tool_calls)
            .with_artifact("ab12", "token_estimates.json", tokens);
        let (reads, issues) = read_facts_for(&sub, "ab12").unwrap();
        assert!(issues.is_empty());
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].path, "a.rs");
        assert_eq!(reads[0].input_tokens, 120);
    }

    #[test]
    fn extracts_file_read_with_captured_hash() {
        let tool_calls =
            b"{\"id\":\"t1\",\"name\":\"Read\",\"file_path\":\"foo.rs\",\"sha256\":\"abc\"}\n";
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_artifact("c1", "tool_calls.jsonl", tool_calls);
        let store = recall_store();
        block(ingest(&store, &sub)).unwrap();

        // the FileRevision carries the CAPTURED hash
        let rows =
            block(store.query("SELECT path, content_sha256 FROM n_file_revision", &[])).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get_text(0).unwrap(), "foo.rs");
        assert_eq!(rows[0].get_text(1).unwrap(), "abc");
        // one file_read + a ToolCall→read→FileRevision edge
        assert_eq!(count(&store, "n_file_read"), 1);
        let read_edges =
            block(store.query("SELECT count(*) FROM graph_edge WHERE kind='read'", &[])).unwrap();
        assert_eq!(read_edges[0].get_i64(0).unwrap(), 1);
    }

    /// Token precedence: an inline figure wins over the caller's fallback map;
    /// a factless hash takes the fallback; a hash resolved nowhere ingests as
    /// 0 with a counted `TokenUnresolved` issue — never a guess.
    #[test]
    fn fallback_token_estimates_fill_only_factless_reads() {
        let tc = b"{\"id\":\"t1\",\"name\":\"Read\",\"file_path\":\"a.rs\",\"sha256\":\"h1\",\"input_tokens\":7}\n\
                   {\"id\":\"t2\",\"name\":\"Read\",\"file_path\":\"b.rs\",\"sha256\":\"h2\"}\n\
                   {\"id\":\"t3\",\"name\":\"Read\",\"file_path\":\"c.rs\",\"sha256\":\"h3\"}\n";
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_artifact("c1", "tool_calls.jsonl", tc);
        let store = recall_store();
        let opts = IngestOptions {
            token_estimate_by_hash: [("h1".to_string(), 999u32), ("h2".to_string(), 55u32)]
                .into_iter()
                .collect(),
            ..IngestOptions::default()
        };
        let report = block(ingest_with(&store, &sub, &opts)).unwrap();
        let rows = block(store.query(
            "SELECT f.input_tokens FROM n_file_read f ORDER BY f.read_ordinal ASC",
            &[],
        ))
        .unwrap();
        let tokens: Vec<i64> = rows.iter().map(|r| r.get_i64(0).unwrap()).collect();
        assert_eq!(
            tokens,
            vec![7, 55, 0],
            "inline wins; fallback fills; absent is 0"
        );
        let unresolved: Vec<_> = report
            .issues
            .iter()
            .filter(|i| i.kind == parse::IssueKind::TokenUnresolved)
            .collect();
        assert_eq!(unresolved.len(), 1, "one summarized issue per checkpoint");
        assert!(unresolved[0].detail.contains("1 read(s)"));
    }

    #[test]
    fn dedupes_same_file_across_checkpoints() {
        let tc1 =
            b"{\"id\":\"t1\",\"name\":\"Read\",\"file_path\":\"foo.rs\",\"sha256\":\"abc\"}\n";
        let tc2 =
            b"{\"id\":\"t2\",\"name\":\"Read\",\"file_path\":\"foo.rs\",\"sha256\":\"abc\"}\n";
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_shard(shard("c2", 2))
            .with_artifact("c1", "tool_calls.jsonl", tc1)
            .with_artifact("c2", "tool_calls.jsonl", tc2);
        let store = recall_store();
        block(ingest(&store, &sub)).unwrap();
        // one FileRevision (content-addressed), two reads
        assert_eq!(count(&store, "n_file_revision"), 1);
        assert_eq!(count(&store, "n_file_read"), 2);
    }

    #[test]
    fn ingest_is_incremental_and_writes_the_durable_watermark() {
        let tc = b"{\"id\":\"t1\",\"name\":\"Read\",\"file_path\":\"a.rs\",\"sha256\":\"h1\"}\n";
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_artifact("c1", "tool_calls.jsonl", tc);
        let store = recall_store();
        let r1 = block(ingest(&store, &sub)).unwrap();
        assert_eq!(r1.skipped, 0);
        let r2 = block(ingest(&store, &sub)).unwrap();
        assert_eq!(
            r2.skipped, 1,
            "an already-projected checkpoint is skipped without re-reading artifacts"
        );
        assert_eq!(r2.nodes, 0);
        // The durable watermark: one append-only row, deduped across re-runs.
        let rows =
            block(store.query("SELECT branch, branch_tip FROM ingest_watermark", &[])).unwrap();
        assert_eq!(rows.len(), 1, "same tip re-run dedupes (content-addressed)");
        assert_eq!(
            rows[0].get_text(0).unwrap(),
            "refs/heads/rift/checkpoints/v1"
        );
        assert_eq!(rows[0].get_text(1).unwrap(), "c1");
    }

    #[test]
    fn ingest_persists_changed_hunks_between_consecutive_revisions() {
        // The same path read at two hashes across checkpoints; bytes resolvable
        // via the substrate → hunks land in n_changed_hunk (the wired path).
        let tc1 =
            b"{\"id\":\"t1\",\"name\":\"Read\",\"file_path\":\"f.rs\",\"sha256\":\"h_old\"}\n";
        let tc2 =
            b"{\"id\":\"t2\",\"name\":\"Read\",\"file_path\":\"f.rs\",\"sha256\":\"h_new\"}\n";
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_artifact("c1", "tool_calls.jsonl", tc1)
            .with_shard(shard("c2", 2))
            .with_artifact("c2", "tool_calls.jsonl", tc2)
            .with_blob("h_old", b"a\nb\nc\n")
            .with_blob("h_new", b"a\nB\nc\n");
        let store = recall_store();
        let r = block(ingest(&store, &sub)).unwrap();
        assert!(r.hunks > 0, "consecutive distinct revisions produce hunks");
        let n = block(store.count("n_changed_hunk")).unwrap();
        assert!(n > 0);
        block(ingest(&store, &sub)).unwrap();
        assert_eq!(
            n,
            block(store.count("n_changed_hunk")).unwrap(),
            "hunk persistence is a content-addressed set-union no-op"
        );
    }

    #[test]
    fn developer_axis_is_populated_from_bindings_metadata_and_fallback() {
        // c1: session binding wins; c2: metadata-carried id wins over binding;
        // c3: the default fallback stands in.
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_artifact("c1", "metadata.json", b"{\"session_ids\":[\"s1\"]}")
            .with_shard(shard("c2", 2))
            .with_artifact(
                "c2",
                "metadata.json",
                b"{\"session_ids\":[\"s2\"],\"developer\":\"dev_meta\"}",
            )
            .with_shard(shard("c3", 3));
        let store = recall_store();
        let opts = IngestOptions {
            developer_by_session: [
                ("s1".to_string(), "dev_bind_1".to_string()),
                ("s2".to_string(), "dev_bind_2".to_string()),
            ]
            .into_iter()
            .collect(),
            default_developer: Some("dev_default".to_string()),
            token_estimate_by_hash: BTreeMap::new(),
        };
        block(ingest_with(&store, &sub, &opts)).unwrap();
        let dev_of = |cid: &str| {
            let rows = block(store.query(
                "SELECT developer FROM n_checkpoint WHERE node_id = ?1",
                &[Value::text(cid)],
            ))
            .unwrap();
            rows[0].get_opt_text(0).unwrap()
        };
        assert_eq!(dev_of("c1").as_deref(), Some("dev_bind_1"));
        assert_eq!(
            dev_of("c2").as_deref(),
            Some("dev_meta"),
            "an explicitly captured id wins over the binding join"
        );
        assert_eq!(dev_of("c3").as_deref(), Some("dev_default"));
        // sessions carry their own binding
        let s1 = block(store.query("SELECT developer FROM n_session WHERE node_id = 's1'", &[]))
            .unwrap();
        assert_eq!(
            s1[0].get_opt_text(0).unwrap().as_deref(),
            Some("dev_bind_1")
        );
    }

    /// A shard frozen with `\`-spelled paths — a Windows capture from before
    /// the `/` rule — projects `/`-spelled rows, and the derived id is the id
    /// of the spelling the row carries, so every lookup keyed the one way
    /// finds it.
    #[test]
    fn a_backslash_spelled_shard_projects_slash_spelled_rows() {
        let tc = br#"{"id":"t1","tool":"Read","path":"bins\\rift\\src\\hooks.rs","sha256":"h1"}
"#;
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_artifact("c1", "tool_calls.jsonl", tc);
        let store = recall_store();
        block(ingest(&store, &sub)).unwrap();

        let rows = block(store.query("SELECT node_id, path FROM n_file_revision", &[])).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get_text(1).unwrap(), "bins/rift/src/hooks.rs");
        assert_eq!(
            rows[0].get_text(0).unwrap(),
            file_revision_id("bins/rift/src/hooks.rs", "h1"),
            "the id addresses the spelling the row carries"
        );

        // …and the in-memory metric slice agrees with the durable row, which
        // is what keeps the worktree hasher hashing a path that exists.
        let (reads, issues) = read_facts_for(&sub, "c1").unwrap();
        assert!(issues.is_empty());
        assert_eq!(reads[0].path, "bins/rift/src/hooks.rs");
        assert!(read_paths(&reads).contains("bins/rift/src/hooks.rs"));
    }

    proptest::proptest! {
        /// **No `\` reaches a row.** Whatever separator a shard froze, every
        /// path column of the projection is `/`-spelled: the ingest passes
        /// each one through the single normaliser, so the property is over
        /// arbitrary shards rather than over the two spellings someone
        /// thought of.
        #[test]
        fn no_backslash_reaches_a_projection_row(
            segments in proptest::collection::vec("[a-z]{1,6}", 1..5),
            backslashed in proptest::bool::ANY,
        ) {
            let sep = if backslashed { r"\\" } else { "/" };
            let path = segments.join(sep);
            let tc = format!(
                "{{\"id\":\"t1\",\"tool\":\"Read\",\"path\":\"{path}\",\"sha256\":\"h1\"}}\n"
            );
            let sub = MemorySubstrate::local()
                .with_shard(shard("c1", 1))
                .with_artifact("c1", "tool_calls.jsonl", tc.as_bytes());
            let store = recall_store();
            block(ingest(&store, &sub)).unwrap();
            let rows = block(store.query("SELECT path FROM n_file_revision", &[])).unwrap();
            proptest::prop_assert_eq!(rows.len(), 1);
            let stored = rows[0].get_text(0).unwrap();
            proptest::prop_assert!(
                !stored.contains('\\'),
                "a separator no lookup spells reached a row: {}",
                stored
            );
            proptest::prop_assert_eq!(stored, segments.join("/"));
        }

        /// Re-ingesting the same substrate adds no rows (content-addressed
        /// set-union). The keystone idempotence invariant.
        #[test]
        fn reingest_is_noop(n in 1usize..6) {
            let mut sub = MemorySubstrate::local();
            for i in 0..n {
                let id = format!("c{i}");
                let tc = format!(
                    "{{\"id\":\"t{i}\",\"name\":\"Read\",\"file_path\":\"f{i}.rs\",\"sha256\":\"h{i}\"}}\n"
                );
                sub = sub
                    .with_shard(shard(&id, i as u64 + 1))
                    .with_artifact(&id, "tool_calls.jsonl", tc.as_bytes());
            }
            let store = recall_store();
            block(ingest(&store, &sub)).unwrap();
            let nodes1 = count(&store, "graph_node");
            let edges1 = count(&store, "graph_edge");
            block(ingest(&store, &sub)).unwrap(); // second pass
            proptest::prop_assert_eq!(nodes1, count(&store, "graph_node"));
            proptest::prop_assert_eq!(edges1, count(&store, "graph_edge"));
        }
    }

    /// The two containment rows are written from the shard header the
    /// ingest already reads — the anchor commit and the archiving member.
    #[test]
    fn an_ingested_shard_records_its_anchor_commit_and_its_member() {
        let meta = br#"{"commit":"AABBCC","worktree":{"name":"swt-c","token":"0123456789ab","linked":true,"branch":"swt/c-recall"}}"#;
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_artifact("c1", "metadata.json", meta);
        let store = recall_store();
        block(ingest(&store, &sub)).unwrap();

        let rows =
            block(store.query("SELECT node_id, commit_oid FROM n_checkpoint_commit", &[])).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get_text(0).unwrap(), "c1");
        assert_eq!(
            rows[0].get_text(1).unwrap(),
            "aabbcc",
            "the anchor is stored case-folded so it compares against a walk"
        );

        let rows = block(store.query(
            "SELECT worktree_token, worktree_name, is_linked, branch FROM n_checkpoint_worktree",
            &[],
        ))
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get_text(0).unwrap(), "0123456789ab");
        assert_eq!(rows[0].get_text(1).unwrap(), "swt-c");
        assert_eq!(rows[0].get_i64(2).unwrap(), 1);
        assert_eq!(rows[0].get_text(3).unwrap(), "swt/c-recall");
    }

    /// The upgrade case, which is every live projection: a checkpoint this
    /// index already holds is skipped by the watermark and would never acquire
    /// an anchor — so the backfill reads its header alone and writes one,
    /// without a re-ingest and without touching the rows it already has.
    #[test]
    fn a_shard_projected_before_containment_is_backfilled_without_a_reingest() {
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_artifact("c1", "metadata.json", br#"{"commit":"aabb"}"#);
        let store = recall_store();
        // A projection as it stands today: the checkpoint row, no anchor row.
        block(store.insert_or_ignore(
            "n_checkpoint",
            &[
                "node_id",
                "dialect",
                "ordinal",
                "prev_checkpoint",
                "strategy",
                "merkle_root",
                "developer",
            ],
            &[
                rift_store::Value::text("c1"),
                rift_store::Value::text("rift"),
                rift_store::Value::Int(1),
                rift_store::Value::Null,
                rift_store::Value::Null,
                rift_store::Value::Null,
                rift_store::Value::Null,
            ],
        ))
        .unwrap();

        let report = block(ingest(&store, &sub)).unwrap();
        assert_eq!(report.skipped, 1, "the watermark still skips the shard");
        assert_eq!(report.containment_backfilled, 1);
        assert_eq!(count(&store, "n_checkpoint_commit"), 1);

        // Idempotent: a second pass finds the anchor and does no work at all.
        let again = block(ingest(&store, &sub)).unwrap();
        assert_eq!(again.containment_backfilled, 0);
        assert_eq!(count(&store, "n_checkpoint_commit"), 1);
    }

    /// A header this side will not believe writes no row. Absence is a value
    /// the predicate already carries (out of scope, reported); a half-trusted
    /// row in an append-only table is not.
    #[test]
    fn a_header_this_side_will_not_believe_writes_no_containment_row() {
        let meta =
            br#"{"commit":"not-a-hex-oid","worktree":{"name":"swt-c","token":"zz","linked":true}}"#;
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_artifact("c1", "metadata.json", meta);
        let store = recall_store();
        block(ingest(&store, &sub)).unwrap();
        assert_eq!(
            count(&store, "n_checkpoint"),
            1,
            "the checkpoint still lands"
        );
        assert_eq!(count(&store, "n_checkpoint_commit"), 0);
        assert_eq!(count(&store, "n_checkpoint_worktree"), 0);
    }
}
