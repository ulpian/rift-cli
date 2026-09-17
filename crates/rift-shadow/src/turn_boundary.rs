//! Turn-boundary working-tree snapshots and the H_i/A_i attribution
//! enrichment.
//!
//! Two halves:
//!
//! * [`capture_boundary`] runs at a turn boundary (`start` closes the human's
//!   H_i window, `end` closes the agent's A_i window): it observes the working
//!   set (every tracked path differing from HEAD plus everything the previous
//!   boundary observed), diffs each file against the previous boundary's
//!   content, and persists append-only `turn_boundary_snapshot` /
//!   `turn_boundary_file` / `turn_diff_hunk` rows. File **bytes** go only into
//!   the local git object store (`hash-object -w`, the same trust class as the
//!   shadow branch); the projection gets hashes and hunk coordinates.
//!
//! * [`enrich_attribution`] runs at commit time: it replays the session's
//!   ordered `[H_0, A_0, H_1, A_1, …]` hunk stack (plus the synthesized final
//!   human window `last boundary → committed`) through the LIFO engine and
//!   replaces the commit capture's all-`unknown` attribution with real
//!   agent/human counts, per-hunk category rows, and a confidence that rises
//!   with capture completeness. [`attribution_preview`] is the pre-commit
//!   flavor the `prepare-commit-msg` trailer renders.
//!
//! # Why a line still answers `unknown` — the causes and their status (2026-08-29)
//!
//! Measured on real repositories (blame unknown on 96 of 96 lines), five causes
//! kept the replay from naming a writer. Each is either closed in this tree or
//! deliberately left, and this list is the register:
//!
//! * **A — trailer/anchor divergence.** A transcript-carrying session's
//!   predicted trailer never lands; the reader resolves through the union of
//!   the commit's claims and the shard's recorded anchor. CLOSED, and the
//!   resolution is permanent.
//! * **B — the `-1` created-at sentinel** ranking as resolved metadata. CLOSED.
//! * **C — End-only adapters never advanced their turn ordinal** (Antigravity:
//!   `Stop` closes a turn nothing opened), so every Stop after the first was
//!   dropped by the insert-or-ignore snapshot key and `open_window_actor`
//!   never resolved. CLOSED: adapters declare `rift_agent::TurnSignals` and
//!   the bridge synthesizes the missing Start (`rift-shadow::bridge`).
//! * **D — a file committed mid-session** replayed its already-consumed hunks
//!   again, over-lengthened the model, and degraded to `unknown`. CLOSED:
//!   the boundary that observed the commit's parent blob is the watermark
//!   (`consumed_through` / `rebase_on_parent` here).
//! * **E — boundary-snapshot failures were swallowed** on the hook path, so a
//!   session could lose its boundaries without a word. CLOSED: the failure is
//!   said once through the capture queue's note channel and `rift doctor`
//!   reports it (`bins/rift/src/hooks.rs::snapshot_boundaries`).
//!
//! Left by construction: **imports carry no boundary** — `rift import` lands
//! a read-only transcript with no working-tree observation, so its lines stay
//! `unknown` and the checkpoint says `import`. Left for a later pin: boundary
//! anchor blobs are unreachable objects and **gc-able** (`absent anchor →
//! unreadable → not an anchor` above); a `refs/rift/anchors/` pin would keep
//! them, and is not built. Installer drift is already pinned elsewhere.

use std::collections::BTreeMap;
use std::path::Path;

use rift_core::Result;
use rift_git::{BlobReader, RepoPaths};
use rift_shadow_attribution::replay::{committed_writers, replay, TurnEdit};
use rift_shadow_attribution::{
    attribute, confidence, diff_attributable, factors_from_signals, Actor, CommitCapture,
    HunkAttribution,
};
use rift_shadow_hooks::trailers::AttributionSummary;
use rift_store::{
    latest_turn_boundary_snapshot, record_turn_boundary_file, record_turn_boundary_snapshot,
    record_turn_diff_hunk, turn_boundary_counts, turn_boundary_files_for_session_path,
    turn_boundary_files_for_snapshot, turn_diff_hunks_for_session, CanonicalStore,
    TurnBoundaryFileRow, TurnBoundarySnapshotRow, TurnDiffHunkRow,
};

use crate::ids;

/// The attribution method version stamped on turn-replay rows (provenance).
pub const METHOD_VERSION: &str = "turn-replay@1";

/// Which boundary is being captured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    /// Turn start — closes the human's between-turns window (H_i).
    Start,
    /// Turn end — closes the agent's within-turn window (A_i).
    End,
}

impl Boundary {
    /// The stored boundary token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::End => "end",
        }
    }

    /// Whose edit window this boundary closes.
    #[must_use]
    pub fn actor(self) -> &'static str {
        match self {
            Self::Start => "human",
            Self::End => "agent",
        }
    }

    /// The `turn_diff_hunk.edit_kind` this boundary's diffs carry.
    #[must_use]
    fn edit_kind(self) -> &'static str {
        self.actor()
    }
}

/// The session's last boundary — **the row and the bytes it anchors, from ONE
/// ordered read.**
///
/// `None` when the session has no boundary at all. The content map is
/// `path → blob_oid` for exactly the returned snapshot, so a caller that needs
/// both the window's actor (the row's `boundary` token) and the anchor bytes
/// takes them from the same snapshot **by construction** rather than by running
/// the selection twice.
///
/// The selection itself lives in `rift-store`
/// ([`latest_turn_boundary_snapshot`]) and is not restated here. It used to be:
/// this function's `ORDER BY` and the store's differed only by a table alias,
/// so "actor and anchor come from the same snapshot" rested on two clauses in
/// two crates happening to match. Ties are impossible today, so nothing would
/// have caught a drift.
async fn last_boundary(
    store: &impl CanonicalStore,
    session_row: &str,
) -> Result<Option<(TurnBoundarySnapshotRow, BTreeMap<String, String>)>> {
    let Some(snapshot) = latest_turn_boundary_snapshot(store, session_row).await? else {
        return Ok(None);
    };
    let files = turn_boundary_files_for_snapshot(store, &snapshot.snapshot_id).await?;
    Ok(Some((
        snapshot,
        files.into_iter().map(|r| (r.path, r.blob_oid)).collect(),
    )))
}

/// The previous boundary's content map (`path → blob_oid`), or empty if this is
/// the session's first boundary. The capture side needs the bytes only.
async fn previous_state(
    store: &impl CanonicalStore,
    session_row: &str,
) -> Result<BTreeMap<String, String>> {
    Ok(last_boundary(store, session_row)
        .await?
        .map(|(_, files)| files)
        .unwrap_or_default())
}

/// Read one worktree file (empty when deleted/absent). Only regular files are
/// observed; anything unreadable is treated as absent.
fn worktree_bytes(work_tree: &Path, rel: &str) -> Vec<u8> {
    std::fs::read(work_tree.join(rel)).unwrap_or_default()
}

fn sha256_hex_of(bytes: &[u8]) -> String {
    crate::sha256_hex(&[bytes])
}

