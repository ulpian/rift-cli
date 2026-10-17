//! A recovery hold must retain historical intent before ordinary collection.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::agent_queue::{AgentJob, BoundaryEdge, BoundarySnapshot, JobKind};
use rift_store::CanonicalStore as _;

struct FrozenDelivery {
    path: PathBuf,
    bytes: Vec<u8>,
    attempts: u32,
}

struct Observed {
    snapshots: i64,
    collections: i64,
    remaining: usize,
    preserved: Vec<bool>,
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(["-c", "user.name=Fixture", "-c", "user.email=fixture-id"])
        .args([
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
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn boundary_job(root: &Path, head: &str, ordinal: u32) -> AgentJob {
    let at = i64::from(ordinal) * 1000;
    AgentJob {
        schema_version: 1,
        work_tree: root.to_owned(),
        queued_at_ms: at,
        attempts: ordinal - 1,
        kind: JobKind::BoundarySnapshot(BoundarySnapshot {
            session_id: "historical-recovery-fixture".into(),
            agent: "claude-code".into(),
            ordinal,
            boundary: BoundaryEdge::Start,
            head_oid: Some(head.to_owned()),
            captured_at_ms: at,
        }),
    }
}

fn collection_counts(root: &Path) -> (i64, i64) {
    let store =
        rift_store::open_capture_projection(super::projection_db_at(root).unwrap()).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    (
        rt.block_on(store.count("turn_boundary_snapshot")).unwrap(),
        rt.block_on(store.count("turn_boundary_observation"))
            .unwrap(),
    )
}

fn freeze_deliveries(git_dir: &Path) -> Vec<FrozenDelivery> {
    let mut frozen: Vec<_> = crate::agent_queue::jobs(git_dir)
        .into_iter()
        .map(|(path, job)| FrozenDelivery {
            bytes: std::fs::read(&path).unwrap(),
            path,
            attempts: job.attempts,
        })
        .collect();
    frozen.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(frozen.len(), 3);
    assert_eq!(
        frozen
            .iter()
            .map(|entry| entry.attempts)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([0, 1, 2])
    );
    frozen
}

fn publish_inert_hold(git_dir: &Path) {
    let path = git_dir.join(".rift-capture-recovery.hold");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    let bytes = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "operation": "fixture-recovery",
        "member_key": "11".repeat(32),
        "plan_sha256": "22".repeat(32),
    }))
    .unwrap();
    file.write_all(&bytes).unwrap();
    file.sync_all().unwrap();
}

fn observe_after_drain(root: &Path, git_dir: &Path, frozen: &[FrozenDelivery]) -> Observed {
    let (snapshots, collections) = collection_counts(root);
    let remaining = crate::agent_queue::jobs(git_dir).len();
    let preserved: Vec<_> = frozen
        .iter()
        .map(|entry| {
            let bytes_equal = std::fs::read(&entry.path).is_ok_and(|bytes| bytes == entry.bytes);
            let attempts = crate::agent_queue::read_job(&entry.path).ok().map(|(_, job)| job.attempts);
            eprintln!(
                "RECOVERY_HOLD_DELIVERY path={} present={} bytes_equal={bytes_equal} expected_attempts={} actual_attempts={attempts:?}",
                entry.path.display(), entry.path.exists(), entry.attempts
            );
            bytes_equal && attempts == Some(entry.attempts)
        })
        .collect();
    eprintln!("RECOVERY_HOLD_OBSERVED snapshots={snapshots} observations={collections} remaining={remaining} preserved={preserved:?}");
    Observed {
        snapshots,
        collections,
        remaining,
        preserved,
    }
}

fn prepared_checkout(
    root: &Path,
) -> (
    rift_git::RepoPaths,
    crate::capture_queue::QueueLock,
    Vec<FrozenDelivery>,
) {
    git(root, &["init", "--quiet"]);
    std::fs::write(root.join("a.txt"), "committed\n").unwrap();
    git(root, &["add", "a.txt"]);
    git(root, &["commit", "--quiet", "-m", "fixture"]);
    crate::family::resolve_at(root).unwrap();
    std::fs::write(root.join("a.txt"), "committed\ncurrent edit\n").unwrap();
    let repo = rift_git::discover(root).unwrap();
    let head = git(root, &["rev-parse", "HEAD"]).trim().to_owned();
    for ordinal in 1..=3 {
        crate::agent_queue::enqueue(&repo.git_dir, &boundary_job(root, &head, ordinal)).unwrap();
    }
    let owner = crate::capture_queue::acquire_owner(&repo.git_dir).unwrap();
    crate::family::run_state::prepare_owned(&owner).unwrap();
    let frozen = freeze_deliveries(&repo.git_dir);
    assert_eq!(collection_counts(root), (0, 0));
    (repo, owner, frozen)
}

