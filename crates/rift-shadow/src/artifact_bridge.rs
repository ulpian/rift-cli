//! The artifact bridge: the **authoritative** canonical-event source.
//!
//! Walks the redacted checkpoint branch through the [`SubstrateReader`] seam,
//! **refuses any shard without a valid redaction report**, re-derives canonical
//! events from `full.jsonl`, projects them through exactly the same writers and
//! id derivations the live hook path uses (`crate::project` + `crate::ids`),
//! and advances the append-only ingest watermark. Because every row id is
//! content-addressed, ingest is `INSERT … OR IGNORE`: re-running the bridge —
//! after a crash, over a hook-fed projection, or from scratch — is a set-union
//! no-op. The bridge never sees the raw shadow branch: the reader it is handed
//! opens only the checkpoint ref, and a non-local reader is refused outright.

use rift_core::{CanonicalEvent, EventKind, Result, RiftError, Sha256Id};
use rift_recall_substrate::parse;
use rift_recall_substrate::{CheckpointShard, Dialect, SubstrateReader};
use rift_recall_types::checkpoint::{CheckpointMetadata, FullEventRecord};
use rift_shadow_hooks::{idempotency_key, AdapterRegistry, AgentId};
use rift_store::{
    record_captured_event, record_checkpoint_commit_if_new, record_checkpoint_if_new,
    record_checkpoint_worktree_if_new, record_ingest_watermark, record_session_in_checkpoint,
    CanonicalStore, CapturedEventRow, CheckpointRecord, IngestWatermarkRow, SessionInCheckpointRow,
};

use crate::backend::WorktreeLabel;
use crate::ids;
use crate::project::{
    payload_facts, project_event, project_main_context, project_usage, ProjectionCtx,
};

/// The checkpoint branch the bridge ingests.
pub const CHECKPOINT_BRANCH: &str = "rift/checkpoints/v1";

/// `checkpoint.created_at` for a shard that records no time of its own —
/// **declared by [`rift_store`], re-exported here.**
///
/// It moved to the crate that stores the column, so the window-scoped readers
/// over that column can bind it without depending on this one. Every existing
/// spelling (`rift_shadow::CREATED_AT_UNKNOWN`) still resolves, which is the
/// whole reason this line is a `pub use` and not a second `const`: a sentinel
/// with two declarations is a sentinel that will be right in one of them.
/// `an_unknown_time_is_an_impossible_time_not_the_epoch` below asserts the
/// VALUE rather than the declaration site, so it reddens either way.
pub use rift_store::CREATED_AT_UNKNOWN;

/// The checkpoint's own creation time, or [`CREATED_AT_UNKNOWN`].
///
/// Native shards record `created_at` (RFC3339 with millis) from the capture
/// event's own time — `condense` threads the hook's `now_ms` into
/// `metadata_json`, so the shard's claim about its own time is capture data,
/// never the clock of whoever happened to run the rebuild. Every shard
/// archived before that writer landed carries no time field, and for
/// those — and for any unparseable claim — this is the sentinel, which is the
/// honest answer.
fn created_at_for(metadata: &CheckpointMetadata) -> i64 {
    metadata
        .created_at
        .as_deref()
        .and_then(parse_rfc3339_ms)
        .unwrap_or(CREATED_AT_UNKNOWN)
}

/// Parse the small RFC3339 subset a capture tool actually writes
/// (`YYYY-MM-DDTHH:MM:SS[.fff…]Z`) into epoch ms. `None` on anything else — a
/// half-understood timestamp is worse than an admitted unknown.
///
/// The fraction is READ, not skipped (truncated past millis, the
/// `rift-recall-substrate` parser's semantics), because the native writer now
/// emits `.mmm` ([`crate::backend`]'s `rfc3339_utc_ms`) and a parser that
/// dropped it would floor every native checkpoint to the second — exactly the
/// precision the squash tie-break needs when a rewrite replays several
/// checkpoints inside one second. For the same reason, bytes between the
/// seconds field and the `Z` that are NOT a digit fraction now refuse instead
/// of being silently ignored: the earlier scan accepted
/// `…T03:04:05GARBAGEZ` as a whole-second instant, which is the
/// half-understood shape this function's own contract forbids.
fn parse_rfc3339_ms(text: &str) -> Option<i64> {
    let at = |i: usize| -> Option<u8> { text.as_bytes().get(i).copied() };
    if text.len() < 20
        || at(4) != Some(b'-')
        || at(7) != Some(b'-')
        || at(10) != Some(b'T')
        || !text.ends_with('Z')
    {
        return None;
    }
    let millis = match at(19) {
        Some(b'Z') if text.len() == 20 => 0,
        Some(b'.') => {
            let digits = text.get(20..text.len().checked_sub(1)?)?;
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            digits
                .bytes()
                .take(3)
                .fold(0i64, |ms, b| ms * 10 + i64::from(b - b'0'))
                * match digits.len() {
                    1 => 100,
                    2 => 10,
                    _ => 1,
                }
        }
        _ => return None,
    };
    let num = |a: usize, z: usize| -> Option<i64> { text.get(a..z)?.parse::<i64>().ok() };
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // Days from civil (Howard Hinnant's algorithm) over the proleptic Gregorian
    // calendar — one field does not justify a date dependency.
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = if shifted >= 0 { shifted } else { shifted - 399 } / 400;
    let year_of_era = shifted - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some((days * 86_400 + hour * 3600 + minute * 60 + second) * 1000 + millis)
}

/// Why a shard was refused for projection (report + audit; never a panic).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedShard {
    /// The shard's checkpoint id.
    pub checkpoint_id: String,
    /// The refusal reason (category only, never payload bytes).
    pub reason: String,
}

/// What one bridge run did — the crash-resume + idempotency evidence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BridgeReport {
    /// The branch walked.
    pub branch: String,
    /// The watermark token recorded (the highest shard id ingested), if any.
    pub tip: Option<String>,
    /// Shards enumerated on the branch.
    pub shards_seen: usize,
    /// Shards projected (valid redaction report).
    pub shards_projected: usize,
    /// Shards refused, with reasons.
    pub refused: Vec<RefusedShard>,
    /// Canonical events re-derived from artifacts.
    pub events_derived: usize,
    /// `captured_event` rows actually inserted (0 on a re-run or over a
    /// hook-fed projection — the dual-source set-union no-op).
    pub events_inserted: usize,
    /// Lenient-parse issues encountered (enumerated, not fatal).
    pub parse_issues: usize,
    /// What those issues **were**, for the ones a human can act on — bounded by
    /// [`MAX_PARSE_NOTES`].
    ///
    /// A count alone is what let a whole archived dialect disappear: every
    /// dot-spelled record failed [`EventKind::from_token`], was skipped, and
    /// added 1 to a number that says nothing about which records or why. A
    /// dropped record is a note now, and a note names its cause.
    pub parse_notes: Vec<String>,
}

/// How many parse notes a bridge run keeps.
///
/// Bounded because the input is untrusted: a corrupt or hostile `full.jsonl`
/// can carry an unbounded number of unknown kinds, and a report that grows with
/// it is a memory footgun and an unreadable one. The count is exact regardless;
/// only the naming is capped, and the cap says so in its own line.
pub const MAX_PARSE_NOTES: usize = 8;

