use super::*;
use cap_fs_ext::MetadataExt as _;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

struct Fixture {
    _checkout: tempfile::TempDir,
    _private: tempfile::TempDir,
    owner: QueueLock,
    source: PathBuf,
    private: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let checkout = tempfile::tempdir().unwrap();
        git(checkout.path(), &["init", "--quiet"]);
        std::fs::write(checkout.path().join("a.txt"), b"fixture\n").unwrap();
        git(checkout.path(), &["add", "a.txt"]);
        git(checkout.path(), &["commit", "--quiet", "-m", "fixture"]);
        crate::family::resolve_at(checkout.path()).unwrap();
        let repo = rift_git::discover(checkout.path()).unwrap();
        let job = crate::agent_queue::AgentJob {
            schema_version: 1,
            work_tree: checkout.path().to_owned(),
            queued_at_ms: 1000,
            attempts: 1,
            kind: crate::agent_queue::JobKind::BoundarySnapshot(
                crate::agent_queue::BoundarySnapshot {
                    session_id: "pin-fixture".into(),
                    agent: "claude-code".into(),
                    ordinal: 1,
                    boundary: crate::agent_queue::BoundaryEdge::Start,
                    head_oid: None,
                    captured_at_ms: 1000,
                },
            ),
        };
        let source = crate::agent_queue::enqueue(&repo.git_dir, &job).unwrap();
        let owner = crate::capture_queue::acquire_owner(&repo.git_dir).unwrap();
        crate::family::run_state::prepare_owned(&owner).unwrap();
        let private = tempfile::tempdir().unwrap();
        let parent = private.path().join("parent");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&parent)
            .unwrap();
        let pins = parent.join("pins");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&pins)
            .unwrap();
        Self {
            _checkout: checkout,
            private: pins.canonicalize().unwrap(),
            _private: private,
            owner,
            source: source.canonicalize().unwrap(),
        }
    }

    fn preserve(&self) -> LivePin {
        preserve(
            &self.owner,
            &self.source,
            &self.private,
            OsStr::new("selected.pin"),
        )
        .unwrap()
    }

    fn pin(&self) -> PathBuf {
        self.private.join("selected.pin")
    }
}

fn git(root: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture-id",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(status.status.success());
}

#[test]
fn original_pin_survives_source_unlink_and_reopens_as_missing() {
    let fixture = Fixture::new();
    let pin = fixture.preserve();
    let expected = pin.selection().clone();
    let identity = pin.original_identity();
    assert_eq!(identity.2, 0o600);
    assert_eq!(
        classify(&fixture.owner, &fixture.source, &pin),
        DeliveryMatch::OriginalPresent
    );
    crate::capture_queue::remove_owned(&fixture.source, &fixture.owner).unwrap();
    assert_eq!(
        classify(&fixture.owner, &fixture.source, &pin),
        DeliveryMatch::Missing
    );
    drop(pin);
    let resumed = reopen(
        &fixture.owner,
        &fixture.source,
        &fixture.private,
        OsStr::new("selected.pin"),
        &expected,
    )
    .unwrap();
    assert_eq!(resumed.original_identity(), identity);
    assert_eq!(
        classify(&fixture.owner, &fixture.source, &resumed),
        DeliveryMatch::Missing
    );
    assert_eq!(std::fs::read(fixture.pin()).unwrap(), expected.raw_bytes);
}

#[test]
fn an_identical_new_inode_is_different_after_reopen() {
    let fixture = Fixture::new();
    let pin = fixture.preserve();
    let expected = pin.selection().clone();
    let (_, job) = crate::agent_queue::read_job(&fixture.source).unwrap();
    crate::capture_queue::remove_owned(&fixture.source, &fixture.owner).unwrap();
    let replacement = crate::agent_queue::enqueue(fixture.owner.git_dir(), &job).unwrap();
    assert_eq!(std::fs::read(&replacement).unwrap(), expected.raw_bytes);
    drop(pin);
    let resumed = reopen(
        &fixture.owner,
        &fixture.source,
        &fixture.private,
        OsStr::new("selected.pin"),
        &expected,
    )
    .unwrap();
    assert_eq!(
        classify(&fixture.owner, &fixture.source, &resumed),
        DeliveryMatch::Different
    );
    assert!(replacement.exists());
}

