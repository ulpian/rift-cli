//! Bounded current-byte collection associated with a journaled boundary event.
//! Event time is separate from the later non-atomic collection-attempt audit.

use std::collections::BTreeMap;

use rift_core::Result;
use rift_git::{BlobReader, RepoPaths, TreeEntryKind};
use rift_store::{
    record_turn_boundary_collection, BoundaryCollectionInterval, BoundaryFileKind, CanonicalStore,
    TurnBoundaryFileRow, TurnBoundarySnapshotRow,
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
    /// The journaled event's instant, independent of collection time.
    captured_at_ms: i64,
    /// The previous boundary's state, `path → blob_oid`.
    prev: &'a BTreeMap<String, String>,
    /// HEAD's tree, `path → blob_oid` (empty without a HEAD commit).
    head_tree: &'a BTreeMap<String, String>,
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

/// Collect bounded current bytes for a journaled turn-boundary event.
/// The hook's HEAD and captured_at identify the event, not when these bytes
/// were read. A separate collection-attempt interval records the later work.
/// No actor-labelled closed-window hunks are inferred from these observations.
/// Existing snapshot keys are never filled or given new provenance on retry.
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
    capture_boundary_with_clock(
        store,
        repo,
        reader,
        head_commit,
        agent,
        native_session,
        turn_ordinal,
        boundary,
        captured_at_ms,
        boundary_collection_time,
    )
    .await
}

/// Capture bounded current bytes with an injected collection-attempt clock.
/// The event timestamp and this non-atomic collection interval are distinct.
/// Neither establishes event-time bytes or authorship.
///
/// # Errors
/// A storage, clock, git or interval-validation failure.
#[allow(clippy::too_many_arguments)]
pub async fn capture_boundary_with_clock(
    store: &impl CanonicalStore,
    repo: &RepoPaths,
    reader: &impl BlobReader,
    head_commit: Option<&str>,
    agent: &str,
    native_session: &str,
    turn_ordinal: u32,
    boundary: Boundary,
    captured_at_ms: i64,
    mut clock: impl FnMut() -> Result<i64>,
) -> Result<()> {
    let started_at = clock()?;
    let session_row = ids::session_row_id(agent, native_session);
    let prev = super::last_boundary(store, &session_row)
        .await?
        .map(|(_, files)| files)
        .unwrap_or_default();
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
        head_kinds: head_kinds.as_ref(),
    };

    let mut observed: Vec<ObservedFile> = Vec::new();
    let mut fresh: Vec<FreshFile> = Vec::new();
    for path in &paths {
        match observe_boundary_path(&ctx, path)? {
            Observation::Kept(file) => observed.push(file),
            Observation::Fresh(file) => fresh.push(file),
            Observation::Dropped => (),
        }
    }
    observed.extend(address_fresh_files(&ctx, fresh)?);
    observed.sort_by(|a, b| a.path.cmp(&b.path));
    let interval = BoundaryCollectionInterval {
        started_at,
        finished_at: clock()?,
    };
    persist_boundary(&ctx, observed, interval).await
}

/// Read the checked system clock for one boundary collection attempt.
///
/// # Errors
/// The clock predates the Unix epoch or cannot fit epoch milliseconds.
pub fn boundary_collection_time() -> Result<i64> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| rift_core::RiftError::storage("boundary collection clock predates epoch"))?;
    i64::try_from(elapsed.as_millis())
        .map_err(|_| rift_core::RiftError::storage("boundary collection clock overflows"))
}

/// Collect current paths differing from HEAD, previously observed paths, and
/// paths changed by commits since the event's HEAD. Sort and deduplicate them
/// before observing their current bytes.
///
/// Rift's own `.rift/state/` paths are excluded from both status and inherited
/// paths so collection never copies those files into the user's object store.
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

/// Find paths differing between the event's HEAD and the current HEAD.
///
/// Include these paths even when current status calls them clean, then apply
/// the normal current-observation selection. Return no extra paths when HEAD
/// has not moved or its current ref cannot be read.
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

/// Collect this path now, retaining the current kind even for equal bytes.
/// Mtime is not event-time evidence. No closed-window writer is inferred.
fn observe_boundary_path<S: CanonicalStore, R: BlobReader>(
    ctx: &BoundaryCtx<'_, S, R>,
    path: &str,
) -> Result<Observation> {
    let read = observation::observe(&ctx.repo.work_tree, path);
    let kind = read.kind;
    let cur = read.bytes;
    if let Some(oid) = ctx.prev.get(path) {
        if ctx.reader.read_blob(oid)?.as_deref() == Some(cur.as_slice()) {
            return Ok(unchanged_observation(ctx, path, &cur, kind));
        }
    }
    Ok(Observation::Fresh(FreshFile {
        path: path.to_owned(),
        bytes: cur,
        kind,
    }))
}