fn drain(root: &Path, repo: &rift_git::RepoPaths, frozen: &[FrozenDelivery]) -> Observed {
    let owner = crate::capture_queue::acquire_owner(&repo.git_dir).unwrap();
    let timer = crate::hooktime::HookTimer::new("recovery-hold-fixture", false);
    let mut attempted = BTreeSet::new();
    super::drain_locked(&timer, &repo.git_dir, &owner, &mut attempted);
    observe_after_drain(root, &repo.git_dir, frozen)
}

fn run_fixture(hold: bool) -> Observed {
    let checkout = tempfile::tempdir().unwrap();
    let root = checkout.path();
    let (repo, owner, frozen) = prepared_checkout(root);
    if hold {
        publish_inert_hold(owner.git_dir());
    }
    drop(owner);
    drain(root, &repo, &frozen)
}

fn assert_held(observed: &Observed, count: usize) {
    assert_eq!((observed.snapshots, observed.collections), (0, 0));
    assert_eq!(observed.remaining, count);
    assert!(observed.preserved.iter().all(|preserved| *preserved));
}

#[test]
fn a_recovery_hold_preserves_three_historical_deliveries_without_collecting() {
    let observed = run_fixture(true);
    assert_eq!((observed.snapshots, observed.collections), (0, 0));
    assert_eq!(observed.remaining, 3);
    assert_eq!(observed.preserved, [true; 3]);
}

#[test]
fn three_historical_deliveries_without_a_hold_really_collect_and_settle() {
    let observed = run_fixture(false);
    assert_eq!((observed.snapshots, observed.collections), (3, 3));
    assert_eq!(observed.remaining, 0);
    assert_eq!(observed.preserved, [false; 3]);
}

#[test]
fn malformed_unknown_and_directory_holds_all_retain_historical_deliveries() {
    for kind in ["malformed", "unknown", "directory"] {
        let checkout = tempfile::tempdir().unwrap();
        let root = checkout.path();
        let (repo, owner, frozen) = prepared_checkout(root);
        let hold = owner.git_dir().join(".rift-capture-recovery.hold");
        match kind {
            "malformed" => std::fs::write(&hold, b"not JSON").unwrap(),
            "unknown" => std::fs::write(&hold, b"{\"schema_version\":999}").unwrap(),
            _ => std::fs::create_dir(&hold).unwrap(),
        }
        drop(owner);
        assert_held(&drain(root, &repo, &frozen), 3);
        assert!(hold.symlink_metadata().is_ok());
    }
}

#[cfg(unix)]
#[test]
fn a_dangling_symlink_hold_retains_historical_deliveries_without_following_it() {
    let checkout = tempfile::tempdir().unwrap();
    let root = checkout.path();
    let (repo, owner, frozen) = prepared_checkout(root);
    let hold = owner.git_dir().join(".rift-capture-recovery.hold");
    let target = root.join("absent-hold-target");
    std::os::unix::fs::symlink(&target, &hold).unwrap();
    drop(owner);
    assert_held(&drain(root, &repo, &frozen), 3);
    assert_eq!(std::fs::read_link(hold).unwrap(), target);
    assert!(!target.exists());
}