#[test]
fn missing_corrupt_and_copied_pins_cannot_certify_a_missing_source() {
    for kind in ["missing", "corrupt", "copy"] {
        let fixture = Fixture::new();
        let pin = fixture.preserve();
        let expected = pin.selection().clone();
        crate::capture_queue::remove_owned(&fixture.source, &fixture.owner).unwrap();
        match kind {
            "missing" => std::fs::remove_file(fixture.pin()).unwrap(),
            "corrupt" => std::fs::write(fixture.pin(), b"corrupt private bytes").unwrap(),
            _ => {
                let copy = fixture.private.join("copy.pin");
                std::fs::copy(fixture.pin(), &copy).unwrap();
                std::fs::rename(copy, fixture.pin()).unwrap();
            }
        }
        assert_eq!(
            classify(&fixture.owner, &fixture.source, &pin),
            DeliveryMatch::Unavailable
        );
        assert!(reopen(
            &fixture.owner,
            &fixture.source,
            &fixture.private,
            OsStr::new("selected.pin"),
            &expected
        )
        .is_err());
    }
}

#[test]
fn changes_to_pinned_bytes_modes_and_attempts_are_unavailable() {
    for kind in ["bytes", "mode", "attempts"] {
        let fixture = Fixture::new();
        let pin = fixture.preserve();
        match kind {
            "bytes" => std::fs::write(&fixture.source, b"changed").unwrap(),
            "mode" => {
                std::fs::set_permissions(&fixture.source, std::fs::Permissions::from_mode(0o640))
                    .unwrap();
            }
            _ => {
                let mut value = pin.frozen.decoded.full.clone();
                value
                    .as_object_mut()
                    .unwrap()
                    .insert("attempts".into(), serde_json::json!(2));
                std::fs::write(&fixture.source, serde_json::to_vec(&value).unwrap()).unwrap();
            }
        }
        assert_eq!(
            classify(&fixture.owner, &fixture.source, &pin),
            DeliveryMatch::Unavailable
        );
        assert!(fixture.source.exists());
    }
}

#[test]
fn directory_and_symlink_namespace_aliases_refuse_without_clobbering() {
    let fixture = Fixture::new();
    let source_parent = fixture.source.parent().unwrap();
    assert!(preserve(
        &fixture.owner,
        &fixture.source,
        source_parent,
        fixture.source.file_name().unwrap()
    )
    .is_err());
    let alias = fixture.private.join("queue-alias");
    std::os::unix::fs::symlink(source_parent, &alias).unwrap();
    let aliased_source = alias.join(fixture.source.file_name().unwrap());
    assert!(preserve(
        &fixture.owner,
        &aliased_source,
        &fixture.private,
        OsStr::new("alias.pin")
    )
    .is_err());
    let private_alias = fixture.private.join("private-alias");
    std::os::unix::fs::symlink(&fixture.private, &private_alias).unwrap();
    assert!(preserve(
        &fixture.owner,
        &fixture.source,
        &private_alias,
        OsStr::new("alias.pin")
    )
    .is_err());
    for name in ["../escape.pin", "./dot.pin", "/absolute.pin"] {
        assert!(preserve(
            &fixture.owner,
            &fixture.source,
            &fixture.private,
            OsStr::new(name)
        )
        .is_err());
    }
    assert!(fixture.source.exists());
}