/// Reuse a verified equal-byte OID with the current observed kind. Only a
/// current regular file matching explicitly regular HEAD can leave the set.
fn unchanged_observation<S, R>(
    ctx: &BoundaryCtx<'_, S, R>,
    path: &str,
    cur: &[u8],
    kind: BoundaryFileKind,
) -> Observation {
    let Some(prev_oid) = ctx.prev.get(path) else {
        return Observation::Dropped;
    };
    if kind == BoundaryFileKind::Regular
        && ctx
            .head_kinds
            .is_some_and(|kinds| kinds.get(path) == Some(&TreeEntryKind::Regular))
        && ctx.head_tree.get(path) == Some(prev_oid)
    {
        return Observation::Dropped;
    }
    Observation::Kept(ObservedFile {
        path: path.to_owned(),
        blob_oid: prev_oid.clone(),
        content_sha256: sha256_hex_of(cur),
        kind,
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

/// Insert a new observation set and append its collection audit last.
async fn persist_boundary<S: CanonicalStore, R>(
    ctx: &BoundaryCtx<'_, S, R>,
    observed: Vec<ObservedFile>,
    interval: BoundaryCollectionInterval,
) -> Result<()> {
    let snapshot_id =
        ids::turn_boundary_snapshot_id(ctx.session_row, ctx.turn_ordinal, ctx.boundary.as_str());
    let snapshot = TurnBoundarySnapshotRow {
        snapshot_id: snapshot_id.clone(),
        session_id: ctx.session_row.to_owned(),
        turn_ordinal: i64::from(ctx.turn_ordinal),
        boundary: ctx.boundary.as_str().to_owned(),
        actor: ctx.boundary.actor().to_owned(),
        worktree_hash: worktree_hash(&observed),
        captured_at: ctx.captured_at_ms,
        schema_version: 1,
    };
    let files: Vec<_> = observed
        .into_iter()
        .map(|file| {
            (
                TurnBoundaryFileRow {
                    snapshot_id: snapshot_id.clone(),
                    path: file.path,
                    blob_oid: file.blob_oid,
                    content_sha256: file.content_sha256,
                    schema_version: 1,
                },
                file.kind,
            )
        })
        .collect();
    record_turn_boundary_collection(ctx.store, &snapshot, &files, interval).await?;
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

    #[allow(clippy::too_many_arguments)]
    async fn capture_boundary(
        store: &impl rift_store::CanonicalStore,
        repo: &RepoPaths,
        reader: &impl BlobReader,
        head: Option<&str>,
        agent: &str,
        native: &str,
        ordinal: u32,
        boundary: Boundary,
        at: i64,
    ) -> Result<()> {
        let mut tick = at + 60_000;
        super::capture_boundary_with_clock(
            store,
            repo,
            reader,
            head,
            agent,
            native,
            ordinal,
            boundary,
            at,
            || {
                let current = tick;
                tick += 1;
                Ok(current)
            },
        )
        .await
    }

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

    async fn assert_latest_observation(
        s: &SqliteStore,
        r: &Collected,
        path: &str,
        bytes: &[u8],
        boundary: Boundary,
        at: i64,
    ) {
        let snapshot =
            rift_store::latest_turn_boundary_snapshot(s, &ids::session_row_id(AGENT, NATIVE))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(snapshot.boundary, boundary.as_str());
        assert_eq!(snapshot.captured_at, at);
        let files = rift_store::turn_boundary_files_for_snapshot(s, &snapshot.snapshot_id)
            .await
            .unwrap();
        let file = files.iter().find(|f| f.path == path).unwrap();
        assert_eq!(r.read_blob(&file.blob_oid).unwrap().unwrap(), bytes);
        let kinds = rift_store::turn_boundary_file_kinds_for_snapshot(s, &snapshot.snapshot_id)
            .await
            .unwrap();
        assert_eq!(kinds.get(path), Some(&BoundaryFileKind::Regular));
        let audit = rift_store::turn_boundary_observation_for_snapshot(s, &snapshot.snapshot_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            audit.interval,
            BoundaryCollectionInterval {
                started_at: at + 60_000,
                finished_at: at + 60_001
            }
        );
        assert_eq!(audit.worktree_hash, snapshot.worktree_hash);
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

    /// An unreadable previous object cannot supply bytes. Re-address the actual
    /// current buffer and retain its exact OID, without claiming any writer.
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
        // Fixture-known creation still supplies no production writer proof.
        let after_turn_0 = hunks(&s).await;
        assert!(
            after_turn_0.is_empty(),
            "observed creation does not identify its writer"
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
            "the exact current buffer is readable again; no writer is inferred"
        );
    }

    /// Two collections preserve both exact buffers, kinds and audit intervals.
    /// Fixture-known actors do not become production attribution facts.
    #[tokio::test]
    async fn two_boundaries_over_real_edits_record_exact_observations_without_authorship() {
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

        assert_latest_observation(
            &s,
            &reader(tmp.path(), &[]),
            "a.txt",
            b"one\ntwo\nthree\n",
            Boundary::Start,
            AFTER_THE_EDITS,
        )
        .await;

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

        assert!(hunks(&s).await.is_empty());
        assert_latest_observation(
            &s,
            &reader(tmp.path(), &[]),
            "a.txt",
            b"one\ntwo\nthree\nfour\n",
            Boundary::End,
            LATER_STILL,
        )
        .await;
    }

    /// An unavailable HEAD object does not hide the bounded current buffer.
    #[tokio::test]
    async fn a_collected_head_blob_does_not_hide_current_regular_observation() {
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

    /// A new regular file still yields its exact current bytes and audit.
    /// Its absence from HEAD does not establish who created it.
    #[tokio::test]
    async fn a_file_absent_from_head_still_records_its_observed_bytes() {
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

        assert!(hunks(&s).await.is_empty());
        assert_latest_observation(
            &s,
            &reader(tmp.path(), &[]),
            "new.txt",
            b"alpha\nbeta\n",
            Boundary::Start,
            AFTER_THE_EDITS,
        )
        .await;
    }
}
