//! The commit side: replay the session's recorded windows plus the still-open
//! one and replace a commit's all-`unknown` attribution with the split.

use std::collections::BTreeMap;

use rift_core::Result;
use rift_git::BlobReader;
use rift_shadow_attribution::replay::{committed_writers, replay, TurnEdit};
use rift_shadow_attribution::{
    attribute, confidence, diff_attributable, factors_from_signals, Actor, CommitCapture,
    FileChange, HunkAttribution,
};
use rift_store::{turn_boundary_counts, turn_diff_hunks_for_session, CanonicalStore};

use super::stack::{edit_stacks, line_count, rebase_on_parent, StackKey};
use super::{last_boundary, METHOD_VERSION};
use crate::ids;

/// What the session recorded, loaded once per commit: every file's edit
/// stack, the last boundary's anchors, whose window is still open, and how
/// completely the session was bracketed.
struct SessionRecord {
    stacks: BTreeMap<String, Vec<(StackKey, TurnEdit)>>,
    /// The last boundary's content map, `path → blob_oid`.
    boundary_state: BTreeMap<String, String>,
    /// `None` = the boundary record contradicts itself (a dropped hook), so no
    /// final window is synthesized at all for any file in this capture.
    pending_actor: Option<Actor>,
    coverage: f64,
}

/// What one file's replay reads besides the file and its own stack.
struct FileReplayCtx<'a, S, R> {
    store: &'a S,
    reader: &'a R,
    session_row: &'a str,
    boundary_state: &'a BTreeMap<String, String>,
    pending_actor: Option<Actor>,
    coverage: f64,
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
    let Some(record) = load_session_record(store, &session_row).await? else {
        return Ok(());
    };
    let SessionRecord {
        mut stacks,
        boundary_state,
        pending_actor,
        coverage,
    } = record;
    let ctx = FileReplayCtx {
        store,
        reader,
        session_row: &session_row,
        boundary_state: &boundary_state,
        pending_actor,
        coverage,
    };

    for f in &mut capture.files {
        if !f.attributable {
            continue;
        }
        let keyed = stacks.remove(&f.path).unwrap_or_default();
        enrich_file(&ctx, f, keyed).await?;
    }
    Ok(())
}

/// Load the session's record, or `None` when nothing was observed at all for
/// this session — no hunks AND no boundary state. There is no anchor to replay
/// against then, so every file is left honest.
async fn load_session_record(
    store: &impl CanonicalStore,
    session_row: &str,
) -> Result<Option<SessionRecord>> {
    let rows = turn_diff_hunks_for_session(store, session_row).await?;
    // ONE read: the snapshot whose token names the open window's actor IS the
    // snapshot whose files supply the anchor bytes. Not two reads of a rule
    // written twice — see [`last_boundary`].
    let last = last_boundary(store, session_row).await?;
    let boundary_state = last
        .as_ref()
        .map(|(_, files)| files.clone())
        .unwrap_or_default();
    if rows.is_empty() && boundary_state.is_empty() {
        return Ok(None);
    }
    let stacks = edit_stacks(&rows);
    let (starts, ends) = turn_boundary_counts(store, session_row).await?;
    let pending_actor = last
        .as_ref()
        .and_then(|(snapshot, _)| open_window_actor(&snapshot.boundary, starts, ends));
    Ok(Some(SessionRecord {
        stacks,
        boundary_state,
        pending_actor,
        coverage: boundary_coverage(starts, ends),
    }))
}

