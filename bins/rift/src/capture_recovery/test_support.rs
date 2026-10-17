//! Real filesystem fixtures only; no production admission shortcut.
#![cfg(unix)]
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::PathBuf;

use super::plan::{member_key, Plan};
use crate::capture_queue::QueueLock;

pub(crate) struct Fixture {
    _checkout: tempfile::TempDir,
    _private: tempfile::TempDir,
    pub(crate) owner: QueueLock,
    pub(crate) work_tree: PathBuf,
    pub(crate) evidence: PathBuf,
    pub(crate) member: PathBuf,
    pub(crate) run_root: PathBuf,
    pub(crate) plan_bytes: Vec<u8>,
}

#[derive(Clone, Copy)]
enum Setup {
    Warm,
    Cold,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        Self::build(Setup::Warm)
    }

    pub(crate) fn cold() -> Self {
        Self::build(Setup::Cold)
    }

    fn build(setup: Setup) -> Self {
        let checkout = tempfile::tempdir().unwrap();
        let initial_owner = if matches!(setup, Setup::Cold) {
            let admin = checkout.path().join(".git");
            std::fs::create_dir(&admin).unwrap();
            let owner = crate::capture_queue::acquire_owner(&admin).unwrap();
            let metadata = std::fs::symlink_metadata(
                crate::capture_queue::owner_path(owner.git_dir()).unwrap(),
            )
            .unwrap();
            Some((owner, (metadata.dev(), metadata.ino())))
        } else {
            None
        };
        git(checkout.path(), &["init", "--quiet"]);
        std::fs::write(checkout.path().join("a"), b"fixture\n").unwrap();
        git(checkout.path(), &["add", "a"]);
        git(checkout.path(), &["commit", "--quiet", "-m", "fixture"]);
        let work_tree = checkout.path().canonicalize().unwrap();
        let repo = rift_git::discover(&work_tree).unwrap();
        match setup {
            Setup::Warm => {
                crate::family::resolve_at(&work_tree).unwrap();
            }
            Setup::Cold => {
                assert!(matches!(
                    rift_git::resolve_family(&repo, rift_git::MintPolicy::Allow).unwrap(),
                    rift_git::FamilyState::Minted(_)
                ));
            }
        }
        let owner = match initial_owner {
            Some((owner, identity)) => {
                let metadata = std::fs::symlink_metadata(
                    crate::capture_queue::owner_path(owner.git_dir()).unwrap(),
                )
                .unwrap();
                assert_eq!((metadata.dev(), metadata.ino()), identity);
                assert_eq!(repo.git_dir.canonicalize().unwrap(), owner.git_dir());
                owner.check_current().unwrap();
                owner
            }
            None => crate::capture_queue::acquire_owner(&repo.git_dir).unwrap(),
        };
        if matches!(setup, Setup::Warm) {
            crate::family::run_state::prepare_owned(&owner).unwrap();
        }
        let rift_git::FamilyState::Minted(family) =
            rift_git::resolve_family(&repo, rift_git::MintPolicy::Never).unwrap()
        else {
            panic!("fixture family")
        };
        let family_dir = rift_store::projection_dir_under(
            &crate::family::cache_root().canonicalize().unwrap(),
            &family.family_id,
        );
        let member = rift_shadow::hooks::run_state::member_dir(&family_dir, &work_tree);
        if matches!(setup, Setup::Cold) {
            for path in [&member, member.parent().unwrap()] {
                assert!(path
                    .symlink_metadata()
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound));
            }
        }
        Self::with_records(checkout, owner, work_tree, member, family.family_id)
    }

    fn with_records(
        checkout: tempfile::TempDir,
        owner: QueueLock,
        work_tree: PathBuf,
        member: PathBuf,
        repo_id: rift_core::RepoId,
    ) -> Self {
        let private = tempfile::tempdir().unwrap();
        let evidence = private.path().join("operation");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&evidence)
            .unwrap();
        let evidence = evidence.canonicalize().unwrap();
        let key = member_key(&repo_id, owner.git_dir().to_str().unwrap()).unwrap();
        let value = serde_json::json!({"schema_version":1,"operation_id":"fixture-op","repo_id":repo_id.as_str(),"member_key":key.to_hex(),"canonical_git_dir":owner.git_dir(),"canonical_work_tree":work_tree,"evidence_dir":evidence,"selector":{"kind":"boundary-snapshots-only","schema_version":1,"unknown_fields":"refuse","inventory":"complete-bounded","reservation":"one-eligible-explicit-current-boundary@1"},"budgets":{"max_inventory_entries":8192,"max_selected_tickets":8192,"max_ticket_bytes":65536,"max_total_ticket_bytes":67_108_864,"max_cut_bytes":8_388_608,"max_artifact_bytes":8_388_608}});
        let plan = Plan::decode(&serde_json::to_vec(&value).unwrap()).unwrap();
        let plan_bytes = plan.encode().unwrap();
        let hold = serde_json::json!({"schema_version":1,"operation_id":"fixture-op","member_key":key.to_hex(),"plan_sha256":plan.sha256().unwrap().as_str()});
        write_record(&evidence.join("plan.json"), &plan_bytes);
        write_record(
            &owner.git_dir().join(".rift-capture-recovery.hold"),
            &crate::capture_queue::delivery::encode(&hold).unwrap(),
        );
        Self {
            _checkout: checkout,
            _private: private,
            owner,
            work_tree,
            evidence,
            run_root: member.parent().unwrap().to_owned(),
            member,
            plan_bytes,
        }
    }

    pub(crate) fn hold_path(&self) -> PathBuf {
        self.owner.git_dir().join(".rift-capture-recovery.hold")
    }
}

pub(crate) fn write_record(path: &std::path::Path, bytes: &[u8]) {
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

pub(crate) fn git(root: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .current_dir(root)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
        .output()
        .unwrap();
    assert!(output.status.success(), "fixture git command failed");
}