/// What one ingest run holds fixed across every shard it walks.
struct IngestCtx<'a, S, R> {
    store: &'a S,
    sub: &'a R,
    repo_id: &'a str,
    default_agent: &'a str,
    now_ms: i64,
    registry: &'a AdapterRegistry,
}

/// One addressable shard's header: the checkpoint it names and what it claims.
struct ShardHeader<'a> {
    id: &'a str,
    ckpt_id: Sha256Id,
    metadata: &'a CheckpointMetadata,
    metadata_bytes: Option<&'a [u8]>,
}

impl BridgeReport {
    /// Render the report as the stable, human-readable summary (snapshot-locked
    /// in tests; brand-free).
    #[must_use]
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(s, "bridge: {}", self.branch);
        let _ = writeln!(
            s,
            "  shards: {} seen, {} projected, {} refused",
            self.shards_seen,
            self.shards_projected,
            self.refused.len()
        );
        for r in &self.refused {
            let _ = writeln!(s, "  refused {}: {}", r.checkpoint_id, r.reason);
        }
        let _ = writeln!(
            s,
            "  events: {} derived, {} inserted",
            self.events_derived, self.events_inserted
        );
        let _ = writeln!(s, "  parse issues: {}", self.parse_issues);
        for n in &self.parse_notes {
            let _ = writeln!(s, "  note: {n}");
        }
        if let Some(extra) = self
            .parse_issues
            .checked_sub(self.parse_notes.len())
            .filter(|n| *n > 0 && self.parse_notes.len() >= MAX_PARSE_NOTES)
        {
            let _ = writeln!(s, "  note: and {extra} more not named");
        }
        match &self.tip {
            Some(t) => {
                let _ = writeln!(s, "  watermark: {t}");
            }
            None => {
                let _ = writeln!(s, "  watermark: (unchanged)");
            }
        }
        s
    }
}

/// Record a parse note, up to [`MAX_PARSE_NOTES`]. `parse_issues` is counted by
/// the caller either way, so capping the naming never hides the fact that
/// something was dropped — only which one it was, past the cap.
/// The commit oid a shard's header claims — **when the claim can be one**.
///
/// `metadata.json` is untrusted, leniently-parsed input, and `commit` is a free
/// string there. A git object id is 40 hex digits (SHA-1) or 64 (SHA-256), so
/// anything else is not a commit and is refused rather than written: a token in
/// an append-only table that cannot be an object id is a permanent false fact,
/// and the whole point of the linkage is to be dereferenceable.
///
/// Normalized to lowercase because the registry's own rule is that every hash
/// and oid is stored as lowercase hex (`schema.rs`, registry header) — a
/// case-split column would answer "which commit" with two different rows.
fn commit_oid_from_metadata(claim: Option<&str>) -> Option<String> {
    let raw = claim?.trim();
    if !matches!(raw.len(), 40 | 64) || !raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(raw.to_ascii_lowercase())
}

/// The screened `worktree` member of a shard's `metadata.json`, or `None` when
/// the shard has none, cannot be parsed, or carries one outside the shape this
/// writer produces (`WorktreeLabel::screened`).
fn worktree_member(metadata_bytes: Option<&[u8]>) -> Option<WorktreeLabel> {
    WorktreeLabel::from_metadata_json(metadata_bytes?)
}

/// Whether the header CLAIMS a worktree at all — the difference between "this
/// shard predates the label" (ordinary, silent) and "this shard carries a label
/// this reader refused" (a fact worth a note).
fn claims_a_worktree(metadata_bytes: Option<&[u8]>) -> bool {
    metadata_bytes
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        .is_some_and(|doc| doc.get("worktree").is_some())
}

fn note(report: &mut BridgeReport, text: String) {
    if report.parse_notes.len() < MAX_PARSE_NOTES {
        report.parse_notes.push(text);
    }
}

/// The longest a shard-supplied token may be when it is quoted back in a note.
///
/// The note is built from bytes an untrusted `full.jsonl` supplied, so it is
/// bounded before it reaches a report a hook prints. Identifiers only — a kind
/// token and a session id — never payload text.
const MAX_NOTE_TOKEN: usize = 48;

/// Bound a shard-supplied identifier for quoting in a note (char-boundary safe).
fn truncate_token(token: &str) -> String {
    match token.char_indices().nth(MAX_NOTE_TOKEN) {
        None => token.to_owned(),
        Some((cut, _)) => format!("{}…", token.get(..cut).unwrap_or_default()),
    }
}

/// Reconstruct a [`CanonicalEvent`] from one archived `full.jsonl` record.
///
/// The kind is parsed with [`EventKind::from_wire_token`], which accepts the
/// dot-separated dialect the committed read-compat fixtures carry as well as
/// the canonical snake_case spelling, and normalises to the latter — so what
/// reaches `captured_event.kind` is one spelling regardless of which the shard
/// used. An unrecognised token is still `None`: never guessed, and now never
/// silent either (the caller names it).
fn event_from_record(rec: &FullEventRecord) -> Option<CanonicalEvent> {
    let kind = EventKind::from_wire_token(&rec.kind)?;
    let payload_sha256 = rec
        .payload_sha256
        .as_deref()
        .and_then(|h| Sha256Id::parse_hex(h).ok())
        .unwrap_or_else(|| {
            // The archived record carries no explicit address: derive it from
            // the (redacted) payload bytes exactly the way ingest does.
            use sha2::{Digest as _, Sha256};
            let bytes = serde_json::to_vec(&rec.payload).unwrap_or_default();
            let mut h = Sha256::new();
            h.update(&bytes);
            let mut b = [0u8; 32];
            b.copy_from_slice(&h.finalize());
            Sha256Id::from_bytes(b)
        });
    Some(CanonicalEvent {
        schema_version: 1,
        session: rec.session.clone(),
        turn_ordinal: rec.turn_ordinal,
        event_ordinal: rec.event_ordinal,
        kind,
        trust: kind.default_trust(),
        occurred_at_ms: rec.occurred_at_ms,
        payload_sha256,
    })
}

/// Ingest everything on the checkpoint branch into the projection.
/// Crash-resumable and idempotent; refuses non-local readers and unredacted
/// shards. `default_agent` names the adapter used for usage extraction when a
/// record does not carry its own agent token.
///
/// # Errors
/// [`rift_core::RiftError`] on a reader or storage failure. A malformed shard
/// is **not** an error — it is refused and reported.
pub async fn ingest_from_substrate(
    store: &impl CanonicalStore,
    sub: &impl SubstrateReader,
    repo_id: &str,
    default_agent: &str,
    now_ms: i64,
) -> Result<BridgeReport> {
    ingest_dialect(store, sub, repo_id, default_agent, now_ms, None).await
}