#[test]
fn direct_migration_removal_and_retry_refuse_a_hold_without_spending_attempts() {
    let checkout = tempfile::tempdir().unwrap();
    let root = checkout.path();
    let (repo, owner, frozen) = prepared_checkout(root);
    publish_inert_hold(owner.git_dir());
    let hold = owner.git_dir().join(".rift-capture-recovery.hold");
    let held_bytes = std::fs::read(&hold).unwrap();
    let error = crate::family::run_state::prepare_owned(&owner).unwrap_err();
    assert!(crate::capture_queue::is_deferred(&error));
    let queue = crate::agent_queue::queue_dir(&repo.git_dir);
    let member = queue.parent().unwrap();
    let ctx = crate::family::run_state::MoveCtx {
        git_dir: &repo.git_dir,
        run_root: member.parent().unwrap(),
        member,
        now_ms: 4000,
    };
    assert!(crate::family::run_state::adopt_owned(&ctx, &owner).is_err());
    for entry in &frozen {
        let (_, job) = crate::agent_queue::read_job(&entry.path).unwrap();
        let removed = crate::capture_queue::remove_owned(&entry.path, &owner).unwrap_err();
        assert!(crate::capture_queue::is_deferred(&removed));
        let retried = crate::agent_queue::record_failure_owned(
            &entry.path,
            &job,
            "fixture execution failure",
            &owner,
        )
        .unwrap_err();
        assert!(crate::capture_queue::is_deferred(&retried));
    }
    assert_held(&observe_after_drain(root, &repo.git_dir, &frozen), 3);
    drop(owner);
    assert!(crate::family::run_state::adopt(&ctx).is_err());
    assert_eq!(std::fs::read(hold).unwrap(), held_bytes);
}

#[test]
fn a_new_producer_delivery_can_publish_under_a_hold_and_remains_owed() {
    let checkout = tempfile::tempdir().unwrap();
    let root = checkout.path();
    let (repo, owner, frozen) = prepared_checkout(root);
    publish_inert_hold(owner.git_dir());
    let head = git(root, &["rev-parse", "HEAD"]).trim().to_owned();
    let mut job = boundary_job(root, &head, 4);
    job.attempts = 0;
    let path = crate::agent_queue::enqueue(&repo.git_dir, &job).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(crate::agent_queue::read_job(&path).unwrap().1, job);
    drop(owner);
    assert_held(&drain(root, &repo, &frozen), 4);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(crate::agent_queue::read_job(&path).unwrap().1, job);
}

#[cfg(unix)]
fn physical_identity(path: &Path) -> (u64, u64, u32) {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = path.symlink_metadata().unwrap();
    (metadata.dev(), metadata.ino(), metadata.mode())
}

#[cfg(unix)]
fn pin_delivery(
    owner: &crate::capture_queue::QueueLock,
    source: &Path,
    private: &Path,
) -> (
    std::fs::File,
    PathBuf,
    crate::capture_recovery::pins::LivePin,
) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(private, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(physical_identity(private).2 & 0o7777, 0o700);
    let file = std::fs::File::open(source).unwrap();
    let pin = private.join("original-delivery.pin");
    let live = crate::capture_recovery::pins::preserve(
        owner,
        &source.canonicalize().unwrap(),
        &private.canonicalize().unwrap(),
        std::ffi::OsStr::new("original-delivery.pin"),
    )
    .unwrap();
    assert_eq!(physical_identity(source), physical_identity(&pin));
    (file, pin, live)
}

#[cfg(unix)]
fn remove_matching_fixture(
    owner: &crate::capture_queue::QueueLock,
    replacement: &Path,
    live_pin: &crate::capture_recovery::pins::LivePin,
) {
    let classification = crate::capture_recovery::pins::classify(
        owner,
        &replacement.canonicalize().unwrap(),
        live_pin,
    );
    assert_eq!(
        classification,
        crate::capture_recovery::pins::DeliveryMatch::Different
    );
    if classification == crate::capture_recovery::pins::DeliveryMatch::OriginalPresent {
        crate::capture_queue::remove_owned(replacement, owner).unwrap();
    }
}

