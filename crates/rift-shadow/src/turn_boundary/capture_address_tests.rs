//! Object addressing must use the captured buffer after a path changes.

use super::*;

#[test]
fn multiple_regular_buffers_are_addressed_without_storing_replacement_disk_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    let mut fresh = Vec::new();
    let mut replacements = Vec::new();
    for index in 0..4 {
        let path = format!("file-{index}");
        std::fs::write(tmp.path().join(&path), format!("replacement {index}\n")).unwrap();
        replacements.push(git(tmp.path(), &["hash-object", &path]));
        fresh.push(FreshFile {
            path,
            bytes: format!("captured {index}\n").into_bytes(),
            kind: BoundaryFileKind::Regular,
        });
    }
    let repo = rift_git::discover(tmp.path()).unwrap();
    let reader = rift_git::GixBlobReader::open(tmp.path()).unwrap();
    let empty = BTreeMap::new();
    let ctx = BoundaryCtx {
        store: &(),
        repo: &repo,
        reader: &reader,
        session_row: "s",
        turn_ordinal: 0,
        boundary: Boundary::Start,
        captured_at_ms: 0,
        prev: &empty,
        head_tree: &empty,
        head_kinds: None,
    };
    let addressed = address_fresh_files(&ctx, fresh).unwrap();
    assert_eq!(addressed.len(), 4);
    for (index, file) in addressed.iter().enumerate() {
        assert_eq!(
            reader.read_blob(&file.blob_oid).unwrap().unwrap(),
            format!("captured {index}\n").as_bytes()
        );
    }
    for oid in replacements {
        assert!(reader.read_blob(&oid).unwrap().is_none());
    }
}

#[cfg(unix)]
#[test]
fn replacing_a_path_with_a_link_before_addressing_never_writes_target_bytes() {
    use rift_git::GixBlobReader;
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    let secret = outside.path().join("secret");
    std::fs::write(&secret, b"private target must never become an object\n").unwrap();
    let secret_oid = git(tmp.path(), &["hash-object", secret.to_str().unwrap()]);
    let path = tmp.path().join("leaf");
    std::fs::write(&path, b"observed\n").unwrap();
    let read = observation::observe(tmp.path(), "leaf");
    std::fs::remove_file(&path).unwrap();
    symlink(&secret, &path).unwrap();
    let repo = rift_git::discover(tmp.path()).unwrap();
    let reader = GixBlobReader::open(tmp.path()).unwrap();
    let empty = BTreeMap::new();
    let ctx = BoundaryCtx {
        store: &(),
        repo: &repo,
        reader: &reader,
        session_row: "s",
        turn_ordinal: 0,
        boundary: Boundary::Start,
        captured_at_ms: 0,
        prev: &empty,
        head_tree: &empty,
        head_kinds: None,
    };
    let files = address_fresh_files(
        &ctx,
        vec![FreshFile {
            path: "leaf".to_owned(),
            bytes: read.bytes,
            kind: read.kind,
        }],
    )
    .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(
        reader.read_blob(&files[0].blob_oid).unwrap().unwrap(),
        b"observed\n"
    );
    assert!(reader.read_blob(&secret_oid).unwrap().is_none());
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}