/// Capture one turn boundary: snapshot the working set and persist the closed
/// window's H_i/A_i hunks. Idempotent per `(session, ordinal, boundary)`.
///
/// # Errors
/// [`rift_core::RiftError`] on a git or storage failure (callers at the hook
/// boundary swallow it — capture never blocks the agent).
#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // one linear observe→diff→persist pass
pub async fn capture_boundary(
    store: &impl CanonicalStore,
    repo: &RepoPaths,
    reader: &impl BlobReader,
    head_commit: Option<&str>,
    agent: &str,
    native_session: &str,
    turn_ordinal: u32,
    boundary: Boundary,
    now_ms: i64,
) -> Result<()> {
    let session_row = ids::session_row_id(agent, native_session);
    let prev = previous_state(store, &session_row).await?;

    // The observation set: everything currently differing from HEAD, plus every
    // path the previous boundary observed (a file reverted since then still
    // needs its closing diff).
    let mut paths: Vec<String> = rift_git::changed_worktree_paths(repo)?;
    paths.extend(prev.keys().cloned());
    paths.sort();
    paths.dedup();

    // HEAD tree for baseline reads (first boundary of a path).
    let head_tree: BTreeMap<String, String> = match head_commit {
        Some(head) => reader
            .tree_entries_at(head)?
            .into_iter()
            .map(|e| (e.path, e.blob_oid))
            .collect(),
        None => BTreeMap::new(),
    };

    let snapshot_id = ids::turn_boundary_snapshot_id(&session_row, turn_ordinal, boundary.as_str());
    let mut observed: Vec<(String, String, String)> = Vec::new(); // (path, blob_oid, sha256)

    for path in &paths {
        let cur = worktree_bytes(&repo.work_tree, path);

        // ★ ABSENCE IS NOT EMPTINESS — the WRITE side of the rule the replay
        // below already follows.
        //
        // `read_blob` answers `None` for a blob that is gone. Turn-boundary
        // anchors are unreachable objects written by `hash-object -w`, so git
        // may collect them at any time, and a HEAD blob can be missing from a
        // partial clone. An earlier `unwrap_or_default()` turned that `None`
        // into empty bytes, which reads as "the file did not exist at the
        // anchor" — so an UNTOUCHED file diffs as a whole-file creation and a
        // phantom hunk lands in append-only `turn_diff_hunk`, crediting every
        // line of it to whoever's window is closing. Downstream cannot catch
        // it: on an added file the replay base is 0, so the length check that
        // guards the replay is satisfied by construction.
        //
        // `None` here therefore means the window's diff is UNKNOWABLE, and the
        // honest record of an unknowable diff is no row at all.
        //
        // Absent-from-the-tree is a DIFFERENT fact and stays an empty
        // baseline: a path missing from HEAD genuinely did not exist, and its
        // creation is a real edit this window is entitled to record.
        let anchor: Option<Vec<u8>> = match prev.get(path) {
            Some(oid) => reader.read_blob(oid)?,
            None => match head_tree.get(path) {
                Some(oid) => reader.read_blob(oid)?,
                None => Some(Vec::new()),
            },
        };
        let Some(prev_bytes) = anchor else {
            // The file is still OBSERVED — only the diff is unknowable. Its
            // current bytes go to the object store and into the boundary
            // state, so the next window has a readable anchor and the
            // blindness lasts exactly one window instead of orphaning the
            // path forever. (When the content matches HEAD again, the fresh
            // oid IS the HEAD oid — content addressing — so the path drops out
            // of the state by the existing unchanged-file rule.)
            let blob_oid = rift_git::hash_object_write(repo, &cur)?;
            observed.push((path.clone(), blob_oid, sha256_hex_of(&cur)));
            continue;
        };

        if cur == prev_bytes {
            // Unchanged within this window; still part of the boundary state if
            // it differs from HEAD (so the next window diffs against it).
            if head_tree.get(path).map(String::as_str)
                != Some(prev.get(path).map_or("", String::as_str))
                && prev.contains_key(path)
            {
                observed.push((
                    path.clone(),
                    prev.get(path).cloned().unwrap_or_default(),
                    sha256_hex_of(&cur),
                ));
            }
            continue;
        }

        // The closed window's hunks for this file (binary/oversize → no hunks,
        // the file still joins the boundary state).
        if let Some(hunks) = diff_attributable(&prev_bytes, &cur) {
            for h in hunks {
                record_turn_diff_hunk(
                    store,
                    &TurnDiffHunkRow {
                        hunk_id: ids::turn_diff_hunk_id(
                            &session_row,
                            turn_ordinal,
                            boundary.edit_kind(),
                            path,
                            h.old_start,
                            h.old_len,
                            h.new_start,
                            h.new_len,
                        ),
                        session_id: session_row.clone(),
                        turn_ordinal: i64::from(turn_ordinal),
                        edit_kind: boundary.edit_kind().to_owned(),
                        path: path.clone(),
                        old_start: i64::from(h.old_start),
                        old_len: i64::from(h.old_len),
                        new_start: i64::from(h.new_start),
                        new_len: i64::from(h.new_len),
                        schema_version: 1,
                    },
                )
                .await?;
            }
        }

        // The file's bytes at this boundary go into the local object store; the
        // projection gets only the addresses.
        let blob_oid = rift_git::hash_object_write(repo, &cur)?;
        observed.push((path.clone(), blob_oid, sha256_hex_of(&cur)));
    }

    // The whole-boundary content hash: over the sorted (path, sha256) set.
    let mut acc: Vec<u8> = Vec::new();
    for (path, _oid, sha) in &observed {
        acc.extend_from_slice(path.as_bytes());
        acc.push(0);
        acc.extend_from_slice(sha.as_bytes());
        acc.push(b'\n');
    }
    let worktree_hash = sha256_hex_of(&acc);

    record_turn_boundary_snapshot(
        store,
        &TurnBoundarySnapshotRow {
            snapshot_id: snapshot_id.clone(),
            session_id: session_row.clone(),
            turn_ordinal: i64::from(turn_ordinal),
            boundary: boundary.as_str().to_owned(),
            actor: boundary.actor().to_owned(),
            worktree_hash,
            captured_at: now_ms,
            schema_version: 1,
        },
    )
    .await?;
    for (path, blob_oid, content_sha256) in observed {
        record_turn_boundary_file(
            store,
            &TurnBoundaryFileRow {
                snapshot_id: snapshot_id.clone(),
                path,
                blob_oid,
                content_sha256,
                schema_version: 1,
            },
        )
        .await?;
    }
    Ok(())
}

/// Count the lines of a byte buffer the way the diff engine does.
#[allow(clippy::naive_bytecount)] // tiny inputs; a bytecount dep is not warranted
fn line_count(bytes: &[u8]) -> u32 {
    if bytes.is_empty() {
        return 0;
    }
    let n = bytes.iter().filter(|b| **b == b'\n').count();
    let tail = usize::from(!bytes.ends_with(b"\n"));
    u32::try_from(n + tail).unwrap_or(u32::MAX)
}

/// Convert stored rows into the replay's per-file edit stacks (rows arrive in
/// replay order from the store query).
/// The position of one recorded hunk in a session's replay order: the turn
/// it closed, then the window kind in the store's own order (`human`/`start`
/// = 0 before `agent`/`end` = 1). A boundary is addressed by the same key, so
/// a hunk with key ≤ a boundary's key was already visible at that boundary.
type StackKey = (i64, u8);

/// The window-kind rank of a hunk's `edit_kind` or a boundary's token.
fn kind_rank(token: &str) -> u8 {
    u8::from(matches!(token, "agent" | "end"))
}

fn edit_stacks(rows: &[TurnDiffHunkRow]) -> BTreeMap<String, Vec<(StackKey, TurnEdit)>> {
    let mut map: BTreeMap<String, Vec<(StackKey, TurnEdit)>> = BTreeMap::new();
    for r in rows {
        let actor = if r.edit_kind == "agent" {
            Actor::Agent
        } else {
            Actor::Human
        };
        let clamp = |v: i64| u32::try_from(v).unwrap_or(0);
        map.entry(r.path.clone()).or_default().push((
            (r.turn_ordinal, kind_rank(&r.edit_kind)),
            TurnEdit {
                actor,
                hunk: rift_shadow_attribution::Hunk {
                    old_start: clamp(r.old_start),
                    old_len: clamp(r.old_len),
                    new_start: clamp(r.new_start),
                    new_len: clamp(r.new_len),
                },
            },
        ));
    }
    map
}