/// [`ingest_from_substrate`], restricted to one dialect.
///
/// `rift import --from-legacy` needs exactly this and nothing more. **Measured
/// before it was written** — the standing measure-first rule — because the
/// finding it answers is narrower than "import writes nothing": the
/// event-projection half of the shard walk below is NOT inside the
/// `Sha256Id::parse_hex(id)` guard, so `rift rebuild` has always projected a
/// pre-Rift shard's events — the same rows an import would write — while
/// `rift import --from-legacy` reported and wrote nothing.
/// The rows were reachable; the verb the operator was told to run was not the
/// verb that wrote them.
///
/// What the restriction buys is that import stays import: an unfiltered call
/// would also ingest this repository's own native checkpoints, which is
/// `rift rebuild`'s job and would make an import report numbers about work
/// nobody imported.
///
/// **A pre-Rift shard still takes no chain position and no `checkpoint` row.**
/// Its id is not a SHA-256 (see `GitCheckpointSource::from_reader`), and
/// minting a Rift id for it would put a row in the projection that the git side
/// cannot produce — `rift verify` would read the surplus row as tamper on every
/// subsequent run. That is "imported checkpoints are claims, not chain
/// anchors" as a mechanical fact rather than a slogan, and giving them a
/// durable home needs a table of its own.
///
/// # Errors
/// [`rift_core::RiftError`] on a reader or storage failure. A malformed shard
/// is **not** an error — it is refused and reported.
pub async fn ingest_dialect(
    store: &impl CanonicalStore,
    sub: &impl SubstrateReader,
    repo_id: &str,
    default_agent: &str,
    now_ms: i64,
    only: Option<Dialect>,
) -> Result<BridgeReport> {
    if !sub.is_local_only() {
        return Err(RiftError::PolicyDenied(
            "the artifact bridge only ingests from a local substrate".into(),
        ));
    }
    let mut shards: Vec<CheckpointShard> = sub.checkpoints()?;
    if let Some(dialect) = only {
        shards.retain(|s| s.dialect == dialect);
    }
    shards.sort_by(|a, b| {
        a.ordinal
            .cmp(&b.ordinal)
            .then(a.checkpoint_id.cmp(&b.checkpoint_id))
    });
    let mut report = BridgeReport {
        branch: CHECKPOINT_BRANCH.to_owned(),
        shards_seen: shards.len(),
        ..BridgeReport::default()
    };
    let registry = AdapterRegistry::with_builtin();
    let ctx = IngestCtx {
        store,
        sub,
        repo_id,
        default_agent,
        now_ms,
        registry: &registry,
    };

    // The nearest preceding shard whose id is a SHA-256 — the one derivation of
    // `prev` (see `record_chain_facts`), advanced before the shard is judged so
    // a shard this walk refuses still occupies its place in the linkage.
    // Refusal is a fact about a shard's report, not about where it sits in the
    // walk, and the checker's own re-derivation does not consult the report at
    // all: if the two advanced on different conditions they would disagree the
    // moment a shard was refused.
    let mut last_parseable: Option<Sha256Id> = None;
    for shard in &shards {
        let id = shard.checkpoint_id.as_str();
        let parsed_id = Sha256Id::parse_hex(id).ok();
        let prev = last_parseable;
        last_parseable = parsed_id.or(last_parseable);
        ingest_shard(&ctx, &mut report, id, parsed_id, prev).await?;
    }

    advance_watermark(&ctx, &mut report, &shards).await?;
    Ok(report)
}

/// Judge one shard, then project what it carries: the chain facts from its
/// header, its session links, and its archived event stream. `parsed_id` is the
/// shard's id when it is a SHA-256; `prev` is the nearest preceding shard whose
/// id is one.
async fn ingest_shard<S: CanonicalStore, R: SubstrateReader>(
    ctx: &IngestCtx<'_, S, R>,
    report: &mut BridgeReport,
    id: &str,
    parsed_id: Option<Sha256Id>,
    prev: Option<Sha256Id>,
) -> Result<()> {
    if let Some(reason) = redaction_refusal(ctx.sub, report, id)? {
        report.refused.push(RefusedShard {
            checkpoint_id: id.to_owned(),
            reason: reason.into(),
        });
        return Ok(());
    }

    // The BYTES are kept beside the parsed view: the worktree member is read
    // from them directly rather than through the recall plane's lenient
    // `CheckpointMetadata`, which belongs to another crate and another
    // projection (`recall.db` keeps its own twin).
    let metadata_bytes = ctx.sub.read_artifact(id, "metadata.json")?;
    let metadata = match &metadata_bytes {
        Some(bytes) => {
            let out = parse::parse_metadata(bytes, id);
            report.parse_issues += out.issues.len();
            out.value
        }
        None => CheckpointMetadata::default(),
    };
    let merkle_root = metadata
        .merkle_root
        .as_deref()
        .and_then(|h| Sha256Id::parse_hex(h).ok());
    if let (Some(ckpt_id), Some(root)) = (parsed_id, merkle_root) {
        let header = ShardHeader {
            id,
            ckpt_id,
            metadata: &metadata,
            metadata_bytes: metadata_bytes.as_deref(),
        };
        record_chain_facts(ctx.store, report, &header, root, prev).await?;
    }
    record_sessions(ctx, id, &metadata).await?;
    project_full_events(ctx, report, id).await?;
    report.shards_projected += 1;
    Ok(())
}

/// STRICT LAYER: a shard without a valid redaction report is never projected.
/// The lenient parser enumerates; this judges — `Some` is the refusal reason.
fn redaction_refusal(
    sub: &impl SubstrateReader,
    report: &mut BridgeReport,
    id: &str,
) -> Result<Option<&'static str>> {
    let Some(report_bytes) = sub.read_artifact(id, "redaction_report.json")? else {
        return Ok(Some("missing redaction report"));
    };
    let parsed_report = parse::parse_redaction_report(&report_bytes, id);
    report.parse_issues += parsed_report.issues.len();
    if !parsed_report.value.is_valid() {
        return Ok(Some("invalid redaction report"));
    }
    Ok(None)
}

/// Checkpoint header → chain row, anchor commit and archiving worktree
/// (idempotent), for a shard whose id is a SHA-256 and whose header carries a
/// Merkle root.
///
/// THE CHAIN COMES FROM THE WALK, NOT FROM THE HEADER.
///
/// This used to read `metadata.prev_hash` — a field the native writer has never
/// populated (`condense` builds every archive with `prev_hash: None`, and
/// `metadata_json` omits the key when it is `None`, so not one shard in a real
/// repository carries it). So the bridge recorded `prev = None` for every
/// checkpoint it ingested, while `GitCheckpointSource` — the source `verify`
/// compares against — derives `prev` from the substrate WALK. Two writers, two
/// notions of the chain, and the disagreement only surfaced when a rebuild
/// inserted a row into a projection that already held a walk-derived chain: a
/// SECOND GENESIS, and `verify` rightly called it tamper.
///
/// One derivation now, and it is **the nearest preceding shard whose id is a
/// SHA-256** rather than `shard.prev` verbatim. `shard.prev` is the previous
/// element of the walk whatever its id looks like, so on a repository holding
/// pre-Rift shards it named a predecessor that cannot BE a chain position — and
/// `Sha256Id::parse_hex(p)?` here turned `rift rebuild` (and the `doctor --fix`
/// that drives it) into an internal error on exactly those repositories, the
/// same abort `GitCheckpointSource` used to raise for `rift verify`. Both sides
/// skip an unaddressable id and keep the linkage continuous across it; when
/// every id parses the two rules coincide, because the walk sets `prev` to the
/// previous element.
async fn record_chain_facts(
    store: &impl CanonicalStore,
    report: &mut BridgeReport,
    header: &ShardHeader<'_>,
    root: Sha256Id,
    prev: Option<Sha256Id>,
) -> Result<()> {
    // The header's own claim is not the chain, but it is not nothing either: a
    // shard that claims a DIFFERENT predecessor is a fact worth surfacing
    // rather than silently discarding. Reported, never acted on — acting on it
    // is what created the fork. Compared against the predecessor actually
    // RECORDED, not against `shard.prev`: on a mixed repository those differ,
    // and a disagreement report about a value no row holds says nothing.
    if let Some(claimed) = header.metadata.prev_hash.as_deref() {
        let recorded = prev.map(|p| p.to_hex());
        let agrees = recorded.as_deref() == Some(claimed);
        if !agrees {
            report.parse_issues += 1;
        }
    }
    record_checkpoint_if_new(
        store,
        &CheckpointRecord {
            checkpoint_id: header.ckpt_id,
            strategy: header
                .metadata
                .strategy
                .clone()
                .unwrap_or_else(|| "manual-commit".into()),
            // NOT `now_ms`: that is when the rebuild ran, not when the
            // checkpoint happened, and stamping ingest time onto a historical
            // row is how a projection comes to disagree with its own git. See
            // `created_at_for`.
            created_at: created_at_for(header.metadata),
            merkle_root: root,
            prev,
            schema_version: 1,
        },
    )
    .await?;
    record_anchor_commit(store, report, header).await?;
    record_archiving_worktree(store, report, header).await
}