/// This is a resumed physical-cut requirement, not the ordinary completion
/// contract: equal context and bytes cannot identify a previously selected file.
#[cfg(unix)]
#[test]
fn a_resumed_physical_cut_must_not_unlink_an_identical_redelivery_at_a_new_inode() {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;

    let checkout = tempfile::tempdir().unwrap();
    let private = tempfile::tempdir().unwrap();
    let (repo, owner, frozen) = prepared_checkout(checkout.path());
    let original = &frozen[0];
    assert!(!private.path().starts_with(checkout.path()));
    assert!(!private
        .path()
        .starts_with(original.path.parent().unwrap().parent().unwrap()));
    let (mut live_source, pin, live_pin) = pin_delivery(&owner, &original.path, private.path());
    let original_identity = physical_identity(&original.path);
    let (_, job) = crate::agent_queue::read_job(&original.path).unwrap();
    let original_context = crate::capture_queue::delivery::decode(&original.bytes).unwrap();
    owner.check_recovery_hold().unwrap();
    crate::capture_queue::remove_owned(&original.path, &owner).unwrap();
    assert!(!original.path.exists());
    let replacement = crate::agent_queue::enqueue(&repo.git_dir, &job).unwrap();
    let replacement_bytes = std::fs::read(&replacement).unwrap();
    let replacement_context = crate::capture_queue::delivery::decode(&replacement_bytes).unwrap();
    let replacement_identity = physical_identity(&replacement);
    let same_path = replacement == original.path;
    let same_bytes = replacement_bytes == original.bytes;
    let same_context = replacement_context == original_context;
    eprintln!(
        "PHYSICAL_CUT_REDELIVERY same_path={same_path} same_bytes={same_bytes} same_context={same_context} original={original_identity:?} pin={:?} replacement={replacement_identity:?} attempts={}",
        physical_identity(&pin), job.attempts
    );
    assert!(same_path && same_bytes && same_context);
    assert_eq!(original_identity, physical_identity(&pin));
    assert_eq!(original_identity.0, replacement_identity.0);
    assert_ne!(original_identity.1, replacement_identity.1);
    assert_eq!(original_identity.2, replacement_identity.2);
    assert_eq!(live_source.metadata().unwrap().ino(), original_identity.1);
    let mut retained_bytes = Vec::new();
    live_source.read_to_end(&mut retained_bytes).unwrap();
    assert_eq!(retained_bytes, original.bytes);
    assert_eq!(crate::agent_queue::read_job(&replacement).unwrap().1, job);
    remove_matching_fixture(&owner, &original.path, &live_pin);
    eprintln!(
        "PHYSICAL_CUT_AFTER_CLASSIFICATION replacement_present={}",
        replacement.exists()
    );
    assert!(
        replacement.exists(),
        "the newly delivered physical file remains owed"
    );
    assert_eq!(std::fs::read(replacement).unwrap(), replacement_bytes);
    assert_eq!(std::fs::read(pin).unwrap(), original.bytes);
}

#[cfg(unix)]
#[test]
fn an_unchanged_selected_physical_delivery_can_be_unlinked_without_changing_its_pin() {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;

    let checkout = tempfile::tempdir().unwrap();
    let private = tempfile::tempdir().unwrap();
    let (_, owner, frozen) = prepared_checkout(checkout.path());
    let original = &frozen[0];
    assert!(!private.path().starts_with(checkout.path()));
    assert!(!private
        .path()
        .starts_with(original.path.parent().unwrap().parent().unwrap()));
    let (mut live_source, pin, live_pin) = pin_delivery(&owner, &original.path, private.path());
    let selected = physical_identity(&original.path);
    assert_eq!(
        live_pin.original_identity(),
        (selected.0, selected.1, selected.2 & 0o7777)
    );
    assert_eq!(
        crate::capture_recovery::pins::classify(
            &owner,
            &original.path.canonicalize().unwrap(),
            &live_pin,
        ),
        crate::capture_recovery::pins::DeliveryMatch::OriginalPresent
    );
    assert_eq!(physical_identity(&pin), selected);
    assert_eq!(live_source.metadata().unwrap().ino(), selected.1);
    owner.check_recovery_hold().unwrap();
    crate::capture_queue::remove_owned(&original.path, &owner).unwrap();
    let mut retained_bytes = Vec::new();
    live_source.read_to_end(&mut retained_bytes).unwrap();
    eprintln!(
        "PHYSICAL_CUT_UNCHANGED selected={selected:?} pin={:?} source_present={} attempts={}",
        physical_identity(&pin),
        original.path.exists(),
        original.attempts
    );
    assert!(!original.path.exists());
    assert_eq!(physical_identity(&pin), selected);
    assert_eq!(retained_bytes, original.bytes);
    assert_eq!(std::fs::read(pin).unwrap(), original.bytes);
    for other in &frozen[1..] {
        assert_eq!(std::fs::read(&other.path).unwrap(), other.bytes);
        assert_eq!(
            crate::agent_queue::read_job(&other.path)
                .unwrap()
                .1
                .attempts,
            other.attempts
        );
    }
}