#[test]
fn a_replaced_or_unprivate_pin_namespace_is_unavailable() {
    for kind in ["replacement", "permissions"] {
        let fixture = Fixture::new();
        let pin = fixture.preserve();
        crate::capture_queue::remove_owned(&fixture.source, &fixture.owner).unwrap();
        if kind == "replacement" {
            let old = fixture.private.with_extension("old");
            std::fs::rename(&fixture.private, &old).unwrap();
            std::fs::create_dir(&fixture.private).unwrap();
            std::fs::set_permissions(&fixture.private, std::fs::Permissions::from_mode(0o700))
                .unwrap();
            std::fs::hard_link(old.join("selected.pin"), fixture.pin()).unwrap();
        } else {
            std::fs::set_permissions(&fixture.private, std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        assert_eq!(
            classify(&fixture.owner, &fixture.source, &pin),
            DeliveryMatch::Unavailable
        );
        assert!(reopen(
            &fixture.owner,
            &fixture.source,
            &fixture.private,
            OsStr::new("selected.pin"),
            pin.selection()
        )
        .is_err());
    }
}

#[test]
fn existing_pins_and_link_races_leave_unaccepted_evidence_untouched() {
    let fixture = Fixture::new();
    std::fs::write(fixture.pin(), b"foreign pin").unwrap();
    let original = std::fs::read(&fixture.source).unwrap();
    assert!(preserve(
        &fixture.owner,
        &fixture.source,
        &fixture.private,
        OsStr::new("selected.pin")
    )
    .is_err());
    assert_eq!(std::fs::read(fixture.pin()).unwrap(), b"foreign pin");
    assert_eq!(std::fs::read(&fixture.source).unwrap(), original);
    for target in [Phase::BeforeLink, Phase::AfterLink] {
        let fixture = Fixture::new();
        let (_, job) = crate::agent_queue::read_job(&fixture.source).unwrap();
        let old = Namespace::open(fixture.source.parent().unwrap()).unwrap();
        let original = old
            .dir
            .symlink_metadata(fixture.source.file_name().unwrap())
            .unwrap();
        let result = preserve_with(
            &fixture.owner,
            &fixture.source,
            &fixture.private,
            OsStr::new("selected.pin"),
            &mut |phase| {
                if phase == target {
                    crate::capture_queue::remove_owned(&fixture.source, &fixture.owner).unwrap();
                    crate::agent_queue::enqueue(fixture.owner.git_dir(), &job).unwrap();
                }
            },
        );
        assert!(result.is_err());
        assert!(fixture.pin().exists());
        assert_eq!(
            crate::agent_queue::read_job(&fixture.source).unwrap().1,
            job
        );
        let current = std::fs::symlink_metadata(&fixture.source).unwrap();
        assert_ne!(
            std::os::unix::fs::MetadataExt::ino(&current),
            original.ino()
        );
    }
}

#[test]
fn a_pin_lost_during_classification_cannot_authorize_missing() {
    let fixture = Fixture::new();
    let pin = fixture.preserve();
    crate::capture_queue::remove_owned(&fixture.source, &fixture.owner).unwrap();
    let observed = classify_with(&fixture.owner, &fixture.source, &pin, &mut |_| {
        std::fs::remove_file(fixture.pin()).unwrap();
    });
    assert_eq!(observed, DeliveryMatch::Unavailable);
}

#[test]
fn nonregular_sources_refuse_without_following_or_waiting_for_writers() {
    for kind in ["symlink", "directory", "fifo"] {
        let fixture = Fixture::new();
        let target = fixture.private.join("target");
        std::fs::copy(&fixture.source, &target).unwrap();
        crate::capture_queue::remove_owned(&fixture.source, &fixture.owner).unwrap();
        match kind {
            "symlink" => std::os::unix::fs::symlink(&target, &fixture.source).unwrap(),
            "directory" => std::fs::create_dir(&fixture.source).unwrap(),
            _ => assert!(std::process::Command::new("mkfifo")
                .arg(&fixture.source)
                .status()
                .unwrap()
                .success()),
        }
        assert!(preserve(
            &fixture.owner,
            &fixture.source,
            &fixture.private,
            OsStr::new("selected.pin")
        )
        .is_err());
        assert!(!fixture.pin().exists());
        assert!(target.exists());
    }
}

#[test]
fn exactly_sixty_four_kib_is_read_but_cap_plus_one_refuses() {
    for size in [MAX_BYTES, MAX_BYTES + 1] {
        let fixture = Fixture::new();
        let mut bytes = std::fs::read(&fixture.source).unwrap();
        bytes.resize(size, b' ');
        std::fs::write(&fixture.source, &bytes).unwrap();
        let result = preserve(
            &fixture.owner,
            &fixture.source,
            &fixture.private,
            OsStr::new("selected.pin"),
        );
        if size == MAX_BYTES {
            let pin = result.unwrap();
            assert_eq!(pin.frozen.raw_bytes, bytes);
            assert_eq!(
                classify(&fixture.owner, &fixture.source, &pin),
                DeliveryMatch::OriginalPresent
            );
        } else {
            assert!(result.is_err());
            assert!(!fixture.pin().exists());
        }
    }
}

#[test]
fn a_regular_source_replaced_before_open_cannot_follow_links_or_wait_on_a_fifo() {
    for kind in ["symlink", "fifo"] {
        let fixture = Fixture::new();
        let target = fixture.private.join("target");
        std::fs::hard_link(&fixture.source, &target).unwrap();
        let scope = source_scope(&fixture.owner, &fixture.source).unwrap();
        let name = fixture.source.file_name().unwrap();
        let mut opened = false;
        let result = read_regular_with(&scope.namespace, name, name, &mut |phase| {
            if phase == Phase::BeforeOpen {
                crate::capture_queue::remove_owned(&fixture.source, &fixture.owner).unwrap();
                if kind == "symlink" {
                    std::os::unix::fs::symlink(&target, &fixture.source).unwrap();
                } else {
                    assert!(std::process::Command::new("mkfifo")
                        .arg(&fixture.source)
                        .status()
                        .unwrap()
                        .success());
                }
            }
            if phase == Phase::AfterOpen {
                opened = true;
            }
        });
        assert!(result.is_err());
        assert_eq!(opened, kind == "fifo");
        assert!(target.exists());
        assert!(!fixture.pin().exists());
    }
}

#[test]
fn growth_after_fd_metadata_stops_the_actual_read_at_cap_plus_one() {
    let fixture = Fixture::new();
    let scope = source_scope(&fixture.owner, &fixture.source).unwrap();
    let name = fixture.source.file_name().unwrap();
    let sample = read_regular(&scope.namespace, name, name).unwrap();
    let mut grown = sample.bytes.clone();
    grown.resize(MAX_BYTES + 4096, b' ');
    let result = read_fd_with(&sample.file, &mut |phase| {
        assert!(phase == Phase::BeforeBoundedRead);
        std::fs::write(&fixture.source, &grown).unwrap();
    });
    assert!(result.is_err());
    let mut view = &sample.file;
    assert_eq!(view.stream_position().unwrap(), (MAX_BYTES + 1) as u64);
    assert_eq!(std::fs::read(&fixture.source).unwrap(), grown);
    assert!(!fixture.pin().exists());
}

#[test]
fn moving_a_private_directory_does_not_rebind_the_frozen_namespace() {
    let fixture = Fixture::new();
    let pin = fixture.preserve();
    let expected = pin.selection().clone();
    crate::capture_queue::remove_owned(&fixture.source, &fixture.owner).unwrap();
    assert_eq!(
        classify(&fixture.owner, &fixture.source, &pin),
        DeliveryMatch::Missing
    );
    let moved = fixture.private.with_extension("moved");
    std::fs::rename(&fixture.private, &moved).unwrap();
    assert!(
        Namespace::open(&moved).unwrap().identity().unwrap() == expected.private_parent_identity
    );
    assert_eq!(
        std::fs::read(moved.join("selected.pin")).unwrap(),
        expected.raw_bytes
    );
    assert_eq!(
        classify(&fixture.owner, &fixture.source, &pin),
        DeliveryMatch::Unavailable
    );
    assert!(reopen(
        &fixture.owner,
        &fixture.source,
        &moved,
        OsStr::new("selected.pin"),
        &expected
    )
    .is_err());
}

#[test]
fn a_replaced_ancestor_refuses_even_when_the_final_directory_and_pin_inodes_match() {
    let fixture = Fixture::new();
    let pin = fixture.preserve();
    let expected = pin.selection().clone();
    crate::capture_queue::remove_owned(&fixture.source, &fixture.owner).unwrap();
    assert_eq!(
        classify(&fixture.owner, &fixture.source, &pin),
        DeliveryMatch::Missing
    );
    let parent = fixture.private.parent().unwrap();
    let prior = parent.with_extension("prior");
    std::fs::rename(parent, &prior).unwrap();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(parent)
        .unwrap();
    std::fs::rename(prior.join("pins"), &fixture.private).unwrap();
    let now = Namespace::open(&fixture.private).unwrap();
    assert!(now.identity().unwrap() == expected.private_parent_identity);
    assert!(now.ancestry != expected.private_ancestry);
    assert_eq!(std::fs::read(fixture.pin()).unwrap(), expected.raw_bytes);
    assert_eq!(
        classify(&fixture.owner, &fixture.source, &pin),
        DeliveryMatch::Unavailable
    );
    assert!(reopen(
        &fixture.owner,
        &fixture.source,
        &fixture.private,
        OsStr::new("selected.pin"),
        &expected
    )
    .is_err());
}

#[test]
fn a_different_real_member_owner_cannot_preserve_reopen_or_match_the_selection() {
    let fixture = Fixture::new();
    let other = Fixture::new();
    assert!(preserve(
        &other.owner,
        &fixture.source,
        &fixture.private,
        OsStr::new("wrong-owner.pin")
    )
    .is_err());
    assert!(!fixture.private.join("wrong-owner.pin").exists());
    let pin = fixture.preserve();
    assert_eq!(
        classify(&other.owner, &fixture.source, &pin),
        DeliveryMatch::Unavailable
    );
    assert!(reopen(
        &other.owner,
        &fixture.source,
        &fixture.private,
        OsStr::new("selected.pin"),
        pin.selection()
    )
    .is_err());
    assert!(fixture.source.exists());
    assert!(other.source.exists());
    assert_eq!(
        classify(&fixture.owner, &fixture.source, &pin),
        DeliveryMatch::OriginalPresent
    );
}

#[test]
fn nested_private_directories_inside_each_actual_excluded_root_refuse() {
    let fixture = Fixture::new();
    let repo = rift_git::discover(fixture.owner.git_dir()).unwrap();
    let checkout = repo.work_tree.canonicalize().unwrap();
    let run_root = fixture
        .source
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    for root in [checkout.as_path(), fixture.owner.git_dir(), run_root] {
        let private = root.join("nested-private-pins");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&private)
            .unwrap();
        let before = std::fs::read(&fixture.source).unwrap();
        assert!(preserve(
            &fixture.owner,
            &fixture.source,
            &private,
            OsStr::new("forbidden.pin")
        )
        .is_err());
        assert!(!private.join("forbidden.pin").exists());
        assert_eq!(std::fs::read(&fixture.source).unwrap(), before);
    }
}
