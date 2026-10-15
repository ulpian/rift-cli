//! The write side: observe the working set at a turn boundary, diff each file
//! against the previous boundary, and persist the append-only boundary rows.

use std::collections::BTreeMap;

use rift_core::Result;
use rift_git::{BlobReader, RepoPaths, TreeEntryKind};
use rift_shadow_attribution::{diff_attributable, Hunk};
use rift_store::{
    record_turn_boundary_file_with_kind, record_turn_boundary_snapshot, record_turn_diff_hunk,
    turn_boundary_file_kinds_for_snapshot, BoundaryFileKind, CanonicalStore, TurnBoundaryFileRow,
    TurnBoundarySnapshotRow, TurnDiffHunkRow,
};

use super::{observation, Boundary};
use crate::ids;

/// Everything one boundary capture reads while it walks the working set.
/// Built once per call so each phase names only the path it works on.
struct BoundaryCtx<'a, S, R> {
    store: &'a S,
    repo: &'a RepoPaths,
    reader: &'a R,
    session_row: &'a str,
    turn_ordinal: u32,
    boundary: Boundary,
    /// The boundary's own instant: the snapshot row's `captured_at`, and the
    /// line a working file's modification time is held against.
    captured_at_ms: i64,
    /// The previous boundary's state, `path → blob_oid`.
    prev: &'a BTreeMap<String, String>,
    /// HEAD's tree, `path → blob_oid` (empty without a HEAD commit).
    head_tree: &'a BTreeMap<String, String>,
    prev_kinds: &'a BTreeMap<String, BoundaryFileKind>,
    head_kinds: Option<&'a BTreeMap<String, TreeEntryKind>>,
}

/// One file as this boundary observed it: the `turn_boundary_file` row to be.
struct ObservedFile {
    path: String,
    blob_oid: String,
    content_sha256: String,
    kind: BoundaryFileKind,
}

/// A file whose current bytes still need an address in the object store.
/// Only this buffer is written; no worktree path is reopened.
struct FreshFile {
    path: String,
    bytes: Vec<u8>,
    kind: BoundaryFileKind,
}

/// What the walk makes of one path.
enum Observation {
    /// Unchanged in this window; kept under the previous boundary's oid.
    Kept(ObservedFile),
    /// Its current bytes join the state once they have an address.
    Fresh(FreshFile),
    /// Not part of this boundary's state.
    Dropped,
}

/// Capture one turn boundary: snapshot the working set and persist the closed
/// window's H_i/A_i hunks. Idempotent per `(session, ordinal, boundary)`.
///
/// Three phases, in this order: observe the working set, diff each file
/// against the previous boundary (its hunk rows land as the walk reaches it),
/// then persist the snapshot row and its file rows.
///
/// `head_commit` and `captured_at_ms` are the boundary's own: the commit
/// `HEAD` named and the instant the hook that closed the boundary ran. The
/// detached worker takes the snapshot later, so the observation is held to
/// that instant rather than to the moment it runs — a path written after it
/// is unknown to this boundary (see `observe_boundary_path`), and a commit
/// made since does not hide the paths it took out of the working set.
///
/// # Errors
/// [`rift_core::RiftError`] on a git or storage failure (callers at the hook
/// boundary swallow it — capture never blocks the agent).
#[allow(clippy::too_many_arguments)] // the public shape every hook caller already passes
pub async fn capture_boundary(
    store: &impl CanonicalStore,
    repo: &RepoPaths,
    reader: &impl BlobReader,
    head_commit: Option<&str>,
    agent: &str,
    native_session: &str,
    turn_ordinal: u32,
    boundary: Boundary,
    captured_at_ms: i64,
) -> Result<()> {
    let session_row = ids::session_row_id(agent, native_session);
    let (prev, prev_kinds) = previous_observation(store, &session_row).await?;
    let committed = paths_committed_since(reader, repo, head_commit)?;
    let paths = working_set(repo, &prev, committed)?;
    let head_tree = head_tree_oids(reader, head_commit, &paths)?;
    let head_kinds = match head_commit {
        Some(head) if !paths.is_empty() => reader.tree_entry_kinds_for(head, &paths)?,
        _ => Some(BTreeMap::new()),
    };
    let ctx = BoundaryCtx {
        store,
        repo,
        reader,
        session_row: &session_row,
        turn_ordinal,
        boundary,
        captured_at_ms,
        prev: &prev,
        head_tree: &head_tree,
        prev_kinds: &prev_kinds,
        head_kinds: head_kinds.as_ref(),
    };

    let mut observed: Vec<ObservedFile> = Vec::new();
    let mut fresh: Vec<FreshFile> = Vec::new();
    for path in &paths {
        match observe_boundary_path(&ctx, path).await? {
            Observation::Kept(file) => observed.push(file),
            Observation::Fresh(file) => fresh.push(file),
            Observation::Dropped => (),
        }
    }
    observed.extend(address_fresh_files(&ctx, fresh)?);
    observed.sort_by(|a, b| a.path.cmp(&b.path));
    persist_boundary(&ctx, observed).await
}