/// THE ANCHOR COMMIT, which was in scope here all along.
///
/// Every shard the native writer produces records the commit it captured
/// (`metadata.json`'s `commit`), and the chain-facts step — which already reads
/// `strategy`, `prev_hash`, `created_at` and `sessions` off the same header —
/// dropped it. So "which commit is this checkpoint about" was answerable only
/// by re-reading git, and the outcome-label deriver's own doc comment ("absent
/// ⇒ no outcome label") described a value the projection never held.
///
/// A projection, not a new fact: it is re-derived from the shard on every
/// rebuild, so the write is idempotent and nothing here can outlive the archive
/// it came from.
///
/// ★ WHAT THE ROW DOES AND DOES NOT PROMISE. It records the commit the shard
/// NAMED, screened to be shaped like an object id — never that the object is
/// still reachable. Measured on this repository's own chain at the commit that
/// added the table: 104 shards name 76 distinct commits, of which **43 still
/// exist as objects (42 of them ancestors of HEAD) and 33 are gone fully** —
/// rebased, amended or collected away, which is ordinary in a program that
/// rebases branches. A consumer must therefore treat a non-resolving oid as
/// "the commit this checkpoint named has left this repository", not as a
/// corrupt row; and this side must not "fix" it by substituting a commit that
/// does still resolve, which would be inventing an anchor.
async fn record_anchor_commit(
    store: &impl CanonicalStore,
    report: &mut BridgeReport,
    header: &ShardHeader<'_>,
) -> Result<()> {
    let id = header.id;
    let claim = header.metadata.commit.as_deref();
    match commit_oid_from_metadata(claim) {
        Some(oid) => {
            record_checkpoint_commit_if_new(store, &header.ckpt_id, &oid).await?;
        }
        None => {
            // A shard that claims a commit we cannot believe is a fact worth
            // naming, not a silent skip — the counter-with-no-name is what once
            // hid a whole archived dialect.
            if claim.is_some() {
                report.parse_issues += 1;
                note(
                    report,
                    format!(
                        "{id}: header claims a commit that is not an object id {:?} \
                         — no checkpoint↔commit link recorded",
                        truncate_token(claim.unwrap_or_default())
                    ),
                );
            }
        }
    }
    Ok(())
}

/// THE ARCHIVING WORKTREE, projected from the shard's own `worktree` member. A
/// PROJECTION, like the anchor commit: it is re-derived from the shard on every
/// rebuild, so the write is idempotent and nothing here outlives the archive it
/// came from.
///
/// **A shard WITHOUT the member projects NO ROW.** That is the whole
/// rebuild-parity claim: every shard written before worktree members existed is
/// memberless, and a row invented for one would be a fact the projection may
/// not mint about a sealed shard. Readers render the absence as `unknown`,
/// never as `main`.
async fn record_archiving_worktree(
    store: &impl CanonicalStore,
    report: &mut BridgeReport,
    header: &ShardHeader<'_>,
) -> Result<()> {
    let id = header.id;
    match worktree_member(header.metadata_bytes) {
        Some(label) => {
            record_checkpoint_worktree_if_new(
                store,
                &header.ckpt_id.to_hex(),
                &label.token,
                &label.name,
                label.linked,
                label.branch.as_deref(),
            )
            .await?;
        }
        None => {
            // A member this side will not believe is NAMED, not silently
            // skipped — the anonymous-counter arm is what once hid a whole
            // archived dialect. Absence itself is not a note: every shard
            // written before the member existed is memberless and that is
            // ordinary.
            if claims_a_worktree(header.metadata_bytes) {
                report.parse_issues += 1;
                note(
                    report,
                    format!(
                        "{id}: header carries a worktree member this reader will not \
                         believe — no checkpoint↔worktree row recorded"
                    ),
                );
            }
        }
    }
    Ok(())
}

/// Link every session the header names to this checkpoint, in header order.
async fn record_sessions<S: CanonicalStore, R>(
    ctx: &IngestCtx<'_, S, R>,
    id: &str,
    metadata: &CheckpointMetadata,
) -> Result<()> {
    for (ordinal, session) in metadata.sessions.iter().enumerate() {
        record_session_in_checkpoint(
            ctx.store,
            &SessionInCheckpointRow {
                checkpoint_id: id.to_owned(),
                session_id: ids::session_row_id(ctx.default_agent, session),
                ordinal: i64::try_from(ordinal).unwrap_or(i64::MAX),
                schema_version: 1,
            },
        )
        .await?;
    }
    Ok(())
}

/// The archived canonical-event stream → the SAME projector the live hook path
/// runs (identical ids ⇒ the union is a set-union no-op). A header-only shard
/// has no `full.jsonl`: its chain facts landed, and it has no events.
async fn project_full_events<S: CanonicalStore, R: SubstrateReader>(
    ctx: &IngestCtx<'_, S, R>,
    report: &mut BridgeReport,
    id: &str,
) -> Result<()> {
    let Some(full_bytes) = ctx.sub.read_artifact(id, "full.jsonl")? else {
        return Ok(());
    };
    let full = parse::parse_full_events(&full_bytes, id);
    report.parse_issues += full.issues.len();
    for rec in &full.value {
        let Some(ev) = event_from_record(rec) else {
            note_dropped_record(report, id, rec);
            continue;
        };
        report.events_derived += 1;
        project_record(ctx, report, rec, &ev).await?;
    }
    Ok(())
}

/// A dropped record says WHY it was dropped, and says it with the token that
/// caused it. This arm used to bump an anonymous counter, which is how the
/// dot-spelled dialect vanished without a word: the number went up and nothing
/// named a kind.
fn note_dropped_record(report: &mut BridgeReport, id: &str, rec: &FullEventRecord) {
    report.parse_issues += 1;
    note(
        report,
        format!(
            "{id}: dropped a record with an unrecognised kind {:?} \
             (session {}, turn {}, event {})",
            truncate_token(&rec.kind),
            truncate_token(&rec.session),
            rec.turn_ordinal,
            rec.event_ordinal
        ),
    );
}

