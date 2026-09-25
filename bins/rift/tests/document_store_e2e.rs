//! The family's document store, through the real binary: what `rift doctor`
//! reports and what `rift clean` collects.
//!
//! The claim under test is the one a unit test cannot make: that the two
//! surfaces are actually WIRED — that the leg is reached, that both members of
//! a family see one store because the store is keyed by the common directory
//! rather than by a checkout, and that a sweep run from one member removes the
//! family's expired answer and that member's view of it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod common;
use common::rift_cmd;

/// A sidecar key: 64 lowercase hex, which is the only name the store mints.
const KEY: &str = "1111111111111111111111111111111111111111111111111111111111111111";
/// A second one, for the view that has nothing behind it.
const ORPHAN_KEY: &str = "2222222222222222222222222222222222222222222222222222222222222222";
/// A third, for the answer that is inside the repository's own TTL.
const FRESH_KEY: &str = "3333333333333333333333333333333333333333333333333333333333333333";

/// What `plant_the_store` wrote, for the sweeps to find gone or kept.
struct PlantedStore {
    /// The family's expired answer.
    sidecar: PathBuf,
    /// The answer inside the repository's own TTL.
    fresh: PathBuf,
    /// The main member's hardlink view of `sidecar`.
    main_view: PathBuf,
    /// The linked member's view whose answer does not exist.
    orphan: PathBuf,
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join("absent-gitconfig"))
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .expect("git is on PATH");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn rift_json(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> serde_json::Value {
    let mut cmd = rift_cmd(cwd);
    cmd.args(args)
        .arg("--json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (key, value) in env {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("the binary under test runs");
    assert_eq!(out.status.code(), Some(0), "rift {args:?}");
    serde_json::from_slice(&out.stdout).expect("a --json surface emits one document")
}

/// **One store, two members, both surfaces.**
///
/// A sidecar planted in the family's directory is seen from the linked
/// worktree as well as from the main one — that is the family model, and a
/// store keyed by a checkout path would report one from each. `rift clean`
/// then collects the expired answer and the view that points at nothing,
/// where the repository's own `[doc] ttl_days` says they have expired.
#[test]
fn the_document_store_is_reported_and_collected_across_a_family() {
    let tmp = tempfile::tempdir().unwrap();
    let (main, linked) = family_with_a_linked_member(tmp.path());

    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
    ];

    the_leg_is_reached_before_anything_is_planted(&main, &env);
    let planted = plant_the_store(&main, &linked);
    both_members_see_one_store(&main, &linked, &env);
    the_linked_sweep_keeps_what_the_policy_floor_keeps(&linked, &env, &planted);
    the_other_members_view_is_its_own_sweeps_orphan(&main, &env, &planted);
}

/// A main work tree with one commit and a linked worktree beside it, hooks
/// pointed at an empty directory so nothing but the test calls the binary.
fn family_with_a_linked_member(tmp: &Path) -> (PathBuf, PathBuf) {
    let main = tmp.join("main");
    let nohooks = tmp.join("nohooks");
    std::fs::create_dir_all(&main).unwrap();
    std::fs::create_dir_all(&nohooks).unwrap();
    git_in(&main, &["init", "-q", "."]);
    git_in(&main, &["config", "user.email", "t@t"]);
    git_in(&main, &["config", "user.name", "T"]);
    git_in(
        &main,
        &["config", "core.hooksPath", nohooks.to_str().unwrap()],
    );
    std::fs::write(main.join("a.txt"), "one\n").unwrap();
    git_in(&main, &["add", "a.txt"]);
    git_in(&main, &["commit", "-q", "-m", "seed"]);
    let linked = tmp.join("linked");
    git_in(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().unwrap(),
            "-b",
            "wt",
        ],
    );
    (main, linked)
}

/// The leg is REACHED before anything is planted, and says the honest empty.
/// A leg asserted only after a fixture exists is a leg that can be deleted
/// without a test noticing.
fn the_leg_is_reached_before_anything_is_planted(main: &Path, env: &[(&str, &str)]) {
    let empty = rift_json(main, &["doctor"], env);
    let store = &empty["document_store"];
    assert_eq!(store["surveyed"], true, "{empty}");
    assert_eq!(store["sidecars"], 0, "{store}");
    assert!(
        store.get("converter").is_some(),
        "the leg says whether the converter is installed at all: {store}"
    );
}