/// **The watermark a mid-session commit leaves.** Given the
/// boundaries at which this session observed a file (`(turn, boundary,
/// blob_oid)`, oldest first) and the blob the current commit diffed against,
/// the LATEST boundary whose observed blob IS that parent: every hunk recorded
/// at or before it was already committed by an earlier commit, so replaying it
/// again over the real parent double-applies it (the stack over-lengthens the
/// model and `committed_writers` degrades the whole file to `unknown`). `None`
/// when no boundary observed the parent — the session never saw that content,
/// so there is nothing honest to rebase on and the caller keeps today's
/// behaviour.
fn consumed_through(rows: &[(i64, String, String)], base_oid: &str) -> Option<StackKey> {
    rows.iter()
        .filter(|(_, _, oid)| oid == base_oid)
        .map(|(turn, boundary, _)| (*turn, kind_rank(boundary)))
        .max()
}

/// The hunks NOT yet consumed by an earlier commit: everything whose key is
/// after `watermark`. Order is preserved.
fn stack_after(edits: Vec<(StackKey, TurnEdit)>, watermark: StackKey) -> Vec<(StackKey, TurnEdit)> {
    edits.into_iter().filter(|(k, _)| *k > watermark).collect()
}

/// Whose edit window is open at the commit, or `None` when the boundary record
/// is provably incomplete and the question cannot be answered honestly.
///
/// A `start` closes `H_i` and opens the agent's `A_i`; an `end` closes `A_i` and
/// opens the next human window. So the last boundary's own token *is* the
/// answer — `start` → the agent is still in flight, `end` → the window is the
/// human's.
///
/// The counts are then used **only to detect disagreement, never to decide**.
/// Under a complete record a trailing `start` implies exactly one unmatched
/// start (`starts == ends + 1`) and a trailing `end` implies none
/// (`starts == ends`). When the ordered token and the counts contradict that,
/// at least one boundary hook was dropped — hooks are best-effort by
/// construction — so the record is known-incomplete and this
/// returns `None`. The caller then refuses to synthesize the window and the
/// lines stay `unknown`.
///
/// Refusing on disagreement rather than believing either source is the whole
/// point: picking one would resolve the contradiction by guessing an author,
/// and a wrong author in an append-only table is permanent.
fn open_window_actor(last_boundary: &str, starts: i64, ends: i64) -> Option<Actor> {
    match last_boundary {
        "start" if starts == ends + 1 => Some(Actor::Agent),
        "end" if starts == ends => Some(Actor::Human),
        _ => None,
    }
}

/// A file already committed mid-session. If a boundary observed
/// exactly the blob this commit diffed against, the hunks up to that boundary
/// were consumed by the earlier commit — drop them and replay from the REAL
/// parent's length (returned as `Some`), so the length guard in
/// [`enrich_attribution`] stays meaningful instead of failing the whole file
/// to `unknown`. No such boundary → the stack is returned untouched and the
/// caller keeps today's behaviour (honest unknown).
async fn rebase_on_parent(
    store: &impl CanonicalStore,
    reader: &impl BlobReader,
    session_row: &str,
    f: &rift_shadow_attribution::FileChange,
    keyed: Vec<(StackKey, TurnEdit)>,
) -> Result<(Vec<TurnEdit>, Option<u32>)> {
    let mut keyed = keyed;
    let mut parent_len = None;
    if let Some(base_oid) = f.base_blob_oid.as_deref() {
        let observed = turn_boundary_files_for_session_path(store, session_row, &f.path).await?;
        if let Some(watermark) = consumed_through(&observed, base_oid) {
            keyed = stack_after(keyed, watermark);
            parent_len = reader.read_blob(base_oid)?.as_deref().map(line_count);
        }
    }
    Ok((keyed.into_iter().map(|(_, e)| e).collect(), parent_len))
}

/// Per-hunk category rows (the irreducible facts the rollup sums): for each
/// committed hunk, how many of its new lines the replay credits to
/// `agent_added`, `human_added`, `human_modified_agent` or `unknown`.
fn hunk_categories(
    writers: &[Vec<Actor>],
    hunks: &[rift_shadow_attribution::Hunk],
) -> Vec<HunkAttribution> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    for (ordinal, h) in hunks.iter().enumerate() {
        let mut counts: BTreeMap<&str, u32> = BTreeMap::new();
        for w in writers.iter().skip(cursor).take(h.new_len as usize) {
            let cat = match w.last() {
                None => "unknown",
                Some(Actor::Agent) => "agent_added",
                Some(Actor::Human) => {
                    if w.contains(&Actor::Agent) {
                        "human_modified_agent"
                    } else {
                        "human_added"
                    }
                }
            };
            *counts.entry(cat).or_default() += 1;
        }
        cursor += h.new_len as usize;
        for (category, line_count) in counts {
            out.push(HunkAttribution {
                hunk_ordinal: u32::try_from(ordinal).unwrap_or(u32::MAX),
                new_start: h.new_start,
                new_len: h.new_len,
                category: category.to_owned(),
                line_count,
            });
        }
    }
    out
}

