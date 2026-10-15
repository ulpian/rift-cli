//! Link payloads are names, never permission to capture their referents.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::symlink;

use super::*;

#[tokio::test]
async fn a_link_payload_equal_to_previous_head_bytes_keeps_current_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, _) = repo_with_a_commit(tmp.path());
    std::fs::write(tmp.path().join("a.txt"), b"target").unwrap();
    git(tmp.path(), &["commit", "-qam", "regular target spelling"]);
    let head = git(tmp.path(), &["rev-parse", "HEAD"]);
    let oid = git(tmp.path(), &["rev-parse", "HEAD:a.txt"]);
    let s = store();
    let session = ids::session_row_id(AGENT, NATIVE);
    let snapshot_id = ids::turn_boundary_snapshot_id(&session, 0, "start");
    record_turn_boundary_snapshot(
        &s,
        &TurnBoundarySnapshotRow {
            snapshot_id: snapshot_id.clone(),
            session_id: session.clone(),
            turn_ordinal: 0,
            boundary: "start".into(),
            actor: "human".into(),
            worktree_hash: "legacy".into(),
            captured_at: 0,
            schema_version: 1,
        },
    )
    .await
    .unwrap();
    rift_store::record_turn_boundary_file_with_kind(
        &s,
        &TurnBoundaryFileRow {
            snapshot_id,
            path: "a.txt".into(),
            blob_oid: oid,
            content_sha256: crate::sha256_hex(&[b"target"]),
            schema_version: 1,
        },
        rift_store::BoundaryFileKind::Regular,
    )
    .await
    .unwrap();
    std::fs::remove_file(tmp.path().join("a.txt")).unwrap();
    symlink("target", tmp.path().join("a.txt")).unwrap();
    capture(&s, &repo, &reader(tmp.path()), &head, Boundary::End).await;
    let latest = rift_store::latest_turn_boundary_snapshot(&s, &session)
        .await
        .unwrap()
        .unwrap();
    let kinds = rift_store::turn_boundary_file_kinds_for_snapshot(&s, &latest.snapshot_id)
        .await
        .unwrap();
    assert_eq!(
        kinds.get("a.txt"),
        Some(&rift_store::BoundaryFileKind::Link)
    );
    let files = rift_store::turn_boundary_files_for_snapshot(&s, &latest.snapshot_id)
        .await
        .unwrap();
    let file = files.iter().find(|file| file.path == "a.txt").unwrap();
    assert_eq!(
        reader(tmp.path())
            .read_blob(&file.blob_oid)
            .unwrap()
            .unwrap(),
        b"target"
    );
}

#[tokio::test]
async fn link_payloads_are_anchored_without_reading_their_targets() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let (repo, head) = repo_with_a_commit(tmp.path());
    let secret = outside.path().join("private.txt");
    std::fs::write(&secret, b"external target must never enter objects\n").unwrap();
    let secret_oid = git(outside.path(), &["hash-object", secret.to_str().unwrap()]);
    let directory = outside.path().join("directory");
    std::fs::create_dir(&directory).unwrap();
    let absolute = tmp.path().join("a.txt");
    let targets = [
        ("relative", OsStr::new("a.txt")),
        ("absolute", absolute.as_os_str()),
        ("external", secret.as_os_str()),
        ("dangling", OsStr::new("absent")),
        ("directory", directory.as_os_str()),
        ("non-utf8", OsStr::from_bytes(b"missing-\xff")),
    ];
    for (name, target) in targets {
        symlink(target, tmp.path().join(name)).unwrap();
    }
    let s = store();
    capture(&s, &repo, &reader(tmp.path()), &head, Boundary::Start).await;

    assert!(hunk_paths(&s).await.is_empty());
    let state = previous_state(&s, &ids::session_row_id(AGENT, NATIVE))
        .await
        .unwrap();
    let blobs = GixBlobReader::open(tmp.path()).unwrap();
    for (name, target) in targets {
        let oid = state.get(name).unwrap();
        assert_eq!(blobs.read_blob(oid).unwrap().unwrap(), target.as_bytes());
    }
    assert!(!object_exists(tmp.path(), &secret_oid));
}