/// Coverage: a fully-bracketed session has an `end` for every `start`.
fn boundary_coverage(starts: i64, ends: i64) -> f64 {
    if starts == 0 {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let c = ends as f64 / starts as f64;
    c.clamp(0.0, 1.0)
}

/// Replay one committed file and, when the session anchors it, replace its
/// attribution. Every early return leaves the file exactly as the commit
/// capture wrote it.
async fn enrich_file<S: CanonicalStore, R: BlobReader>(
    ctx: &FileReplayCtx<'_, S, R>,
    f: &mut FileChange,
    keyed: Vec<(StackKey, TurnEdit)>,
) -> Result<()> {
    let (mut edits, parent_len) =
        rebase_on_parent(ctx.store, ctx.reader, ctx.session_row, f, keyed).await?;
    // An anchor is only usable if the window it opens has a known owner: a
    // contradictory boundary record (`pending_actor == None`) makes the
    // anchor bytes readable but the authorship unknowable.
    let anchored = ctx.boundary_state.contains_key(&f.path) && ctx.pending_actor.is_some();
    // ★ NEVER INVENT AN ANCHOR. Recorded hunks and a usable boundary anchor
    // are the only two things that tie a committed line to a writer. With
    // neither, the file changed at a time this session never observed — it
    // may predate the session fully — so it stays `unknown` under
    // `commit-diff@1`, untouched by the replay. A plausible guess written
    // into an append-only canonical table is a permanent false fact, and
    // strictly worse than a blank: `blame` would name an author no evidence
    // supports.
    if edits.is_empty() && !anchored {
        return Ok(());
    }
    // An unreadable committed blob is ABSENCE, not emptiness. Defaulting it
    // would make a file with content look like a file with none.
    let Some(committed) = ctx.reader.read_blob(&f.head_blob_oid)? else {
        return Ok(());
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
        (anchored, ctx.boundary_state.get(&f.path), ctx.pending_actor)
    {
        let Some(last_bytes) = ctx.reader.read_blob(oid)? else {
            return Ok(()); // collected anchor → unreadable → not an anchor
        };
        edits.extend(final_window_edits(&last_bytes, &committed, actor));
    }

    let base_len = parent_len.unwrap_or_else(|| parent_len_from_diff(f, committed_len));
    score_file(f, &edits, base_len, committed_len, ctx.coverage);
    Ok(())
}

/// The still-open window's edits, all credited to `actor`: the diff from the
/// last observed boundary bytes to the committed content (none when they are
/// equal, or when either side is not attributable text).
fn final_window_edits(last_bytes: &[u8], committed: &[u8], actor: Actor) -> Vec<TurnEdit> {
    if last_bytes == committed {
        return Vec::new();
    }
    let Some(hunks) = diff_attributable(last_bytes, committed) else {
        return Vec::new();
    };
    hunks
        .into_iter()
        .map(|hunk| TurnEdit { actor, hunk })
        .collect()
}

/// The replay base is the commit's parent-side line count (exact from
/// diff conservation: parent = committed − net insertions).
fn parent_len_from_diff(f: &FileChange, committed_len: u32) -> u32 {
    let net: i64 = f
        .hunks
        .iter()
        .map(|h| i64::from(h.new_len) - i64::from(h.old_len))
        .sum();
    u32::try_from(i64::from(committed_len) - net).unwrap_or(0)
}

/// Replay the file's edits through the LIFO engine and write the result onto
/// the capture: counts, confidence, per-hunk category rows, method stamp.
///
/// What the downstream guard does and does NOT protect. On divergence or
/// a length mismatch `committed_writers` returns empty histories, so a
/// composition whose LINE COUNTS do not add up degrades to `unknown`.
///
/// It constrains counts, not authorship: it compares
/// `histories.len()` against `committed_len` and cannot observe WHO a
/// history names, so it will pass a perfectly-shaped replay that credits
/// the wrong actor. Every actor [`enrich_file`] supplies must therefore be
/// sound BEFORE it reaches here — which is why the open window's owner
/// comes from one ordered source and is abandoned on contradiction, and
/// why an unreadable anchor disqualifies the file. Do not weaken either
/// of those on the strength of this guard.
///
/// The guard is also blind on ADDED files: their replay base is 0, so a
/// fabricated full-file authorship has the right length by construction.
/// `absent_anchor_blob_on_added_file_is_refused` is the test that fails
/// if that stops being handled upstream.
fn score_file(
    f: &mut FileChange,
    edits: &[TurnEdit],
    base_len: u32,
    committed_len: u32,
    coverage: f64,
) {
    let replayed = replay(base_len, edits);
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

#[cfg(test)]
mod tests {
    use super::*;
    use rift_git::{MemoryBlobReader, TreeEntry};
    use rift_shadow_attribution::capture_commit;
    use rift_store::{
        record_turn_boundary_file, record_turn_boundary_snapshot, record_turn_diff_hunk,
        SqliteStore, TurnBoundaryFileRow, TurnBoundarySnapshotRow, TurnDiffHunkRow,
    };

    use crate::turn_boundary::Boundary;

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