/// Enrich a commit capture with turn-boundary attribution: replay each file's
/// `[H_0, A_0, …]` stack plus the synthesized final window (last boundary →
/// committed content, credited to whoever's window is still open), score it
/// through the LIFO engine, and replace the all-`unknown` commit-boundary
/// attribution.
///
/// **A file is enriched only when the session gives it a content anchor** —
/// either recorded turn hunks or an observed boundary blob. A file with neither
/// keeps its honest `unknown` counts and its `commit-diff@1` stamp: nothing was
/// observed about who wrote it, and an append-only ledger must not be told
/// otherwise (see the `★` note at the guard).
///
/// # Errors
/// [`rift_core::RiftError`] on a storage or blob-read failure.
pub async fn enrich_attribution(
    store: &impl CanonicalStore,
    reader: &impl BlobReader,
    agent: &str,
    native_session: &str,
    capture: &mut CommitCapture,
) -> Result<()> {
    let session_row = ids::session_row_id(agent, native_session);
    let rows = turn_diff_hunks_for_session(store, &session_row).await?;
    // ONE read: the snapshot whose token names the open window's actor IS the
    // snapshot whose files supply the anchor bytes. Not two reads of a rule
    // written twice — see [`last_boundary`].
    let last = last_boundary(store, &session_row).await?;
    let boundary_state = last
        .as_ref()
        .map(|(_, files)| files.clone())
        .unwrap_or_default();
    // Nothing observed at all for this session — no hunks AND no boundary state.
    // There is no anchor to replay against, so leave every file honest.
    if rows.is_empty() && boundary_state.is_empty() {
        return Ok(());
    }
    let mut stacks = edit_stacks(&rows);
    let (starts, ends) = turn_boundary_counts(store, &session_row).await?;
    // `None` = the boundary record contradicts itself (a dropped hook), so no
    // final window is synthesized at all for any file in this capture.
    let pending_actor = last
        .as_ref()
        .and_then(|(snapshot, _)| open_window_actor(&snapshot.boundary, starts, ends));
    // Coverage: a fully-bracketed session has an `end` for every `start`.
    let coverage = if starts == 0 {
        0.0
    } else {
        #[allow(clippy::cast_precision_loss)]
        let c = ends as f64 / starts as f64;
        c.clamp(0.0, 1.0)
    };

    for f in &mut capture.files {
        if !f.attributable {
            continue;
        }
        let keyed = stacks.remove(&f.path).unwrap_or_default();
        let (mut edits, parent_len) =
            rebase_on_parent(store, reader, &session_row, f, keyed).await?;
        // An anchor is only usable if the window it opens has a known owner: a
        // contradictory boundary record (`pending_actor == None`) makes the
        // anchor bytes readable but the authorship unknowable.
        let anchored = boundary_state.contains_key(&f.path) && pending_actor.is_some();
        // ★ NEVER INVENT AN ANCHOR. Recorded hunks and a usable boundary anchor
        // are the only two things that tie a committed line to a writer. With
        // neither, the file changed at a time this session never observed — it
        // may predate the session fully — so it stays `unknown` under
        // `commit-diff@1`, untouched by the replay. A plausible guess written
        // into an append-only canonical table is a permanent false fact, and
        // strictly worse than a blank: `blame` would name an author no evidence
        // supports.
        if edits.is_empty() && !anchored {
            continue;
        }
        // An unreadable committed blob is ABSENCE, not emptiness. Defaulting it
        // would make a file with content look like a file with none.
        let Some(committed) = reader.read_blob(&f.head_blob_oid)? else {
            continue;
        };
        let committed_len = line_count(&committed);

        // Synthesize the still-open window: last observed boundary state → the
        // committed content. Credited to whoever's window is open (an agent
        // committing mid-turn is the agent's work, not the human's).
        //
        // ★ ABSENCE IS NOT EMPTINESS. `read_blob` returns `None` for a blob that
        // is gone — turn-boundary blobs are unreachable objects written by
        // `hash-object -w`, so git may garbage-collect them at any time. An
        // earlier `unwrap_or_default()` here turned that `None` into empty
        // bytes, which reads as "the file did not exist at the boundary" and
        // credits its full content to the open window. On an ADDED file the
        // replay base is 0, so the downstream length check cannot fire and the
        // false credit lands in `attribution`. A collected anchor is an
        // unreadable anchor: the file is treated as unanchored and left
        // `unknown`.
        if let (true, Some(oid), Some(actor)) =
            (anchored, boundary_state.get(&f.path), pending_actor)
        {
            let Some(last_bytes) = reader.read_blob(oid)? else {
                continue; // collected anchor → unreadable → not an anchor
            };
            if last_bytes != committed {
                if let Some(hunks) = diff_attributable(&last_bytes, &committed) {
                    edits.extend(hunks.into_iter().map(|hunk| TurnEdit { actor, hunk }));
                }
            }
        }

        // The replay base is the commit's parent-side line count (exact from
        // diff conservation: parent = committed − net insertions).
        let net: i64 = f
            .hunks
            .iter()
            .map(|h| i64::from(h.new_len) - i64::from(h.old_len))
            .sum();
        let base_len = parent_len
            .unwrap_or_else(|| u32::try_from(i64::from(committed_len) - net).unwrap_or(0));

        // What the downstream guard does and does NOT protect. On divergence or
        // a length mismatch `committed_writers` returns empty histories, so a
        // composition whose LINE COUNTS do not add up degrades to `unknown`.
        //
        // It constrains counts, not authorship: it compares
        // `histories.len()` against `committed_len` and cannot observe WHO a
        // history names, so it will pass a perfectly-shaped replay that credits
        // the wrong actor. Every actor this function supplies must therefore be
        // sound BEFORE it reaches here — which is why the open window's owner
        // comes from one ordered source and is abandoned on contradiction, and
        // why an unreadable anchor disqualifies the file. Do not weaken either
        // of those on the strength of this guard.
        //
        // The guard is also blind on ADDED files: their replay base is 0, so a
        // fabricated full-file authorship has the right length by construction.
        // `absent_anchor_blob_on_added_file_is_refused` is the test that fails
        // if that stops being handled upstream.
        let replayed = replay(base_len, &edits);
        let writers = committed_writers(&replayed, &f.hunks, committed_len);
        let attribution = attribute(&writers, replayed.agent_removed_by_human);

        let unknown_ratio = if attribution.total_committed() == 0 {
            1.0
        } else {
            f64::from(attribution.unknown_lines) / f64::from(attribution.total_committed())
        };
        let factors = factors_from_signals(unknown_ratio, false, false, false, true);
        let file_confidence = (confidence(&factors) * coverage.max(0.5)).clamp(0.0, 1.0);

        let hunk_categories = hunk_categories(&writers, &f.hunks);
        f.attribution = attribution;
        f.confidence = file_confidence;
        f.hunk_categories = hunk_categories;
        METHOD_VERSION.clone_into(&mut f.method_version);
    }
    Ok(())
}

/// The pre-commit attribution preview the `prepare-commit-msg` trailer renders:
/// LIFO tallies over the session's turn hunks alone (no committed diff yet).
/// `None` when the session has produced no turn edits.
///
/// # Errors
/// [`rift_core::RiftError`] on a storage failure.
pub async fn attribution_preview(
    store: &impl CanonicalStore,
    agent: &str,
    native_session: &str,
) -> Result<Option<AttributionSummary>> {
    let session_row = ids::session_row_id(agent, native_session);
    let rows = turn_diff_hunks_for_session(store, &session_row).await?;
    if rows.is_empty() {
        return Ok(None);
    }
    let (starts, ends) = turn_boundary_counts(store, &session_row).await?;

    let mut agent_lines: u64 = 0;
    let mut human_lines: u64 = 0;
    for (_path, keyed) in edit_stacks(&rows) {
        // The preview is pre-commit: there is no parent to rebase on yet, so
        // the whole stack replays and the keys are not needed.
        let edits: Vec<TurnEdit> = keyed.into_iter().map(|(_, e)| e).collect();
        // Replay over a generous base so session-relative coordinates fit; only
        // session-written lines (non-empty histories) are tallied.
        let max_extent = edits
            .iter()
            .map(|e| e.hunk.old_start.saturating_add(e.hunk.old_len))
            .max()
            .unwrap_or(0);
        let replayed = replay(max_extent, &edits);
        for h in &replayed.histories {
            match h.last() {
                None => {}
                Some(Actor::Agent) => agent_lines += 1,
                Some(Actor::Human) => human_lines += 1,
            }
        }
    }
    let total = agent_lines + human_lines;
    if total == 0 {
        return Ok(None);
    }
    let pct = |n: u64| u8::try_from(n * 100 / total).unwrap_or(100);
    // Confidence: the whole-turn granularity factor, scaled by boundary
    // coverage (every started turn also observed its end).
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let confidence_pct = if starts == 0 {
        0u8
    } else {
        ((ends as f64 / starts as f64).clamp(0.0, 1.0) * 80.0).round() as u8
    };
    Ok(Some(AttributionSummary {
        agent_pct: pct(agent_lines),
        human_pct: pct(human_lines),
        confidence_pct,
    }))
}

/// [`capture_boundary`]'s first tests — driven through the REAL function
/// against a REAL git repository, with only the [`BlobReader`] seam faking a
/// collected blob. Everything else is genuine: `git status` names the working
/// set, `hash-object -w` writes the anchors, and the rows land in a real
/// append-only projection.
///
/// The write side had no tests at all before this module, which is how an
/// absence-as-emptiness bug survived 270 lines above the comment declaring the
/// same class fixed.
#[cfg(test)]
mod capture_boundary_tests {
    use std::collections::HashSet;
    use std::path::Path;
    use std::process::Command;

    use super::*;
    use rift_git::{BlobReader, GixBlobReader, RepoPaths, TreeEntry};
    use rift_store::SqliteStore;

    const AGENT: &str = "claude-code";
    const NATIVE: &str = "sess-b";

