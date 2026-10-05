//! The per-shard facts projected from a shard's header: the chain row, the
//! anchor commit, the archiving worktree and the session links.

use rift_core::{Result, Sha256Id};
use rift_recall_types::checkpoint::CheckpointMetadata;
use rift_store::{
    record_checkpoint_commit_if_new, record_checkpoint_if_new, record_checkpoint_worktree_if_new,
    record_session_in_checkpoint, CanonicalStore, CheckpointRecord, SessionInCheckpointRow,
};

use crate::backend::WorktreeLabel;
use crate::ids;

use super::created_at::created_at_for;
use super::report::{note, truncate_token, BridgeReport};
use super::sessions::SessionAgents;
use super::{IngestCtx, ShardHeader};

/// Checkpoint header → the predecessor check, anchor commit and archiving
/// worktree (idempotent), for a shard whose id is a SHA-256 and whose header
/// carries a Merkle root; [`seal_checkpoint`] writes the chain row itself.
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
pub(super) async fn record_chain_facts(
    store: &impl CanonicalStore,
    report: &mut BridgeReport,
    header: &ShardHeader<'_>,
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
    record_anchor_commit(store, report, header).await?;
    record_archiving_worktree(store, report, header).await
}

/// The shard's `checkpoint` row, written as the **last** row of its projection.
///
/// Last because the row is what tells a later walk the shard is projected
/// (`resume::already_projected`): written first, a crash between it and the
/// shard's events would leave a shard every later walk skips half-projected.
pub(super) async fn seal_checkpoint(
    store: &impl CanonicalStore,
    header: &ShardHeader<'_>,
    root: Sha256Id,
    prev: Option<Sha256Id>,
) -> Result<()> {
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
    Ok(())
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

/// Link every session the header names to this checkpoint, in header order,
/// each keyed under the agent the shard names for it (the walk's default
/// agent when it names none).
pub(super) async fn record_sessions<S: CanonicalStore, R>(
    ctx: &IngestCtx<'_, S, R>,
    id: &str,
    metadata: &CheckpointMetadata,
    agents: &SessionAgents,
) -> Result<()> {
    for (ordinal, session) in metadata.sessions.iter().enumerate() {
        let agent = agents.agent_of(session, ctx.default_agent);
        record_session_in_checkpoint(
            ctx.store,
            &SessionInCheckpointRow {
                checkpoint_id: id.to_owned(),
                session_id: ids::session_row_id(agent, session),
                ordinal: i64::try_from(ordinal).unwrap_or(i64::MAX),
                schema_version: 1,
            },
        )
        .await?;
    }
    Ok(())
}

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
pub(super) fn commit_oid_from_metadata(claim: Option<&str>) -> Option<String> {
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