/// Write one re-derived event: its `captured_event` row (keyed by the same
/// idempotency key the live hook path derives), then its projection, its usage
/// and its main-context usage when the record's adapter reads them.
async fn project_record<S: CanonicalStore, R>(
    ctx: &IngestCtx<'_, S, R>,
    report: &mut BridgeReport,
    rec: &FullEventRecord,
    ev: &CanonicalEvent,
) -> Result<()> {
    let agent_token = rec
        .agent
        .clone()
        .unwrap_or_else(|| ctx.default_agent.to_owned());
    let agent_id = AgentId::new(&agent_token);
    let key = idempotency_key(
        &agent_id,
        &ev.session,
        &rec.verb,
        ev.turn_ordinal,
        ev.event_ordinal,
        &ev.payload_sha256,
    );
    let inserted = record_captured_event(
        ctx.store,
        &CapturedEventRow {
            idempotency_key: key.to_hex(),
            session_id: ev.session.clone(),
            turn_ordinal: i64::from(ev.turn_ordinal),
            event_ordinal: i64::from(ev.event_ordinal),
            kind: ev.kind.as_str().to_owned(),
            trust: ev.trust.as_str().to_owned(),
            payload_sha256: ev.payload_sha256.to_hex(),
            occurred_at_ms: ev.occurred_at_ms,
            schema_version: i64::from(ev.schema_version),
        },
    )
    .await?;
    if inserted {
        report.events_inserted += 1;
    }

    let facts = payload_facts(&rec.payload);
    let pctx = ProjectionCtx {
        agent: &agent_token,
        repo_id: ctx.repo_id,
    };
    project_event(ctx.store, ev, &facts, pctx).await?;
    if let Some(usage) = ctx
        .registry
        .get(&agent_id)
        .and_then(|a| a.usage(&rec.verb, &rec.payload))
    {
        project_usage(ctx.store, ev, &facts, pctx, &usage, ctx.now_ms).await?;
    }
    if let Some(usage) = ctx
        .registry
        .get(&agent_id)
        .and_then(|a| a.main_context_usage(&rec.verb, &rec.payload))
    {
        project_main_context(ctx.store, ev, &rec.verb, &facts, pctx, &usage).await?;
    }
    Ok(())
}

