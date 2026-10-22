//! `rift-memd rebuild` over a checkpoint that names **no commit**.
//!
//! A checkpoint minted on an unborn HEAD carries no `commit` member in its
//! `metadata.json` (absent, never `""`). The recall projection
//! takes the absence as the answer: the checkpoint lands, no
//! `n_checkpoint_commit` row is invented, no outcome row is labelled against
//! an anchor that does not exist, and the rebuild reports no parse issue.
//!
//! Hermetic as the sibling suites: a fixture repository in a tempdir and
//! every root the binary could resolve on this machine redirected.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use rift_git::{Family, FamilyState, MintPolicy};
use rift_store::CanonicalStore;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "rift-test")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "rift-test")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .expect("git is on PATH");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// A repository whose `main` is **unborn** and whose orphan checkpoint branch
/// holds one shard with no `commit` member.
fn build_anchorless_fixture(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["checkout", "-q", "--orphan", "rift/checkpoints/v1"]);
    write(
        &dir.join("ab/12/metadata.json"),
        r#"{"canon_version":3,"strategy":"manual-commit","sessions":["s-unborn"],"created_at":"2026-10-17T12:00:00.000Z"}"#,
    );
    write(
        &dir.join("ab/12/redaction_report.json"),
        r#"{"total_spans":0,"bytes_masked":0,"allowlisted":0,"rule_pack_versions":["builtin@2026.06.0"],"input_sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","by_detector":{},"by_class":{}}"#,
    );
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "checkpoints"]);
    git(dir, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    // Back on an unborn `main`: the orphan branch is the only commit here.
    git(dir, &["rm", "-rfq", "--cached", "."]);
}

fn mint_fixture_family(repo: &Path, families: &Path) -> Family {
    let paths = rift_git::discover(repo).unwrap();
    let family_dir = families.display().to_string();
    let state = rift_git::family::resolve_family_under(
        |k| (k == rift_git::family::ENV_FAMILY_DIR).then(|| family_dir.clone()),
        &paths,
        MintPolicy::Allow,
    )
    .unwrap();
    match state {
        FamilyState::Minted(family) => family,
        other => panic!("the fixture family must mint: {other:?}"),
    }
}

fn run_rebuild(cwd: &Path, repo: &Path, home: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_rift-memd"))
        .current_dir(cwd)
        .args(["rebuild", "--repo", &repo.display().to_string()])
        .env("RIFT_CACHE_DIR", home.join("cache"))
        .env("RIFT_CONFIG_DIR", home.join("config"))
        .env("RIFT_FAMILY_DIR", home.join("families"))
        .env("HOME", home)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .expect("rift-memd runs");
    assert!(
        out.status.success(),
        "rift-memd rebuild: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn count(db: &Path, table: &str) -> i64 {
    let store = rift_store::open_migrated(db, &rift_store::recall_migrations()).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let rows = rt
        .block_on(store.query(&format!("SELECT COUNT(*) FROM {table}"), &[]))
        .unwrap();
    rows[0].get_i64(0).unwrap()
}

/// The anchorless checkpoint rebuilds with no commit row, no outcome row and
/// no parse issue, from a repository whose user HEAD is unborn.
#[test]
fn rebuild_projects_an_anchorless_checkpoint_with_no_commit_or_outcome_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    let cache = home.join("cache");
    let families = home.join("families");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(&families).unwrap();

    build_anchorless_fixture(&repo);
    assert!(
        !Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", "HEAD"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success(),
        "the fixture's HEAD is unborn"
    );
    let family = mint_fixture_family(&repo, &families);
    let stdout = run_rebuild(tmp.path(), &repo, &home);
    assert!(
        !stdout.contains("parse issues"),
        "an absent anchor is not an issue: {stdout}"
    );
    assert!(stdout.contains("checkpoints:    1"), "{stdout}");

    let db: PathBuf = rift_store::recall_db_for_family_under(&cache, &family);
    assert!(
        db.exists(),
        "the rebuild wrote no store at {}",
        db.display()
    );
    assert_eq!(count(&db, "n_checkpoint"), 1);
    assert_eq!(
        count(&db, "n_checkpoint_commit"),
        0,
        "no anchor row is invented"
    );
    assert_eq!(
        count(&db, "outcome_label"),
        0,
        "nothing is labelled against a missing anchor"
    );
}