#[tokio::test]
async fn a_link_to_regular_transition_preserves_current_observations() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, head) = repo_with_a_commit(tmp.path());
    symlink("a.txt", tmp.path().join("new")).unwrap();
    let s = store();
    capture(&s, &repo, &reader(tmp.path()), &head, Boundary::Start).await;
    std::fs::remove_file(tmp.path().join("new")).unwrap();
    std::fs::write(tmp.path().join("new"), b"regular\n").unwrap();
    capture(&s, &repo, &reader(tmp.path()), &head, Boundary::End).await;
    assert!(hunk_paths(&s).await.is_empty());
    std::fs::write(tmp.path().join("new"), b"regular\nedit\n").unwrap();
    capture_boundary(
        &s,
        &repo,
        &reader(tmp.path()),
        Some(&head),
        AGENT,
        NATIVE,
        1,
        Boundary::Start,
        AFTER_THE_EDITS,
    )
    .await
    .unwrap();
    assert!(hunk_paths(&s).await.is_empty());
    let state = previous_state(&s, &ids::session_row_id(AGENT, NATIVE))
        .await
        .unwrap();
    assert_eq!(
        reader(tmp.path())
            .read_blob(state.get("new").unwrap())
            .unwrap()
            .unwrap(),
        b"regular\nedit\n"
    );
}

#[tokio::test]
async fn a_head_link_does_not_hide_a_current_regular_observation() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, _) = repo_with_a_commit(tmp.path());
    symlink("a.txt", tmp.path().join("tracked-link")).unwrap();
    git(tmp.path(), &["add", "tracked-link"]);
    git(tmp.path(), &["commit", "-qm", "link"]);
    let head = git(tmp.path(), &["rev-parse", "HEAD"]);
    std::fs::remove_file(tmp.path().join("tracked-link")).unwrap();
    std::fs::write(tmp.path().join("tracked-link"), b"regular\n").unwrap();
    let s = store();
    capture(&s, &repo, &reader(tmp.path()), &head, Boundary::Start).await;
    assert!(hunk_paths(&s).await.is_empty());
    std::fs::write(tmp.path().join("tracked-link"), b"regular\nedit\n").unwrap();
    capture(&s, &repo, &reader(tmp.path()), &head, Boundary::End).await;
    assert!(hunk_paths(&s).await.is_empty());
    let state = previous_state(&s, &ids::session_row_id(AGENT, NATIVE))
        .await
        .unwrap();
    assert_eq!(
        reader(tmp.path())
            .read_blob(state.get("tracked-link").unwrap())
            .unwrap()
            .unwrap(),
        b"regular\nedit\n"
    );
}

async fn seed_legacy_anchor(s: &SqliteStore, dir: &Path, path: &str) {
    let session = ids::session_row_id(AGENT, NATIVE);
    let snapshot_id = ids::turn_boundary_snapshot_id(&session, 0, "start");
    record_turn_boundary_snapshot(
        s,
        &TurnBoundarySnapshotRow {
            snapshot_id: snapshot_id.clone(),
            session_id: session,
            turn_ordinal: 0,
            boundary: "start".to_owned(),
            actor: "human".to_owned(),
            worktree_hash: "legacy".to_owned(),
            captured_at: 0,
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
            blob_oid: git(dir, &["hash-object", "-w", path]),
            content_sha256: "legacy".to_owned(),
            schema_version: 1,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_legacy_anchor_without_kind_does_not_guess_its_prior_type() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, head) = repo_with_a_commit(tmp.path());
    let s = store();
    seed_legacy_anchor(&s, tmp.path(), "a.txt").await;
    std::fs::write(tmp.path().join("a.txt"), b"different\n").unwrap();
    capture(&s, &repo, &reader(tmp.path()), &head, Boundary::End).await;
    assert!(hunk_paths(&s).await.is_empty());
    std::fs::write(tmp.path().join("a.txt"), b"different\nnext\n").unwrap();
    capture_boundary(
        &s,
        &repo,
        &reader(tmp.path()),
        Some(&head),
        AGENT,
        NATIVE,
        1,
        Boundary::Start,
        AFTER_THE_EDITS,
    )
    .await
    .unwrap();
    assert!(hunk_paths(&s).await.is_empty());
}

#[tokio::test]
async fn an_inherited_path_under_a_parent_link_never_captures_the_referent() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let (repo, head) = repo_with_a_commit(tmp.path());
    std::fs::create_dir(tmp.path().join("cfg")).unwrap();
    std::fs::write(tmp.path().join("cfg/leaf"), b"old\n").unwrap();
    let s = store();
    seed_legacy_anchor(&s, tmp.path(), "cfg/leaf").await;
    std::fs::remove_dir_all(tmp.path().join("cfg")).unwrap();
    std::fs::write(outside.path().join("leaf"), b"private parent-link target\n").unwrap();
    let secret_oid = git(
        tmp.path(),
        &["hash-object", outside.path().join("leaf").to_str().unwrap()],
    );
    symlink(outside.path(), tmp.path().join("cfg")).unwrap();
    capture(&s, &repo, &reader(tmp.path()), &head, Boundary::End).await;
    assert!(hunk_paths(&s).await.is_empty());
    assert!(!object_exists(tmp.path(), &secret_oid));
}
