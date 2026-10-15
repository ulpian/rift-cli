//! Deterministic replacement fixtures at each observation phase.

use super::*;

#[test]
fn a_regular_file_is_bounded_and_a_directory_is_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("regular"), b"ordinary\n").unwrap();
    let ordinary = observe(tmp.path(), "regular");
    assert_eq!(ordinary.kind, BoundaryFileKind::Regular);
    assert_eq!(ordinary.bytes, b"ordinary\n");
    std::fs::create_dir(tmp.path().join("directory")).unwrap();
    assert_eq!(
        observe(tmp.path(), "directory").kind,
        BoundaryFileKind::Unknown
    );
    let file = std::fs::File::create(tmp.path().join("oversize")).unwrap();
    file.set_len(MAX_DIFF_BYTES as u64 + 1).unwrap();
    let large = observe(tmp.path(), "oversize");
    assert_eq!(large.kind, BoundaryFileKind::Unknown);
    assert!(large.bytes.is_empty());
}

#[test]
fn missing_paths_are_absent_and_path_escapes_are_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(
        observe(tmp.path(), "missing/child").kind,
        BoundaryFileKind::Absent
    );
    for path in ["../outside", "/absolute", "", ".git/config"] {
        assert_eq!(
            observe(tmp.path(), path).kind,
            BoundaryFileKind::Unknown,
            "{path}"
        );
    }
}

#[cfg(unix)]
#[test]
fn replacing_a_leaf_with_a_link_never_reads_the_target() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret");
    std::fs::write(&secret, b"outside secret bytes\n").unwrap();
    for phase in [Phase::BeforeOpen, Phase::BeforeRead, Phase::AfterRead] {
        let leaf = tmp.path().join("leaf");
        if std::fs::symlink_metadata(&leaf).is_ok() {
            std::fs::remove_file(&leaf).unwrap();
        }
        std::fs::write(&leaf, b"inside\n").unwrap();
        let result = observe_with(tmp.path(), "leaf", |now| {
            if now == phase {
                std::fs::remove_file(&leaf).unwrap();
                symlink(&secret, &leaf).unwrap();
            }
        });
        assert_eq!(result.kind, BoundaryFileKind::Unknown, "{phase:?}");
        assert!(result.bytes.is_empty(), "{phase:?}");
    }
}

#[cfg(unix)]
#[test]
fn replacing_a_parent_with_a_link_cannot_redirect_the_open_handle() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("leaf"), b"external target\n").unwrap();
    for (index, phase) in [Phase::BeforeOpen, Phase::BeforeRead, Phase::AfterRead]
        .into_iter()
        .enumerate()
    {
        let parent = tmp.path().join(format!("p{index}"));
        std::fs::create_dir(&parent).unwrap();
        std::fs::write(parent.join("leaf"), b"inside\n").unwrap();
        let result = observe_with(tmp.path(), &format!("p{index}/leaf"), |now| {
            if now == phase {
                std::fs::rename(&parent, tmp.path().join(format!("moved{index}"))).unwrap();
                symlink(outside.path(), &parent).unwrap();
            }
        });
        assert_eq!(result.kind, BoundaryFileKind::Unknown, "{phase:?}");
        assert!(result.bytes.is_empty());
    }
}

#[cfg(unix)]
#[test]
fn a_socket_and_a_fifo_are_refused_before_they_can_block() {
    let tmp = tempfile::tempdir().unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(tmp.path().join("socket")).unwrap();
    assert!(std::process::Command::new("mkfifo")
        .arg(tmp.path().join("fifo"))
        .status()
        .unwrap()
        .success());
    for path in ["socket", "fifo"] {
        let result = observe(tmp.path(), path);
        assert_eq!(result.kind, BoundaryFileKind::Unknown);
        assert!(result.bytes.is_empty());
    }
}

#[test]
fn replacing_a_regular_file_with_another_regular_file_is_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    for phase in [Phase::BeforeOpen, Phase::BeforeRead, Phase::AfterRead] {
        std::fs::write(tmp.path().join("leaf"), b"inside\n").unwrap();
        std::fs::write(tmp.path().join("replacement"), b"new bytes\n").unwrap();
        let result = observe_with(tmp.path(), "leaf", |now| {
            if now == phase {
                std::fs::remove_file(tmp.path().join("leaf")).unwrap();
                std::fs::rename(tmp.path().join("replacement"), tmp.path().join("leaf")).unwrap();
            }
        });
        assert_eq!(result.kind, BoundaryFileKind::Unknown, "{phase:?}");
        assert!(result.bytes.is_empty());
    }
}

#[cfg(unix)]
#[test]
fn a_root_replaced_with_a_link_before_open_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("leaf"), b"inside\n").unwrap();
    std::fs::write(outside.path().join("leaf"), b"outside\n").unwrap();
    let read = observe_with(&root, "leaf", |phase| {
        if phase == Phase::BeforeRoot {
            std::fs::rename(&root, tmp.path().join("old-root")).unwrap();
            std::os::unix::fs::symlink(outside.path(), &root).unwrap();
        }
    });
    assert_eq!(read.kind, BoundaryFileKind::Unknown);
    assert!(read.bytes.is_empty());
}

#[test]
fn a_missing_path_created_before_absence_is_confirmed_is_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    for path in ["leaf", "new/leaf"] {
        let read = observe_with(tmp.path(), path, |phase| {
            if phase == Phase::BeforeMissing {
                let full = tmp.path().join(path);
                std::fs::create_dir_all(full.parent().unwrap()).unwrap();
                std::fs::write(full, b"arrived during observation\n").unwrap();
            }
        });
        assert_eq!(read.kind, BoundaryFileKind::Unknown);
        assert!(read.bytes.is_empty());
    }
}