async fn previous_observation(
    store: &impl CanonicalStore,
    session: &str,
) -> Result<(BTreeMap<String, String>, BTreeMap<String, BoundaryFileKind>)> {
    let Some((snapshot, files)) = super::last_boundary(store, session).await? else {
        return Ok((BTreeMap::new(), BTreeMap::new()));
    };
    let kinds = turn_boundary_file_kinds_for_snapshot(store, &snapshot.snapshot_id).await?;
    Ok((files, kinds))
}

/// The observation set: everything currently differing from HEAD, plus every
/// path the previous boundary observed (a file reverted since then still
/// needs its closing diff), plus every path a commit since the boundary
/// changed. Sorted and deduplicated.
///
/// Rift's own state under `.rift/state/` is never part of it: it is neither a
/// person's edit nor an agent's, and observing it would copy Rift's files into
/// the user's object store at every boundary. The status call leaves it out,
/// and a path an older boundary anchored there is dropped here.
fn working_set(
    repo: &RepoPaths,
    prev: &BTreeMap<String, String>,
    committed: Vec<String>,
) -> Result<Vec<String>> {
    let mut paths: Vec<String> = rift_git::changed_worktree_paths_outside_rift_state(repo)?;
    let inherited = prev.keys().cloned().chain(committed);
    paths.extend(inherited.filter(|p| !is_rift_state(p)));
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// The paths a commit made since the boundary changed: they differ between
/// the boundary's `HEAD` and the one the work tree has now.
///
/// The status call compares the work tree with the current `HEAD`, so a file
/// the boundary saw edited and a later commit took in reads as clean there.
/// Adding back every path the two commits disagree on keeps the observation
/// the boundary's own; a path that was not part of it reads unchanged against
/// the boundary's tree and drops out by the unchanged-file rule. Nothing is
/// listed while `HEAD` has not moved, which is every boundary the worker
/// reaches before the next commit, nor when the current `HEAD` cannot be read
/// from the ref files, where nothing says it moved.
fn paths_committed_since(
    reader: &impl BlobReader,
    repo: &RepoPaths,
    head_commit: Option<&str>,
) -> Result<Vec<String>> {
    let Some(now) = rift_git::head_state(repo).oid else {
        return Ok(Vec::new());
    };
    if head_commit == Some(now.as_str()) {
        return Ok(Vec::new());
    }
    let then = tree_oids(reader, head_commit)?;
    let mut now = tree_oids(reader, Some(&now))?;
    let mut moved: Vec<String> = Vec::new();
    for (path, oid) in then {
        if now.remove(&path).as_ref() != Some(&oid) {
            moved.push(path);
        }
    }
    moved.extend(now.into_keys());
    Ok(moved)
}

/// A commit's whole tree, `path → blob_oid`; empty without a commit.
fn tree_oids(reader: &impl BlobReader, commit: Option<&str>) -> Result<BTreeMap<String, String>> {
    let Some(commit) = commit else {
        return Ok(BTreeMap::new());
    };
    Ok(reader
        .tree_entries_at(commit)?
        .into_iter()
        .map(|e| (e.path, e.blob_oid))
        .collect())
}

/// Whether a work-tree-relative path lies in Rift's own state directory.
fn is_rift_state(path: &str) -> bool {
    path.starts_with(".rift/state/")
}

/// HEAD's blob oids for the observed paths (a path's first-window baseline).
/// Only those paths are looked up, and none when there are none: listing the
/// whole tree would cost a decode of every directory at every boundary.
fn head_tree_oids(
    reader: &impl BlobReader,
    head_commit: Option<&str>,
    paths: &[String],
) -> Result<BTreeMap<String, String>> {
    let Some(head) = head_commit else {
        return Ok(BTreeMap::new());
    };
    if paths.is_empty() {
        return Ok(BTreeMap::new());
    }
    Ok(reader
        .tree_entries_for(head, paths)?
        .into_iter()
        .map(|e| (e.path, e.blob_oid))
        .collect())
}

/// Observe one path: record the closed window's hunks for it and say what
/// the boundary state keeps for it.
///
/// ★ A PATH WRITTEN AFTER THE BOUNDARY IS UNKNOWN TO IT. The snapshot is
/// taken by the detached worker, after the hook that closed the boundary,
/// and the bytes on disk by then may already carry the next window's edits.
/// Diffing them would credit an edit to the window that closed before it was
/// made: an agent's first edit after a prompt would be booked as the human's.
/// So a file whose modification time is after the boundary's instant (or a
/// missing file whose directory changed after it) is treated as an unreadable
/// anchor is: no hunk row for this window, and its current bytes anchor the
/// next one. What that costs is the path's H_i/A_i split for the window this
/// boundary closes, and the late edit itself, which lands in no window; a
/// commit of the path then replays to `unknown` rather than to a writer — a
/// blank, where the alternative is a false author in an append-only table.
async fn observe_boundary_path<S: CanonicalStore, R: BlobReader>(
    ctx: &BoundaryCtx<'_, S, R>,
    path: &str,
) -> Result<Observation> {
    let read = observation::observe(&ctx.repo.work_tree, path);
    let late = read.modified_ms.is_some_and(|ms| ms > ctx.captured_at_ms);
    let kind = read.kind;
    let cur = read.bytes;
    let fresh = |bytes: Vec<u8>| {
        Observation::Fresh(FreshFile {
            path: path.to_owned(),
            bytes,
            kind,
        })
    };
    if late || !regular_window(ctx, path, kind) {
        return Ok(fresh(cur));
    }
    let Some(prev_bytes) = window_anchor(ctx, path)? else {
        // The file is still OBSERVED — only the diff is unknowable. Its
        // current bytes go to the object store and into the boundary
        // state, so the next window has a readable anchor and the
        // blindness lasts exactly one window instead of orphaning the
        // path forever. (When the content matches HEAD again, the fresh
        // oid IS the HEAD oid — content addressing — so the path drops out
        // of the state by the existing unchanged-file rule.)
        return Ok(fresh(cur));
    };

    if cur == prev_bytes {
        return Ok(unchanged_observation(ctx, path, &cur));
    }

    // The closed window's hunks for this file (binary/oversize → no hunks,
    // the file still joins the boundary state).
    if let Some(hunks) = diff_attributable(&prev_bytes, &cur) {
        record_window_hunks(ctx, path, &hunks).await?;
    }

    // The file's bytes at this boundary go into the local object store; the
    // projection gets only the addresses.
    Ok(fresh(cur))
}

/// Type-less historical anchors remain unknown for one window. A regular
/// anchor recorded now restores coverage for the next one. Links retain
/// their payloads without attributing target spellings as edited source.
fn regular_window<S, R>(
    ctx: &BoundaryCtx<'_, S, R>,
    path: &str,
    current: BoundaryFileKind,
) -> bool {
    if !matches!(
        current,
        BoundaryFileKind::Regular | BoundaryFileKind::Absent
    ) {
        return false;
    }
    if ctx.prev.contains_key(path) {
        return matches!(
            ctx.prev_kinds.get(path),
            Some(BoundaryFileKind::Regular | BoundaryFileKind::Absent)
        );
    }
    ctx.head_kinds
        .is_some_and(|kinds| !matches!(kinds.get(path), Some(TreeEntryKind::Link)))
}

/// The bytes this window's diff starts from: the previous boundary's anchor,
/// else HEAD's blob, else empty for a path HEAD does not have.
///
/// ★ ABSENCE IS NOT EMPTINESS — the WRITE side of the rule the replay in
/// `enrich` already follows.
///
/// `read_blob` answers `None` for a blob that is gone. Turn-boundary
/// anchors are unreachable objects written by `hash-object -w`, so git
/// may collect them at any time, and a HEAD blob can be missing from a
/// partial clone. An earlier `unwrap_or_default()` turned that `None`
/// into empty bytes, which reads as "the file did not exist at the
/// anchor" — so an UNTOUCHED file diffs as a whole-file creation and a
/// phantom hunk lands in append-only `turn_diff_hunk`, crediting every
/// line of it to whoever's window is closing. Downstream cannot catch
/// it: on an added file the replay base is 0, so the length check that
/// guards the replay is satisfied by construction.
///
/// `None` here therefore means the window's diff is UNKNOWABLE, and the
/// honest record of an unknowable diff is no row at all.
///
/// Absent-from-the-tree is a DIFFERENT fact and stays an empty
/// baseline: a path missing from HEAD genuinely did not exist, and its
/// creation is a real edit this window is entitled to record.
fn window_anchor<S, R: BlobReader>(
    ctx: &BoundaryCtx<'_, S, R>,
    path: &str,
) -> Result<Option<Vec<u8>>> {
    match ctx.prev.get(path) {
        Some(oid) => ctx.reader.read_blob(oid),
        None => match ctx.head_tree.get(path) {
            Some(oid) => ctx.reader.read_blob(oid),
            None => Ok(Some(Vec::new())),
        },
    }
}

/// Unchanged within this window; still part of the boundary state if it
/// differs from HEAD (so the next window diffs against it), under the
/// previous boundary's oid.
fn unchanged_observation<S, R>(ctx: &BoundaryCtx<'_, S, R>, path: &str, cur: &[u8]) -> Observation {
    let Some(prev_oid) = ctx.prev.get(path) else {
        return Observation::Dropped;
    };
    if ctx.head_tree.get(path) == Some(prev_oid) {
        return Observation::Dropped;
    }
    Observation::Kept(ObservedFile {
        path: path.to_owned(),
        blob_oid: prev_oid.clone(),
        content_sha256: sha256_hex_of(cur),
        kind: ctx
            .prev_kinds
            .get(path)
            .copied()
            .unwrap_or(BoundaryFileKind::Unknown),
    })
}

/// Write every fresh file's bytes to the local object store and observe them
/// under the oids git returns.
///
/// Only observed bytes reach git. Reopening worktree paths here would race
/// replacements and follow links before any returned-object check could help.
fn address_fresh_files<S, R: BlobReader>(
    ctx: &BoundaryCtx<'_, S, R>,
    fresh: Vec<FreshFile>,
) -> Result<Vec<ObservedFile>> {
    fresh
        .into_iter()
        .map(|file| {
            let blob_oid = rift_git::hash_object_write(ctx.repo, &file.bytes)?;
            Ok(ObservedFile {
                content_sha256: sha256_hex_of(&file.bytes),
                path: file.path,
                blob_oid,
                kind: file.kind,
            })
        })
        .collect()
}

/// Append the closed window's hunks for one file, in diff order.
async fn record_window_hunks<S: CanonicalStore, R>(
    ctx: &BoundaryCtx<'_, S, R>,
    path: &str,
    hunks: &[Hunk],
) -> Result<()> {
    for h in hunks {
        record_turn_diff_hunk(ctx.store, &turn_diff_hunk_row(ctx, path, h)).await?;
    }
    Ok(())
}

/// The `turn_diff_hunk` row for one hunk of the window this boundary closes.
fn turn_diff_hunk_row<S, R>(ctx: &BoundaryCtx<'_, S, R>, path: &str, h: &Hunk) -> TurnDiffHunkRow {
    let edit_kind = ctx.boundary.edit_kind();
    TurnDiffHunkRow {
        hunk_id: ids::turn_diff_hunk_id(
            ctx.session_row,
            ctx.turn_ordinal,
            edit_kind,
            path,
            h.old_start,
            h.old_len,
            h.new_start,
            h.new_len,
        ),
        session_id: ctx.session_row.to_owned(),
        turn_ordinal: i64::from(ctx.turn_ordinal),
        edit_kind: edit_kind.to_owned(),
        path: path.to_owned(),
        old_start: i64::from(h.old_start),
        old_len: i64::from(h.old_len),
        new_start: i64::from(h.new_start),
        new_len: i64::from(h.new_len),
        schema_version: 1,
    }
}

/// Persist the boundary: its snapshot row first, then one file row per
/// observed path, in path order.
async fn persist_boundary<S: CanonicalStore, R>(
    ctx: &BoundaryCtx<'_, S, R>,
    observed: Vec<ObservedFile>,
) -> Result<()> {
    let snapshot_id =
        ids::turn_boundary_snapshot_id(ctx.session_row, ctx.turn_ordinal, ctx.boundary.as_str());
    record_turn_boundary_snapshot(
        ctx.store,
        &TurnBoundarySnapshotRow {
            snapshot_id: snapshot_id.clone(),
            session_id: ctx.session_row.to_owned(),
            turn_ordinal: i64::from(ctx.turn_ordinal),
            boundary: ctx.boundary.as_str().to_owned(),
            actor: ctx.boundary.actor().to_owned(),
            worktree_hash: worktree_hash(&observed),
            captured_at: ctx.captured_at_ms,
            schema_version: 1,
        },
    )
    .await?;
    for file in observed {
        record_turn_boundary_file_with_kind(
            ctx.store,
            &TurnBoundaryFileRow {
                snapshot_id: snapshot_id.clone(),
                path: file.path.clone(),
                blob_oid: file.blob_oid,
                content_sha256: file.content_sha256,
                schema_version: 1,
            },
            file.kind,
        )
        .await?;
    }
    Ok(())
}

/// The whole-boundary content hash: over the sorted (path, sha256) set.
fn worktree_hash(observed: &[ObservedFile]) -> String {
    let mut acc: Vec<u8> = Vec::new();
    for file in observed {
        acc.extend_from_slice(file.path.as_bytes());
        acc.push(0);
        acc.extend_from_slice(file.content_sha256.as_bytes());
        acc.push(b'\n');
    }
    sha256_hex_of(&acc)
}

fn sha256_hex_of(bytes: &[u8]) -> String {
    crate::sha256_hex(&[bytes])
}

#[cfg(test)]
#[path = "capture_address_tests.rs"]
mod addressing_tests;

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
mod tests {
    use std::collections::HashSet;
    use std::path::Path;
    use std::process::Command;

    use super::super::previous_state;
    use super::*;
    use rift_git::{BlobReader, GixBlobReader, RepoPaths, TreeEntry};
    use rift_store::{turn_diff_hunks_for_session, SqliteStore};

    const AGENT: &str = "claude-code";
    const NATIVE: &str = "sess-b";
    /// Boundary instants after every edit these fixtures make, fixed so no
    /// verdict reads the clock (2100-01-01 and a millisecond on).
    const AFTER_THE_EDITS: i64 = 4_102_444_800_000;
    const LATER_STILL: i64 = AFTER_THE_EDITS + 1;

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
        fn tree_entry_kinds_for(
            &self,
            commit: &str,
            paths: &[String],
        ) -> Result<Option<BTreeMap<String, rift_git::TreeEntryKind>>> {
            self.inner.tree_entry_kinds_for(commit, paths)
        }
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
            AFTER_THE_EDITS,
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
            LATER_STILL,
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
            AFTER_THE_EDITS,
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
            LATER_STILL,
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
            AFTER_THE_EDITS,
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
            AFTER_THE_EDITS,
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