    /// The production reader over a real repo, with a chosen set of oids made
    /// to look **collected**. Turn-boundary blobs are unreachable objects
    /// written by `hash-object -w`, so git may garbage-collect them at any
    /// time and `read_blob` then answers `None`. This is the ONLY thing faked:
    /// the repository, its objects, and every other read are real.
    struct Collected {
        inner: GixBlobReader,
        hidden: HashSet<String>,
    }

    impl BlobReader for Collected {
        fn read_blob(&self, oid_hex: &str) -> Result<Option<Vec<u8>>> {
            if self.hidden.contains(oid_hex) {
                return Ok(None);
            }
            self.inner.read_blob(oid_hex)
        }
        fn tree_entries_at(&self, commit_hex: &str) -> Result<Vec<TreeEntry>> {
            self.inner.tree_entries_at(commit_hex)
        }
        fn parent_of(&self, commit_hex: &str) -> Result<Option<String>> {
            self.inner.parent_of(commit_hex)
        }
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// A real repo with one commit (`a.txt` = "one\ntwo\n"), plus its paths.
    fn repo_with_a_commit(dir: &Path) -> RepoPaths {
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "t@example.invalid"]);
        git(dir, &["config", "user.name", "T"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("a.txt"), b"one\ntwo\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-q", "-m", "seed"]);
        rift_git::discover(dir).unwrap()
    }

    fn reader(dir: &Path, hidden: &[&str]) -> Collected {
        Collected {
            inner: GixBlobReader::open(dir).unwrap(),
            hidden: hidden.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        s
    }

    /// Every hunk row the session recorded, as `(edit_kind, path, old_start,
    /// old_len, new_start, new_len)` — the shape a `blame` answer is built from.
    async fn hunks(s: &SqliteStore) -> Vec<(String, String, i64, i64, i64, i64)> {
        let session = ids::session_row_id(AGENT, NATIVE);
        let rows = turn_diff_hunks_for_session(s, &session).await.unwrap();
        rows.into_iter()
            .map(|r| {
                (
                    r.edit_kind,
                    r.path,
                    r.old_start,
                    r.old_len,
                    r.new_start,
                    r.new_len,
                )
            })
            .collect()
    }

    /// The paths the session's LATEST boundary anchors, with their oids — read
    /// through the same one-home selection the production code uses.
    async fn anchors(s: &SqliteStore) -> Vec<(String, String)> {
        let session = ids::session_row_id(AGENT, NATIVE);
        previous_state(s, &session)
            .await
            .unwrap()
            .into_iter()
            .collect()
    }

    /// ★ THE PHANTOM HUNK. Two `start` boundaries with the file untouched on
    /// disk between them, and the first boundary's anchor blob collected in
    /// between.
    ///
    /// With `read_blob(..)?.unwrap_or_default()` the absent anchor reads as
    /// EMPTY, so the unchanged file diffs as a whole-file creation and a
    /// `human added.txt 0 0 -> 0 2` row lands in append-only `turn_diff_hunk`
    /// for a file nobody edited. Restoring `unwrap_or_default()` at either read
    /// site reds this test.
    ///
    /// The second half matters as much: the file must still be ANCHORED by the
    /// new boundary, or refusing the hunk would blind every later window too.
    #[tokio::test]
    async fn a_collected_anchor_writes_no_hunk_and_still_anchors_the_next_window() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_with_a_commit(tmp.path());
        let head = git(tmp.path(), &["rev-parse", "HEAD"]);
        std::fs::write(tmp.path().join("added.txt"), b"x\ny\n").unwrap();
        let s = store();

        // Turn 0 opens: the untracked file is observed and its bytes anchored.
        capture_boundary(
            &s,
            &repo,
            &reader(tmp.path(), &[]),
            Some(&head),
            AGENT,
            NATIVE,
            0,
            Boundary::Start,
            1,
        )
        .await
        .unwrap();
        let anchored = anchors(&s).await;
        let oid = anchored
            .iter()
            .find(|(p, _)| p == "added.txt")
            .map(|(_, o)| o.clone())
            .expect("added.txt is anchored by the first boundary");
        // Turn 0 legitimately records the file's creation (it is absent from
        // HEAD). That row is real; the question is whether turn 1 invents a
        // SECOND one out of an unreadable anchor.
        let after_turn_0 = hunks(&s).await;
        assert_eq!(
            after_turn_0,
            vec![("human".into(), "added.txt".into(), 0, 0, 0, 2)],
            "the creation itself is a real fact"
        );

        // Turn 1 opens with that anchor COLLECTED and the file untouched.
        capture_boundary(
            &s,
            &repo,
            &reader(tmp.path(), &[&oid]),
            Some(&head),
            AGENT,
            NATIVE,
            1,
            Boundary::Start,
            2,
        )
        .await
        .unwrap();

        assert_eq!(
            hunks(&s).await,
            after_turn_0,
            "an unreadable anchor is ABSENCE, not empty content: no hunk may be \
             invented for a file nobody touched (the phantom row this reds on is \
             `human added.txt 0 0 -> 0 2`, a second whole-file creation)"
        );
        let after: Vec<String> = anchors(&s).await.into_iter().map(|(p, _)| p).collect();
        assert!(
            after.contains(&"added.txt".to_owned()),
            "the file must still be anchored, or the NEXT window is blind too: {after:?}"
        );
        let fresh = anchors(&s)
            .await
            .into_iter()
            .find(|(p, _)| p == "added.txt")
            .map(|(_, o)| o)
            .unwrap();
        // Content addressing means the replacement anchor for unchanged bytes
        // IS the same oid — and `hash-object -w` has put the object back, so
        // the blindness lasts exactly this one window. (The reader below does
        // not fake anything: the collection was a fake, the recovery is real.)
        assert_eq!(fresh, oid, "same bytes, same oid");
        assert!(
            reader(tmp.path(), &[]).read_blob(&fresh).unwrap().is_some(),
            "the anchor object is back in the store — the next window can diff"
        );
    }

    /// The healthy path, so the refusal above cannot be met by refusing
    /// everything: two boundaries over two REAL edits record exactly those two
    /// edits, on the right side of the human/agent split.
    #[tokio::test]
    async fn two_boundaries_over_real_edits_record_exactly_those_edits() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_with_a_commit(tmp.path());
        let head = git(tmp.path(), &["rev-parse", "HEAD"]);
        let s = store();

        // The human appended a line before the turn opened.
        std::fs::write(tmp.path().join("a.txt"), b"one\ntwo\nthree\n").unwrap();
        capture_boundary(
            &s,
            &repo,
            &reader(tmp.path(), &[]),
            Some(&head),
            AGENT,
            NATIVE,
            0,
            Boundary::Start,
            1,
        )
        .await
        .unwrap();

        // The agent appended another during the turn.
        std::fs::write(tmp.path().join("a.txt"), b"one\ntwo\nthree\nfour\n").unwrap();
        capture_boundary(
            &s,
            &repo,
            &reader(tmp.path(), &[]),
            Some(&head),
            AGENT,
            NATIVE,
            0,
            Boundary::End,
            2,
        )
        .await
        .unwrap();

        let recorded = hunks(&s).await;
        assert_eq!(
            recorded.len(),
            2,
            "one hunk per closed window, no more: {recorded:?}"
        );
        assert_eq!(recorded[0].0, "human", "the pre-turn edit is the human's");
        assert_eq!(recorded[0].1, "a.txt");
        assert_eq!(recorded[1].0, "agent", "the in-turn edit is the agent's");
        assert_eq!(recorded[1].1, "a.txt");
        // Each window added exactly one line to a file that had one more each time.
        assert_eq!(
            (recorded[0].3, recorded[0].5),
            (0, 1),
            "window 1 inserted one line and removed none"
        );
        assert_eq!(
            (recorded[1].3, recorded[1].5),
            (0, 1),
            "window 2 inserted one line and removed none"
        );
    }

    /// A file whose HEAD blob is unreadable is the same class: the baseline for
    /// its FIRST window comes from the HEAD tree, and defaulting a collected
    /// HEAD blob to empty credits the whole file to whoever's window is open.
    /// This is the second `unwrap_or_default()` at the same site.
    #[tokio::test]
    async fn a_collected_head_blob_is_absence_too() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_with_a_commit(tmp.path());
        let head = git(tmp.path(), &["rev-parse", "HEAD"]);
        let head_blob = git(tmp.path(), &["rev-parse", "HEAD:a.txt"]);
        let s = store();

        std::fs::write(tmp.path().join("a.txt"), b"one\ntwo\nthree\n").unwrap();
        capture_boundary(
            &s,
            &repo,
            &reader(tmp.path(), &[&head_blob]),
            Some(&head),
            AGENT,
            NATIVE,
            0,
            Boundary::Start,
            1,
        )
        .await
        .unwrap();

        assert_eq!(
            hunks(&s).await,
            Vec::new(),
            "an unreadable HEAD blob must not make a 2-line edit look like a \
             3-line creation"
        );
        assert!(
            anchors(&s).await.iter().any(|(p, _)| p == "a.txt"),
            "the file is still observed — only the DIFF is unknowable"
        );
    }

    /// A genuinely new file (absent from HEAD, not merely unreadable) still
    /// records its creation. Absence-of-the-blob and absence-from-the-tree are
    /// different facts and only one of them is a refusal.
    #[tokio::test]
    async fn a_file_absent_from_head_still_records_its_creation() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_with_a_commit(tmp.path());
        let head = git(tmp.path(), &["rev-parse", "HEAD"]);
        let s = store();

        std::fs::write(tmp.path().join("new.txt"), b"alpha\nbeta\n").unwrap();
        capture_boundary(
            &s,
            &repo,
            &reader(tmp.path(), &[]),
            Some(&head),
            AGENT,
            NATIVE,
            0,
            Boundary::Start,
            1,
        )
        .await
        .unwrap();

        let recorded = hunks(&s).await;
        assert_eq!(recorded.len(), 1, "the creation is one hunk: {recorded:?}");
        assert_eq!(recorded[0].0, "human");
        assert_eq!(recorded[0].1, "new.txt");
        assert_eq!(
            (recorded[0].3, recorded[0].5),
            (0, 2),
            "two lines created out of nothing"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_git::{MemoryBlobReader, TreeEntry};
    use rift_shadow_attribution::capture_commit;
    use rift_store::SqliteStore;

    const AGENT: &str = "claude-code";
    const NATIVE: &str = "sess-1";
    const COMMIT_STUB: &str = rift_shadow_attribution::capture::METHOD_VERSION;

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        s
    }

    fn entry(path: &str, oid: &str) -> TreeEntry {
        TreeEntry {
            path: path.into(),
            blob_oid: oid.into(),
        }
    }

    /// parent `c0`: `a.txt` = "one\ntwo\n"; commit `c1` appends "three\n" and
    /// ADDS `added.txt`. `other.txt` is added by `c1` and never observed.
    fn reader() -> MemoryBlobReader {
        MemoryBlobReader::new()
            .with_blob("A0", b"one\ntwo\n")
            .with_blob("A1", b"one\ntwo\nthree\n")
            .with_blob("O1", b"unobserved\n")
            .with_blob("ADD1", b"x\ny\n")
            .with_tree("c0", vec![entry("a.txt", "A0")])
            .with_tree(
                "c1",
                vec![
                    entry("a.txt", "A1"),
                    entry("other.txt", "O1"),
                    entry("added.txt", "ADD1"),
                ],
            )
            .with_parent("c1", "c0")
    }

    /// Record the given boundaries in order; the anchor file `(path, blob_oid)`
    /// is attached to the LAST one recorded — the snapshot the ordered read
    /// selects, so the actor and the anchor always come from the same row.
    async fn seed(s: &SqliteStore, boundaries: &[(u32, Boundary)], anchor: (&str, &str)) {
        let session_row = ids::session_row_id(AGENT, NATIVE);
        let mut last = String::new();
        for (ordinal, b) in boundaries {
            let snapshot_id = ids::turn_boundary_snapshot_id(&session_row, *ordinal, b.as_str());
            record_turn_boundary_snapshot(
                s,
                &TurnBoundarySnapshotRow {
                    snapshot_id: snapshot_id.clone(),
                    session_id: session_row.clone(),
                    turn_ordinal: i64::from(*ordinal),
                    boundary: b.as_str().to_owned(),
                    actor: b.actor().to_owned(),
                    worktree_hash: "wh".into(),
                    captured_at: 1,
                    schema_version: 1,
                },
            )
            .await
            .unwrap();
            last = snapshot_id;
        }
        record_turn_boundary_file(
            s,
            &TurnBoundaryFileRow {
                snapshot_id: last,
                path: anchor.0.to_owned(),
                blob_oid: anchor.1.to_owned(),
                content_sha256: "sha".into(),
                schema_version: 1,
            },
        )
        .await
        .unwrap();
    }

    async fn enrich(s: &SqliteStore, r: &MemoryBlobReader) -> CommitCapture {
        let mut capture = capture_commit(r, "c1").unwrap();
        enrich_attribution(s, r, AGENT, NATIVE, &mut capture)
            .await
            .unwrap();
        capture
    }

    fn file<'a>(c: &'a CommitCapture, path: &str) -> &'a rift_shadow_attribution::FileChange {
        c.files.iter().find(|f| f.path == path).expect("file")
    }

    // ----- a file committed mid-session -----------------------------------

    /// One boundary snapshot with one observed file row.
    async fn boundary_with_file(
        s: &SqliteStore,
        ordinal: u32,
        b: Boundary,
        path: &str,
        blob_oid: &str,
    ) {
        let session_row = ids::session_row_id(AGENT, NATIVE);
        let snapshot_id = ids::turn_boundary_snapshot_id(&session_row, ordinal, b.as_str());
        record_turn_boundary_snapshot(
            s,
            &TurnBoundarySnapshotRow {
                snapshot_id: snapshot_id.clone(),
                session_id: session_row.clone(),
                turn_ordinal: i64::from(ordinal),
                boundary: b.as_str().to_owned(),
                actor: b.actor().to_owned(),
                worktree_hash: "wh".into(),
                captured_at: 1,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        record_turn_boundary_file(
            s,
            &TurnBoundaryFileRow {
                snapshot_id,
                path: path.to_owned(),
                blob_oid: blob_oid.to_owned(),
                content_sha256: "sha".into(),
                schema_version: 1,
            },
        )
        .await
        .unwrap();
    }

    /// One recorded hunk for `a.txt` (0-based, half-open coordinates).
    async fn hunk_row(s: &SqliteStore, ordinal: u32, kind: &str, coords: (u32, u32, u32, u32)) {
        let session_row = ids::session_row_id(AGENT, NATIVE);
        let (os, ol, ns, nl) = coords;
        record_turn_diff_hunk(
            s,
            &TurnDiffHunkRow {
                hunk_id: ids::turn_diff_hunk_id(
                    &session_row,
                    ordinal,
                    kind,
                    "a.txt",
                    os,
                    ol,
                    ns,
                    nl,
                ),
                session_id: session_row,
                turn_ordinal: i64::from(ordinal),
                edit_kind: kind.to_owned(),
                path: "a.txt".into(),
                old_start: i64::from(os),
                old_len: i64::from(ol),
                new_start: i64::from(ns),
                new_len: i64::from(nl),
                schema_version: 1,
            },
        )
        .await
        .unwrap();
    }

    /// The mid-session shape: `c0` holds `a.txt` = "base"; turn 1's agent
    /// appends two lines (committed as `c1` MID-SESSION); turn 2's human
    /// appends one and its agent one more; `c2` commits those two over `c1`.
    fn mid_session_reader() -> MemoryBlobReader {
        MemoryBlobReader::new()
            .with_blob("A0", b"base\n")
            .with_blob("A1", b"base\na1\na2\n")
            .with_blob("A3", b"base\na1\na2\nh1\na3\n")
            .with_tree("c0", vec![entry("a.txt", "A0")])
            .with_tree("c1", vec![entry("a.txt", "A1")])
            .with_tree("c2", vec![entry("a.txt", "A3")])
            .with_parent("c1", "c0")
            .with_parent("c2", "c1")
    }

    /// The session's full hunk stack for the shape above, with the boundary
    /// that closed turn 1 observing `first_end_blob` (the parent of `c2` when
    /// the record is honest).
    async fn seed_mid_session(s: &SqliteStore, first_end_blob: &str) {
        hunk_row(s, 1, "agent", (1, 0, 1, 2)).await;
        boundary_with_file(s, 1, Boundary::End, "a.txt", first_end_blob).await;
        hunk_row(s, 2, "human", (3, 0, 3, 1)).await;
        boundary_with_file(s, 2, Boundary::Start, "a.txt", "A2").await;
        hunk_row(s, 2, "agent", (4, 0, 4, 1)).await;
        boundary_with_file(s, 2, Boundary::End, "a.txt", "A3").await;
    }

    /// **A file committed mid-session replays only the hunks after its
    /// parent.** Before: the stack `[A_1(+2), H_2(+1), A_2(+1)]`
    /// replayed over a base derived from `c2`'s own diff (5 − 2 = 3 lines),
    /// grew the model to 7 lines against a 5-line commit, and
    /// `committed_writers` degraded every line to `unknown` — the
    /// all-unknown `blame` the earlier fixture had to shape itself around. Now
    /// the boundary that observed exactly `c2`'s parent blob (`A1`, at turn
    /// 1's end) is the watermark: `A_1` is dropped as consumed, the replay
    /// starts from the real parent's 3 lines, and the two committed lines
    /// resolve to the human and the agent that wrote them.
    ///
    /// Calibrated by making `consumed_through` return `None`: both lines
    /// fall back to `unknown` (0/0) — the quoted red in the commit message.
    #[tokio::test]
    async fn a_file_committed_mid_session_replays_only_the_hunks_after_its_parent() {
        let s = store();
        let r = mid_session_reader();
        seed_mid_session(&s, "A1").await;
        let mut capture = capture_commit(&r, "c2").unwrap();
        assert_eq!(file(&capture, "a.txt").base_blob_oid.as_deref(), Some("A1"));
        enrich_attribution(&s, &r, AGENT, NATIVE, &mut capture)
            .await
            .unwrap();
        let f = file(&capture, "a.txt");
        assert_eq!(f.attribution.human_lines, 1, "{:?}", f.attribution);
        assert_eq!(f.attribution.agent_lines, 1, "{:?}", f.attribution);
        assert_eq!(f.attribution.unknown_lines, 0, "{:?}", f.attribution);
        assert_eq!(f.method_version, METHOD_VERSION);
    }

    /// **No boundary carries the parent → the stack is not rebased** (the
    /// honest fallback): the session never observed `c2`'s parent blob, so
    /// there is nothing to rebase on, and the over-long replay degrades to
    /// `unknown` exactly as before — never a guessed author.
    #[tokio::test]
    async fn no_boundary_carries_the_parent_so_the_stack_is_not_rebased() {
        let s = store();
        let r = mid_session_reader();
        seed_mid_session(&s, "A1-never-observed").await;
        let mut capture = capture_commit(&r, "c2").unwrap();
        enrich_attribution(&s, &r, AGENT, NATIVE, &mut capture)
            .await
            .unwrap();
        let f = file(&capture, "a.txt");
        assert_eq!(f.attribution.human_lines, 0, "{:?}", f.attribution);
        assert_eq!(f.attribution.agent_lines, 0, "{:?}", f.attribution);
        assert_eq!(f.attribution.unknown_lines, 2, "{:?}", f.attribution);
    }

    #[test]
    fn consumed_through_truth_table() {
        let row = |t: i64, b: &str, oid: &str| (t, b.to_owned(), oid.to_owned());
        assert_eq!(consumed_through(&[], "X"), None);
        assert_eq!(consumed_through(&[row(1, "end", "Y")], "X"), None);
        assert_eq!(consumed_through(&[row(1, "start", "X")], "X"), Some((1, 0)));
        assert_eq!(consumed_through(&[row(1, "end", "X")], "X"), Some((1, 1)));
        // Several boundaries observed the parent (the file sat unchanged
        // across windows): the LATEST is the watermark.
        assert_eq!(
            consumed_through(
                &[row(1, "end", "X"), row(2, "start", "X"), row(2, "end", "Z")],
                "X"
            ),
            Some((2, 0))
        );
        // stack_after keeps strictly-later keys, in order.
        let e = |k: StackKey| {
            (
                k,
                TurnEdit {
                    actor: Actor::Agent,
                    hunk: rift_shadow_attribution::Hunk {
                        old_start: 0,
                        old_len: 0,
                        new_start: 0,
                        new_len: 1,
                    },
                },
            )
        };
        let kept: Vec<StackKey> =
            stack_after(vec![e((1, 0)), e((1, 1)), e((2, 0)), e((2, 1))], (1, 1))
                .into_iter()
                .map(|(k, _)| k)
                .collect();
        assert_eq!(kept, vec![(2, 0), (2, 1)]);
    }

    /// A file the replay refused: zero counts AND the commit-boundary stamp, so
    /// no `turn-replay@1` row can claim it was reasoned about.
    fn assert_refused(c: &CommitCapture, path: &str) {
        let f = file(c, path);
        assert_eq!(f.attribution.agent_lines, 0, "{path}: no invented author");
        assert_eq!(f.attribution.human_lines, 0, "{path}: no invented author");
        assert_eq!(
            f.attribution.human_modified_agent_lines, 0,
            "{path}: no invented author"
        );
        assert_eq!(f.method_version, COMMIT_STUB, "{path}: keeps the stub");
    }

    // --- the healthy derivations ------------------------------------------

    /// An open agent window (`start` last, exactly one unmatched start) credits
    /// the agent. Reverting the derivation to a hardcoded `Actor::Human` reddens
    /// this; so does restoring the old `stacks.remove(..) else { continue }`.
    #[tokio::test]
    async fn commit_inside_open_agent_window_credits_the_agent() {
        let s = store();
        seed(&s, &[(0, Boundary::Start)], ("a.txt", "A0")).await;
        let c = enrich(&s, &reader()).await;
        let a = file(&c, "a.txt");
        assert_eq!(
            a.attribution.agent_lines, 1,
            "the added line is the agent's"
        );
        assert_eq!(a.attribution.human_lines, 0);
        assert_eq!(a.attribution.unknown_lines, 0);
        assert_eq!(a.method_version, METHOD_VERSION);
        assert!(a.confidence > 0.0);
    }

    /// The derivation is two-sided: a bracketed turn (`end` last, counts level)
    /// leaves the pending edits with the human.
    #[tokio::test]
    async fn commit_after_a_bracketed_turn_credits_the_human() {
        let s = store();
        seed(
            &s,
            &[(0, Boundary::Start), (0, Boundary::End)],
            ("a.txt", "A0"),
        )
        .await;
        let c = enrich(&s, &reader()).await;
        let a = file(&c, "a.txt");
        assert_eq!(a.attribution.human_lines, 1);
        assert_eq!(a.attribution.agent_lines, 0);
    }

    // --- ATTRv's plants: each must be REFUSED, not merely re-signed ---------

    /// PLANT A — a dropped `end`. Counts say an agent window is open
    /// (`starts > ends`) while the last boundary is an `end`, so the window is
    /// closed. The two sources contradict each other; the record is provably
    /// incomplete and the file is refused rather than credited to either actor.
    /// At the rejected tip this wrote `agent=1 human=0` as `turn-replay@1`.
    #[tokio::test]
    async fn plant_a_dropped_end_is_refused() {
        let s = store();
        // turn 0 start (its `end` never fired), turn 1 start+end.
        seed(
            &s,
            &[
                (0, Boundary::Start),
                (1, Boundary::Start),
                (1, Boundary::End),
            ],
            ("a.txt", "A0"),
        )
        .await;
        let c = enrich(&s, &reader()).await;
        assert_refused(&c, "a.txt");
    }

    /// PLANT B — a dropped `start`. Counts say the human's window is open while
    /// the last boundary is a `start`, i.e. an agent turn is in flight. This is
    /// the very defect ATTR-1 set out to remove, in a degenerate shape; it must
    /// be refused, not silently re-credited. At the rejected tip: `human=1`.
    #[tokio::test]
    async fn plant_b_dropped_start_is_refused() {
        let s = store();
        // turn 0's `start` never fired; turn 1 opened and is still open.
        seed(
            &s,
            &[(0, Boundary::End), (1, Boundary::Start)],
            ("a.txt", "A0"),
        )
        .await;
        let c = enrich(&s, &reader()).await;
        assert_refused(&c, "a.txt");
    }

    /// PLANT C — the anchor blob was garbage-collected. Turn-boundary blobs are
    /// unreachable objects, so git may collect them at any time; `read_blob`
    /// then returns `None`. Defaulting that to empty bytes made an ADDED file
    /// look created inside the open window, and because an added file's replay
    /// base is 0 the length check could not fire. At the rejected tip this wrote
    /// `agent=2 human=0` for a file the session never saw the content of.
    #[tokio::test]
    async fn plant_c_absent_anchor_blob_on_added_file_is_refused() {
        let s = store();
        seed(&s, &[(0, Boundary::Start)], ("added.txt", "COLLECTED")).await;
        let r = reader(); // "COLLECTED" is deliberately absent from the reader
        let c = enrich(&s, &r).await;
        assert_refused(&c, "added.txt");
    }

    /// CONTROL D — the same collected anchor on a file that also exists in the
    /// parent. The length check happened to catch this shape already; it must
    /// stay refused now that the absence is caught upstream instead.
    #[tokio::test]
    async fn control_d_absent_anchor_blob_on_modified_file_is_refused() {
        let s = store();
        seed(&s, &[(0, Boundary::Start)], ("a.txt", "COLLECTED")).await;
        let c = enrich(&s, &reader()).await;
        assert_refused(&c, "a.txt");
    }

    // --- the ★ honesty guarantees -----------------------------------------

    /// Seed one `turn_diff_hunk` row directly — the shape a phantom takes once
    /// the write side has already produced it.
    async fn seed_hunk(
        s: &SqliteStore,
        turn: u32,
        kind: &str,
        path: &str,
        h: (i64, i64, i64, i64),
    ) {
        let session_row = ids::session_row_id(AGENT, NATIVE);
        record_turn_diff_hunk(
            s,
            &TurnDiffHunkRow {
                hunk_id: ids::turn_diff_hunk_id(
                    &session_row,
                    turn,
                    kind,
                    path,
                    u32::try_from(h.0).unwrap(),
                    u32::try_from(h.1).unwrap(),
                    u32::try_from(h.2).unwrap(),
                    u32::try_from(h.3).unwrap(),
                ),
                session_id: session_row,
                turn_ordinal: i64::from(turn),
                edit_kind: kind.to_owned(),
                path: path.to_owned(),
                old_start: h.0,
                old_len: h.1,
                new_start: h.2,
                new_len: h.3,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
    }

    /// ★ WHY THE PHANTOM HAD TO BE STOPPED AT THE WRITE SIDE, pinned as a fact
    /// rather than argued.
    ///
    /// **This test passes before and after the `capture_boundary` fix** — it is
    /// not a regression guard for it, and does not pretend to be. What it pins
    /// is the blast radius: once a whole-file hunk EXISTS as a file's only
    /// recorded edit, the replay has no way to tell it from a real one and
    /// answers `human = 2` under `turn-replay@1` on a file the session never
    /// observed. The downstream length check cannot save it either, because an
    /// added file's replay base is 0, so the fabricated authorship has the
    /// right length by construction.
    ///
    /// A false author in an append-only table is permanent. That is why the
    /// refusal belongs where the row is written, not where it is read.
    #[tokio::test]
    async fn a_phantom_whole_file_hunk_becomes_a_false_blame_answer() {
        let s = store();
        seed(&s, &[(0, Boundary::Start)], ("a.txt", "A0")).await;
        seed_hunk(&s, 0, "human", "added.txt", (0, 0, 0, 2)).await;

        let c = enrich(&s, &reader()).await;
        let f = file(&c, "added.txt");
        assert_eq!(
            f.attribution.human_lines, 2,
            "a phantom hunk is believed in full"
        );
        assert_eq!(f.attribution.unknown_lines, 0);
        assert_eq!(
            f.method_version, METHOD_VERSION,
            "and it is stamped as a replayed answer, indistinguishable from a real one"
        );
    }

    /// A file no boundary observed keeps zero counts and the commit stub, in the
    /// same run where a sibling resolves — so the guard is discriminating.
    #[tokio::test]
    async fn unanchored_file_stays_unknown_even_when_a_sibling_resolves() {
        let s = store();
        seed(&s, &[(0, Boundary::Start)], ("a.txt", "A0")).await;
        let c = enrich(&s, &reader()).await;
        assert_refused(&c, "other.txt");
        assert_eq!(file(&c, "other.txt").attribution.unknown_lines, 1);
        assert_eq!(file(&c, "a.txt").attribution.agent_lines, 1);
    }

    #[tokio::test]
    async fn session_with_no_observations_enriches_nothing() {
        let s = store();
        let c = enrich(&s, &reader()).await;
        for f in &c.files {
            assert_refused(&c, &f.path);
        }
    }

    /// The derivation refuses every contradiction and decides only the two
    /// shapes a complete record can produce.
    #[test]
    fn open_window_actor_truth_table() {
        // Complete records: the ordered token and the counts agree.
        assert_eq!(open_window_actor("start", 1, 0), Some(Actor::Agent));
        assert_eq!(open_window_actor("start", 3, 2), Some(Actor::Agent));
        assert_eq!(open_window_actor("end", 0, 0), Some(Actor::Human));
        assert_eq!(open_window_actor("end", 4, 4), Some(Actor::Human));
        // Contradictions — a dropped hook on either side.
        assert_eq!(open_window_actor("start", 1, 1), None, "plant B shape");
        assert_eq!(open_window_actor("start", 2, 0), None);
        assert_eq!(open_window_actor("end", 2, 1), None, "plant A shape");
        assert_eq!(open_window_actor("end", 0, 1), None, "owner's ordinal 140");
        // An unknown token is never guessed at.
        assert_eq!(open_window_actor("commit", 1, 0), None);
    }
}