/// Advance the append-only watermark to the branch's high-water shard.
/// Written LAST: a crash before this step re-runs as a pure no-op.
async fn advance_watermark<S: CanonicalStore, R>(
    ctx: &IngestCtx<'_, S, R>,
    report: &mut BridgeReport,
    shards: &[CheckpointShard],
) -> Result<()> {
    let Some(tip) = shards.last().map(|s| s.checkpoint_id.clone()) else {
        return Ok(());
    };
    record_ingest_watermark(
        ctx.store,
        &IngestWatermarkRow {
            watermark_id: ids::watermark_id(CHECKPOINT_BRANCH, &tip),
            branch: CHECKPOINT_BRANCH.to_owned(),
            branch_tip: tip.clone(),
            ingested_at: ctx.now_ms,
            schema_version: 1,
        },
    )
    .await?;
    report.tip = Some(tip);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_recall_substrate::MemorySubstrate;
    use rift_store::SqliteStore;

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        s
    }

    fn valid_report() -> String {
        format!(
            r#"{{"total_spans":1,"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
            "c".repeat(64)
        )
    }

    fn full_jsonl() -> String {
        [
            r#"{"session_id":"native-1","turn":0,"event":0,"kind":"session_start","verb":"SessionStart","occurred_at_ms":10,"payload":{"model":"claude-sonnet-4-5"}}"#,
            r#"{"session_id":"native-1","turn":1,"event":0,"kind":"tool_call","verb":"PreToolUse","occurred_at_ms":11,"payload":{"tool_name":"Bash"}}"#,
            r#"{"session_id":"native-1","turn":1,"event":0,"kind":"turn_end","verb":"Stop","occurred_at_ms":12,"payload":{}}"#,
            r#"{"session_id":"native-1","turn":2,"event":0,"kind":"session_end","verb":"SessionEnd","occurred_at_ms":13,"payload":{}}"#,
        ]
        .join("\n")
    }

    fn shard_id(n: u8) -> String {
        format!("{n:02x}").repeat(32)
    }

    fn shard(id: &str, ordinal: u64) -> CheckpointShard {
        CheckpointShard {
            checkpoint_id: id.to_owned(),
            dialect: rift_recall_substrate::Dialect::Rift,
            prev: None,
            ordinal,
        }
    }

    fn substrate_with_shard(id: &str, redaction: Option<&str>) -> MemorySubstrate {
        let mut sub = MemorySubstrate::local().with_shard(shard(id, 0));
        if let Some(r) = redaction {
            sub = sub.with_artifact(id, "redaction_report.json", r.as_bytes());
        }
        sub.with_artifact(
            id,
            "metadata.json",
            format!(
                r#"{{"strategy":"manual-commit","merkle_root":"{}","sessions":["native-1"]}}"#,
                "d".repeat(64)
            )
            .as_bytes(),
        )
        .with_artifact(id, "full.jsonl", full_jsonl().as_bytes())
    }

    /// A valid shard whose header additionally spells `commit` as `claim`
    /// (verbatim JSON, so a test can put a non-string or a junk token there).
    fn substrate_claiming_commit(id: &str, claim: &str) -> MemorySubstrate {
        MemorySubstrate::local()
            .with_shard(shard(id, 0))
            .with_artifact(id, "redaction_report.json", valid_report().as_bytes())
            .with_artifact(
                id,
                "metadata.json",
                format!(
                    r#"{{"strategy":"manual-commit","merkle_root":"{}","sessions":["native-1"],"commit":{claim}}}"#,
                    "d".repeat(64)
                )
                .as_bytes(),
            )
            .with_artifact(id, "full.jsonl", full_jsonl().as_bytes())
    }

    async fn linked_commits(s: &SqliteStore) -> Vec<(String, String)> {
        let rows = s
            .query(
                "SELECT checkpoint_id, commit_oid FROM checkpoint_commit ORDER BY checkpoint_id",
                &[],
            )
            .await
            .unwrap();
        rows.iter()
            .map(|r| (r.get_text(0).unwrap(), r.get_text(1).unwrap()))
            .collect()
    }

    /// **The linkage, end to end.** The commit a shard names lands as a row,
    /// keyed to that checkpoint — and a second ingest of the same substrate
    /// (what a REBUILD is) re-derives it without a duplicate and without an
    /// UPDATE. Deleting the `record_checkpoint_commit_if_new` call reds the
    /// first assertion; making the write non-idempotent reds the second.
    #[tokio::test]
    async fn the_commit_a_shard_names_lands_and_a_rebuild_re_derives_it() {
        let s = store();
        let id = shard_id(0x61);
        let oid = "ab".repeat(20); // 40 hex = a SHA-1 commit
        let sub = substrate_claiming_commit(&id, &format!("\"{oid}\""));

        let r = ingest_from_substrate(&s, &sub, "repo", "claude-code", 1)
            .await
            .unwrap();
        assert_eq!(r.parse_issues, 0);
        assert_eq!(linked_commits(&s).await, vec![(id.clone(), oid.clone())]);

        // The rebuild: same substrate, same store, set-union no-op.
        ingest_from_substrate(&s, &sub, "repo", "claude-code", 2)
            .await
            .unwrap();
        assert_eq!(
            linked_commits(&s).await,
            vec![(id, oid)],
            "a rebuild re-derives the same row, it does not duplicate or update it"
        );
    }

    /// A shard that names no commit gets no row — the absence is the answer,
    /// which is exactly what a separate relation buys over a nullable column.
    #[tokio::test]
    async fn a_shard_that_names_no_commit_gets_no_row() {
        let s = store();
        let id = shard_id(0x62);
        let sub = substrate_with_shard(&id, Some(&valid_report()));
        let r = ingest_from_substrate(&s, &sub, "repo", "claude-code", 1)
            .await
            .unwrap();
        assert_eq!(r.parse_issues, 0, "silence is not an issue");
        assert!(linked_commits(&s).await.is_empty());
    }

    /// A header claiming something that cannot be an object id is REFUSED and
    /// SAID — no row, and a note naming the shard. The checkpoint itself still
    /// projects: one bad field does not cost the chain row.
    #[tokio::test]
    async fn a_claim_that_cannot_be_an_object_id_is_refused_and_named() {
        let s = store();
        let id = shard_id(0x63);
        let sub = substrate_claiming_commit(&id, "\"HEAD~1\"");
        let r = ingest_from_substrate(&s, &sub, "repo", "claude-code", 1)
            .await
            .unwrap();
        assert!(linked_commits(&s).await.is_empty(), "no invented linkage");
        assert_eq!(r.parse_issues, 1);
        assert!(
            r.parse_notes.iter().any(|n| n.contains("not an object id")),
            "the refusal names itself: {:?}",
            r.parse_notes
        );
        let ckpts = s
            .query("SELECT COUNT(*) FROM checkpoint", &[])
            .await
            .unwrap();
        assert_eq!(
            ckpts.first().unwrap().get_i64(0).unwrap(),
            1,
            "the chain row is unaffected by a bad commit claim"
        );
    }

    /// The screen, exhaustively. Both git oid widths pass, in either case, and
    /// storage is canonical lowercase; everything shaped like a revision
    /// expression, a truncated id, or an empty string is refused.
    #[test]
    fn commit_oid_screen_accepts_object_ids_and_nothing_else() {
        let sha1 = "a".repeat(40);
        let sha256 = "b".repeat(64);
        assert_eq!(commit_oid_from_metadata(Some(&sha1)), Some(sha1.clone()));
        assert_eq!(commit_oid_from_metadata(Some(&sha256)), Some(sha256));
        assert_eq!(
            commit_oid_from_metadata(Some(&"AB".repeat(20))),
            Some("ab".repeat(20)),
            "stored lowercase — the registry's rule for every hash and oid"
        );
        assert_eq!(
            commit_oid_from_metadata(Some(&format!("  {sha1}  "))),
            Some(sha1.clone()),
            "surrounding whitespace is not part of the id"
        );
        for junk in [
            "",
            "   ",
            "HEAD",
            "HEAD~1",
            "refs/heads/main",
            &sha1[..39],         // truncated
            &format!("{sha1}0"), // 41
            &"g".repeat(40),     // hex-shaped but not hex
            &format!("{}z", &sha1[..39]),
        ] {
            assert_eq!(
                commit_oid_from_metadata(Some(junk)),
                None,
                "{junk:?} is not an object id"
            );
        }
        assert_eq!(commit_oid_from_metadata(None), None);
    }

    /// A shard whose `full.jsonl` is exactly `lines` (everything else valid).
    fn substrate_with_events(id: &str, lines: &[&str]) -> MemorySubstrate {
        MemorySubstrate::local()
            .with_shard(shard(id, 0))
            .with_artifact(id, "redaction_report.json", valid_report().as_bytes())
            .with_artifact(
                id,
                "metadata.json",
                format!(
                    r#"{{"strategy":"manual-commit","merkle_root":"{}","sessions":["native-1"]}}"#,
                    "d".repeat(64)
                )
                .as_bytes(),
            )
            .with_artifact(id, "full.jsonl", lines.join("\n").as_bytes())
    }

    /// **The dot-drop, closed.** The committed read-compat fixtures under
    /// `legacy-fixtures/` record `"kind":"session.start"`, and the bridge used
    /// to answer that with `from_token` → `None` → skip: a whole archived
    /// dialect discarded into an anonymous counter, on a repository Rift
    /// promises to read forever.
    ///
    /// Both spellings project now, and — the half that matters — what lands in
    /// the store is ONE spelling regardless of which the shard used.
    #[tokio::test]
    async fn a_dot_spelled_record_projects_and_is_stored_under_the_canonical_spelling() {
        let s = store();
        let id = shard_id(0x51);
        let sub = substrate_with_events(
            &id,
            &[
                r#"{"session_id":"native-1","turn":0,"event":0,"kind":"session.start","verb":"SessionStart","occurred_at_ms":10,"payload":{}}"#,
                r#"{"session_id":"native-1","turn":1,"event":0,"kind":"tool.call","verb":"PreToolUse","occurred_at_ms":11,"payload":{}}"#,
            ],
        );
        let r = ingest_from_substrate(&s, &sub, "repo", "claude-code", 1)
            .await
            .unwrap();
        assert_eq!(r.events_derived, 2, "neither record was dropped");
        assert_eq!(r.parse_issues, 0, "and neither counted as an issue");
        assert!(r.parse_notes.is_empty());

        let rows = s
            .query("SELECT kind FROM captured_event ORDER BY kind", &[])
            .await
            .unwrap();
        let kinds: Vec<String> = rows
            .iter()
            .map(|r| r.get_text(0).unwrap_or_default().clone())
            .collect();
        assert_eq!(
            kinds,
            vec!["session_start".to_owned(), "tool_call".to_owned()],
            "the store carries the canonical spelling, never the wire dialect"
        );
    }

    /// **The silent drop dies loudly.** A kind nothing recognises is still
    /// refused — guessing is worse — but it is NAMED, with its token, its
    /// shard and its position, instead of adding 1 to a number.
    #[tokio::test]
    async fn an_unrecognised_kind_becomes_a_named_parse_note() {
        let s = store();
        let id = shard_id(0x52);
        let sub = substrate_with_events(
            &id,
            &[
                r#"{"session_id":"native-1","turn":0,"event":0,"kind":"session_start","verb":"SessionStart","occurred_at_ms":10,"payload":{}}"#,
                r#"{"session_id":"native-1","turn":3,"event":7,"kind":"banana_split","verb":"Nope","occurred_at_ms":11,"payload":{}}"#,
            ],
        );
        let r = ingest_from_substrate(&s, &sub, "repo", "claude-code", 1)
            .await
            .unwrap();
        assert_eq!(r.events_derived, 1, "the good record still projects");
        assert_eq!(r.parse_issues, 1);
        assert_eq!(r.parse_notes.len(), 1, "and the bad one is named");
        let note = &r.parse_notes[0];
        for expected in ["banana_split", "native-1", "turn 3", "event 7", &id] {
            assert!(note.contains(expected), "note names {expected}: {note}");
        }
        assert!(
            r.render().contains("banana_split"),
            "and it reaches the rendered report:\n{}",
            r.render()
        );
    }

    /// The naming is bounded and says so; the COUNT never is.
    #[tokio::test]
    async fn parse_notes_are_capped_and_the_report_says_how_many_were_not_named() {
        let s = store();
        let id = shard_id(0x53);
        let bad: Vec<String> = (0..MAX_PARSE_NOTES + 3)
            .map(|i| {
                format!(
                    r#"{{"session_id":"native-1","turn":{i},"event":0,"kind":"unknown_{i}","verb":"X","occurred_at_ms":1,"payload":{{}}}}"#
                )
            })
            .collect();
        let lines: Vec<&str> = bad.iter().map(String::as_str).collect();
        let r = ingest_from_substrate(&s, &substrate_with_events(&id, &lines), "repo", "cc", 1)
            .await
            .unwrap();
        assert_eq!(r.parse_issues, MAX_PARSE_NOTES + 3, "every drop is counted");
        assert_eq!(r.parse_notes.len(), MAX_PARSE_NOTES, "the naming is capped");
        assert!(
            r.render().contains("and 3 more not named"),
            "the cap is disclosed:\n{}",
            r.render()
        );
    }

    /// A shard-supplied token is bounded before it is quoted back: the note is
    /// built from untrusted bytes and is printed by a hook.
    #[tokio::test]
    async fn a_hostile_kind_token_is_truncated_in_the_note() {
        let s = store();
        let id = shard_id(0x54);
        let huge = "z".repeat(4000);
        let line = format!(
            r#"{{"session_id":"native-1","turn":0,"event":0,"kind":"{huge}","verb":"X","occurred_at_ms":1,"payload":{{}}}}"#
        );
        let r = ingest_from_substrate(&s, &substrate_with_events(&id, &[&line]), "repo", "cc", 1)
            .await
            .unwrap();
        assert_eq!(r.parse_notes.len(), 1);
        assert!(
            r.parse_notes[0].len() < 300,
            "the note is bounded: {} bytes",
            r.parse_notes[0].len()
        );
        assert!(r.parse_notes[0].contains('…'), "and says it was cut");
    }

    #[tokio::test]
    async fn bridge_projects_valid_shard_and_rerun_is_noop() {
        let s = store();
        let id = shard_id(0xab);
        let sub = substrate_with_shard(&id, Some(&valid_report()));

        let r1 = ingest_from_substrate(&s, &sub, "repo-1", "claude-code", 100)
            .await
            .unwrap();
        assert_eq!(r1.shards_projected, 1);
        assert!(r1.refused.is_empty());
        assert_eq!(r1.events_derived, 4);
        assert_eq!(r1.events_inserted, 4);
        // Normalized rows landed through the shared projector.
        assert_eq!(s.count("session").await.unwrap(), 1);
        assert_eq!(s.count("session_event").await.unwrap(), 2);
        assert_eq!(s.count("tool_call").await.unwrap(), 1);
        assert_eq!(s.count("checkpoint").await.unwrap(), 1);
        assert_eq!(s.count("session_in_checkpoint").await.unwrap(), 1);
        assert_eq!(s.count("ingest_watermark").await.unwrap(), 1);

        // Re-run: zero inserts anywhere (crash-resume / idempotency).
        let r2 = ingest_from_substrate(&s, &sub, "repo-1", "claude-code", 200)
            .await
            .unwrap();
        assert_eq!(r2.events_inserted, 0, "re-ingest is a set-union no-op");
        assert_eq!(s.count("captured_event").await.unwrap(), 4);
        assert_eq!(s.count("ingest_watermark").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn shard_without_valid_redaction_report_is_refused() {
        let s = store();
        let missing = shard_id(0x01);
        let invalid = shard_id(0x02);
        let sub = MemorySubstrate::local()
            .with_shard(shard(&missing, 0))
            .with_artifact(&missing, "full.jsonl", full_jsonl().as_bytes())
            .with_shard(shard(&invalid, 1))
            .with_artifact(&invalid, "redaction_report.json", b"{}")
            .with_artifact(&invalid, "full.jsonl", full_jsonl().as_bytes());

        let r = ingest_from_substrate(&s, &sub, "repo-1", "claude-code", 5)
            .await
            .unwrap();
        assert_eq!(r.shards_projected, 0);
        assert_eq!(r.refused.len(), 2);
        assert_eq!(
            s.count("captured_event").await.unwrap(),
            0,
            "a shard without a valid redaction report is NEVER projected"
        );
    }

    #[tokio::test]
    async fn non_local_reader_is_refused_outright() {
        struct RemoteReader(MemorySubstrate);
        impl SubstrateReader for RemoteReader {
            fn is_local_only(&self) -> bool {
                false
            }
            fn checkpoints(&self) -> rift_core::Result<Vec<CheckpointShard>> {
                self.0.checkpoints()
            }
            fn blob(&self, id: &str) -> rift_core::Result<Option<Vec<u8>>> {
                self.0.blob(id)
            }
            fn read_artifact(&self, c: &str, rel: &str) -> rift_core::Result<Option<Vec<u8>>> {
                self.0.read_artifact(c, rel)
            }
        }
        let s = store();
        let sub = RemoteReader(MemorySubstrate::local());
        let out = ingest_from_substrate(&s, &sub, "repo-1", "claude-code", 1).await;
        assert!(matches!(out, Err(RiftError::PolicyDenied(_))));
    }

    #[tokio::test]
    async fn bridge_report_snapshot() {
        let s = store();
        let ok = shard_id(0xab);
        let bad = shard_id(0xcd);
        // one refused shard alongside the good one
        let sub = substrate_with_shard(&ok, Some(&valid_report()))
            .with_shard(shard(&bad, 1))
            .with_artifact(&bad, "full.jsonl", full_jsonl().as_bytes());
        let r = ingest_from_substrate(&s, &sub, "repo-1", "claude-code", 42)
            .await
            .unwrap();
        insta::assert_snapshot!("bridge_report", r.render());
    }
    /// A shard whose header carries a verbatim `worktree` member.
    fn substrate_with_worktree(id: &str, member: &str) -> MemorySubstrate {
        MemorySubstrate::local()
            .with_shard(shard(id, 0))
            .with_artifact(id, "redaction_report.json", valid_report().as_bytes())
            .with_artifact(
                id,
                "metadata.json",
                format!(
                    r#"{{"strategy":"manual-commit","merkle_root":"{}","sessions":["native-1"],"worktree":{member}}}"#,
                    "d".repeat(64)
                )
                .as_bytes(),
            )
            .with_artifact(id, "full.jsonl", full_jsonl().as_bytes())
    }

    async fn worktree_rows(s: &SqliteStore) -> Vec<(String, String, String, i64, Option<String>)> {
        s.query(
            "SELECT checkpoint_id, worktree_token, worktree_name, is_linked, branch \
             FROM checkpoint_worktree ORDER BY checkpoint_id",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| {
            (
                r.get_text(0).unwrap(),
                r.get_text(1).unwrap(),
                r.get_text(2).unwrap(),
                r.get_i64(3).unwrap(),
                r.get_opt_text(4).unwrap(),
            )
        })
        .collect()
    }

    /// **Rebuild parity, which is the whole point.** Every shard written
    /// before the member existed carries no `worktree` member, and the
    /// projection must invent NO row for it — back-filling a sealed shard is
    /// forbidden, and readers render the absent row as `unknown`, never as
    /// `main`.
    /// The shard that DOES carry one projects exactly it, and a rebuild
    /// re-derives the same row rather than duplicating or updating it.
    #[tokio::test]
    async fn a_shard_without_worktree_provenance_projects_no_row() {
        let s = store();
        let old = shard_id(0x51);
        let sub = substrate_with_shard(&old, Some(&valid_report()));
        let r = ingest_from_substrate(&s, &sub, "repo", "claude-code", 1)
            .await
            .unwrap();
        assert_eq!(r.shards_projected, 1, "the shard itself still projects");
        assert_eq!(
            r.parse_issues, 0,
            "a memberless shard is ordinary, not a note"
        );
        assert!(
            worktree_rows(&s).await.is_empty(),
            "no member, no row — unknown stays unknown"
        );

        // A labelled shard projects the label it carries.
        let new = shard_id(0x52);
        let labelled = substrate_with_worktree(
            &new,
            r#"{"name":"swt-a","token":"0123456789ab","linked":true,"branch":"swt/a-foundations"}"#,
        );
        ingest_from_substrate(&s, &labelled, "repo", "claude-code", 2)
            .await
            .unwrap();
        assert_eq!(
            worktree_rows(&s).await,
            vec![(
                new.clone(),
                "0123456789ab".to_owned(),
                "swt-a".to_owned(),
                1,
                Some("swt/a-foundations".to_owned())
            )]
        );

        // The rebuild: same substrate, same store, set-union no-op.
        ingest_from_substrate(&s, &labelled, "repo", "claude-code", 3)
            .await
            .unwrap();
        assert_eq!(
            worktree_rows(&s).await.len(),
            1,
            "a rebuild re-derives, never duplicates"
        );
    }

    /// A member this reader will not believe is NAMED rather than silently
    /// dropped — an anonymous counter is how an archived dialect once vanished
    /// — and still writes no row, because a half-trusted label in an
    /// append-only table cannot be corrected.
    #[tokio::test]
    async fn a_worktree_member_the_reader_refuses_is_named_and_writes_nothing() {
        let s = store();
        let id = shard_id(0x53);
        let sub =
            substrate_with_worktree(&id, r#"{"name":"swt-a","token":"../../etc","linked":true}"#);
        let r = ingest_from_substrate(&s, &sub, "repo", "claude-code", 1)
            .await
            .unwrap();
        assert!(worktree_rows(&s).await.is_empty());
        assert_eq!(r.parse_issues, 1);
        assert!(
            r.parse_notes.iter().any(|n| n.contains("worktree member")),
            "{:?}",
            r.parse_notes
        );
    }
}

#[cfg(test)]
mod chain_source_tests {
    use super::*;

    #[test]
    fn an_unknown_time_is_an_impossible_time_not_the_epoch() {
        // The whole point: `0` is a real instant, so it cannot mean "unknown".
        assert_eq!(CREATED_AT_UNKNOWN, -1);
        // A capture time is epoch-ms from a real clock, so it is non-negative;
        // the sentinel is outside that range by construction, not by luck.
        assert!(
            !(0..=i64::MAX).contains(&CREATED_AT_UNKNOWN),
            "the sentinel must be unreachable by any real capture time"
        );
        let none = CheckpointMetadata::default();
        assert_eq!(created_at_for(&none), CREATED_AT_UNKNOWN);
    }

    #[test]
    fn a_shards_own_time_beats_the_clock_of_whoever_ran_the_rebuild() {
        let m = CheckpointMetadata {
            created_at: Some("2026-06-01T00:00:00Z".into()),
            ..CheckpointMetadata::default()
        };
        assert_eq!(created_at_for(&m), 1_780_272_000_000);
        // A round-trip anchor that is not the same arithmetic as the parser.
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_ms("2000-01-01T00:00:00Z"),
            Some(946_684_800_000)
        );
    }

    #[test]
    fn a_half_understood_timestamp_is_refused_rather_than_guessed() {
        for bad in [
            "",
            "2026-06-01",
            "2026-06-01T00:00:00",  // no zone
            "2026-13-01T00:00:00Z", // month 13
            "2026-06-32T00:00:00Z", // day 32
            "2026-06-01T24:00:00Z", // hour 24
            "not-a-timestamp-at-all!!!",
            // Shapes the pre-fraction scan half-understood: it checked five
            // positions and `ends_with('Z')`, so everything between the
            // seconds field and the `Z` was silently ignored. Now they refuse.
            "2026-06-01T00:00:05GARBAGEZ",
            "2026-06-01T00:00:05.Z",     // a dot with no digits
            "2026-06-01T00:00:05.12a3Z", // a non-digit inside the fraction
        ] {
            assert_eq!(parse_rfc3339_ms(bad), None, "{bad}");
            let m = CheckpointMetadata {
                created_at: Some(bad.to_owned()),
                ..CheckpointMetadata::default()
            };
            assert_eq!(created_at_for(&m), CREATED_AT_UNKNOWN, "{bad}");
        }
    }

    #[test]
    fn the_fraction_is_read_not_skipped() {
        // The native writer emits `.mmm`; a parser that dropped it would floor
        // every native checkpoint to the second — losing exactly the
        // resolution the squash tie-break needs when a rewrite replays several
        // checkpoints inside one second.
        let base = 1_780_272_000_000; // 2026-06-01T00:00:00Z, pinned above
        assert_eq!(
            parse_rfc3339_ms("2026-06-01T00:00:00.123Z"),
            Some(base + 123)
        );
        // Short fractions scale, long ones truncate past millis — the
        // `rift-recall-substrate` parser's semantics, kept identical so the
        // two lenient views of one field can never disagree on a value.
        assert_eq!(parse_rfc3339_ms("2026-06-01T00:00:00.1Z"), Some(base + 100));
        assert_eq!(
            parse_rfc3339_ms("2026-06-01T00:00:00.12Z"),
            Some(base + 120)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-06-01T00:00:00.1259Z"),
            Some(base + 125)
        );
    }

    #[test]
    fn the_writer_and_the_reader_agree_to_the_millisecond() {
        // `metadata.json`'s `created_at` is written by `rfc3339_utc_ms` and
        // read back by `parse_rfc3339_ms`; the projection's
        // `checkpoint.created_at` equals the capture event's own recorded time
        // exactly when these two are inverses. Swept over instants chosen to
        // hit every carry: epoch+1ms, a leap-day, both edges of a millisecond
        // decade, end-of-year, and the fixture clocks the suites already pin.
        for ms in [
            1,
            999,
            1_000,
            86_399_999,        // 1970-01-01T23:59:59.999Z
            951_782_400_000,   // 2000-02-29T00:00:00Z (leap day)
            1_700_000_000_000, // the parity/backends fixture clock
            1_756_000_000_000, // FIXTURE_CREATED_AT_MS
            1_780_272_000_000, // 2026-06-01T00:00:00Z
            1_767_225_599_999, // 2025-12-31T23:59:59.999Z
            4_102_444_800_000, // 2100-01-01T00:00:00Z (century, non-leap)
        ] {
            let text = crate::backend::rfc3339_utc_ms(ms);
            assert_eq!(
                parse_rfc3339_ms(&text),
                Some(ms),
                "round-trip broke at {ms} (rendered {text})"
            );
        }
        // …and the rendering itself is pinned once, so a formatting drift is a
        // visible diff and not just a still-round-tripping mystery.
        assert_eq!(
            crate::backend::rfc3339_utc_ms(1_700_000_000_000),
            "2023-11-14T22:13:20.000Z"
        );
    }
}