/// One answer in the family's directory — the common dir, which is `.git` for
/// the main work tree and is SHARED by the linked one — with the main member's
/// hardlink view of it, and a view in the linked member whose answer does not
/// exist.
fn plant_the_store(main: &Path, linked: &Path) -> PlantedStore {
    let family = main.join(".git").join("rift-family").join("doc");
    std::fs::create_dir_all(&family).unwrap();
    let sidecar = family.join(format!("{KEY}.md"));
    std::fs::write(&sidecar, "<!-- converted by rift -->\n\n# Spec\n").unwrap();
    // Old enough that any TTL has passed over it, without this test owning a
    // clock: age is what the sweep reads, so age is what the fixture states.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&sidecar)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
        .unwrap();
    let fresh = family.join(format!("{FRESH_KEY}.md"));
    std::fs::write(&fresh, "# converted a moment ago\n").unwrap();
    let main_view = main.join(".rift/state/doc").join(format!("{KEY}.md"));
    std::fs::create_dir_all(main_view.parent().unwrap()).unwrap();
    std::fs::hard_link(&sidecar, &main_view).unwrap();
    let orphan = linked
        .join(".rift/state/doc")
        .join(format!("{ORPHAN_KEY}.md"));
    std::fs::create_dir_all(orphan.parent().unwrap()).unwrap();
    std::fs::write(&orphan, "# an answer nobody holds any more\n").unwrap();
    PlantedStore {
        sidecar,
        fresh,
        main_view,
        orphan,
    }
}

/// Both members report one store; the orphan view is the linked member's own.
fn both_members_see_one_store(main: &Path, linked: &Path, env: &[(&str, &str)]) {
    let from_main = rift_json(main, &["doctor"], env)["document_store"].clone();
    assert_eq!(from_main["sidecars"], 2, "{from_main}");
    assert_eq!(from_main["member_links"], 1, "{from_main}");
    assert_eq!(from_main["orphan_links"], 0, "{from_main}");
    assert!(from_main["bytes"].as_u64().unwrap() > 0, "{from_main}");

    let from_linked = rift_json(linked, &["doctor"], env)["document_store"].clone();
    assert_eq!(
        from_linked["sidecars"], from_main["sidecars"],
        "both members of one family see ONE store: {from_linked}"
    );
    assert_eq!(
        from_linked["orphan_links"], 1,
        "a view with no answer behind it is this member's own: {from_linked}"
    );
}

/// The repository's own TTL is a FLOOR: this sweep asks for everything
/// (`--ttl-days 0`) and still keeps what the policy says is worth keeping,
/// because collecting a sidecar early costs a parse the next time an agent
/// opens the file.
fn the_linked_sweep_keeps_what_the_policy_floor_keeps(
    linked: &Path,
    env: &[(&str, &str)],
    planted: &PlantedStore,
) {
    let policies = linked.join(".rift").join("policies");
    std::fs::create_dir_all(&policies).unwrap();
    std::fs::write(policies.join("serve.toml"), "[doc]\nttl_days = 1\n").unwrap();

    let swept = rift_json(linked, &["clean", "--ttl-days", "0"], env);
    assert_eq!(swept["expired_doc_sidecars"], 1, "{swept}");
    assert_eq!(swept["orphan_doc_links"], 1, "{swept}");
    assert!(
        !planted.sidecar.exists(),
        "the family's expired answer is collected"
    );
    assert!(
        !planted.orphan.exists(),
        "and so is the view that pointed nowhere"
    );
    assert!(
        planted.fresh.exists(),
        "a flag may keep a sidecar longer than the policy promised, never less"
    );
}

/// The other member's view outlived the answer by one sweep, which is exactly
/// what makes it an orphan — and its own `clean` takes it.
fn the_other_members_view_is_its_own_sweeps_orphan(
    main: &Path,
    env: &[(&str, &str)],
    planted: &PlantedStore,
) {
    assert!(planted.main_view.exists());
    let after = rift_json(main, &["doctor"], env)["document_store"].clone();
    assert_eq!(
        (after["sidecars"].as_u64(), after["orphan_links"].as_u64()),
        (Some(1), Some(1)),
        "{after}"
    );
    let main_sweep = rift_json(main, &["clean", "--ttl-days", "0"], env);
    assert_eq!(main_sweep["orphan_doc_links"], 1, "{main_sweep}");
    assert!(!planted.main_view.exists());
    assert!(
        planted.fresh.exists(),
        "and the shipped default keeps it too"
    );
}
