//! `rift-memd rebuild` re-spells the history it rebuilds from.
//!
//! The defect this pins: a checkpoint shard frozen by a Windows capture from
//! before the `/` rule carries `\`-spelled read facts, and history is
//! append-only — those bytes stay on the branch for as long as the branch
//! exists. So "the projection is rebuildable" is only a remedy if the rebuild
//! itself keys its rows the way every lookup spells them. It does, through the
//! one normaliser at the recall ingest's parse
//! (`rift_recall_substrate::worktree::slash_spelled`), and this walks the
//! whole road: a real orphan checkpoint branch, the shipped binary, the store
//! it actually writes.
//!
//! Hermetic by construction: a fixture repository in a tempdir, every root the
//! binary might otherwise resolve on the developer's machine redirected
//! (`RIFT_CACHE_DIR`, `RIFT_CONFIG_DIR`, `RIFT_FAMILY_DIR`, `HOME`), and the
//! family minted through the same override the child reads. Nothing here can
//! reach — let alone rebuild — a real projection.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use rift_git::{FamilyState, MintPolicy};
use rift_store::CanonicalStore;

/// The path the fixture's one read fact names, in the two spellings: the one
/// the shard froze, and the one every row and every lookup uses.
const FROZEN: &str = r"bins\\rift\\src\\hooks.rs";
const KEYED: &str = "bins/rift/src/hooks.rs";

const HOOKS_RS: &str = "fn main() {}\n";

/// Run `git` in `dir` with a fixed identity and no ambient re-targeting.
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

/// A repository with a real `main` branch and an orphan checkpoint branch
/// holding one shard whose read fact is `\`-spelled — the shape a Windows
/// capture froze before the `/` rule.
fn build_fixture(dir: &Path) {
    let hash = rift_recall_substrate::worktree::sha256_hex(HOOKS_RS.as_bytes());

    git(dir, &["init", "-q", "-b", "main"]);
    write(&dir.join("bins/rift/src/hooks.rs"), HOOKS_RS);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "worktree"]);

    git(dir, &["checkout", "-q", "--orphan", "rift/checkpoints/v1"]);
    git(dir, &["rm", "-rfq", "."]);
    write(
        &dir.join("ab/12/tool_calls.jsonl"),
        &format!(
            r#"{{"id":"t1","tool":"Read","path":"{FROZEN}","sha256":"{hash}","input_tokens":11}}"#
        ),
    );
    write(
        &dir.join("ab/12/metadata.json"),
        r#"{"strategy":"manual-commit"}"#,
    );
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "checkpoints"]);
    git(dir, &["checkout", "-q", "main"]);
}

/// The frozen spelling really is on the branch — the premise the rest of this
/// test stands on, read back out of git rather than assumed.
fn assert_history_kept_the_frozen_spelling(dir: &Path) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["show", "rift/checkpoints/v1:ab/12/tool_calls.jsonl"])
        .output()
        .expect("git is on PATH");
    assert!(out.status.success());
    let line = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        line.contains(FROZEN),
        "the fixture shard must carry the frozen spelling: {line}"
    );
}

#[test]
fn rebuild_projects_slash_spelled_rows_from_a_backslash_spelled_shard() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    let cache = home.join("cache");
    let families = home.join("families");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(&families).unwrap();

    build_fixture(&repo);
    assert_history_kept_the_frozen_spelling(&repo);

    // Mint the family through the SAME override the child will read, so the
    // manifest the binary resolves is this fixture's and never the machine's.
    let paths = rift_git::discover(&repo).unwrap();
    let family_dir = families.display().to_string();
    let state = rift_git::family::resolve_family_under(
        |k| (k == rift_git::family::ENV_FAMILY_DIR).then(|| family_dir.clone()),
        &paths,
        MintPolicy::Allow,
    )
    .unwrap();
    let family = match state {
        FamilyState::Minted(family) => family,
        other => panic!("the fixture family must mint: {other:?}"),
    };

    let out = Command::new(env!("CARGO_BIN_EXE_rift-memd"))
        .current_dir(tmp.path())
        .args(["rebuild", "--repo", &repo.display().to_string()])
        .env("RIFT_CACHE_DIR", &cache)
        .env("RIFT_CONFIG_DIR", home.join("config"))
        .env("RIFT_FAMILY_DIR", &families)
        .env("HOME", &home)
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

    // The store the rebuild wrote — resolved under the redirected cache root,
    // so this can only ever name the fixture's own projection.
    let db: PathBuf = rift_store::recall_db_for_family_under(&cache, &family);
    assert!(
        db.exists(),
        "the rebuild wrote no store at {}",
        db.display()
    );
    let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    let rows = rt
        .block_on(store.query("SELECT path FROM n_file_revision ORDER BY path ASC", &[]))
        .unwrap();
    let paths: Vec<String> = rows.iter().map(|r| r.get_text(0).unwrap()).collect();
    assert_eq!(
        paths,
        vec![KEYED.to_owned()],
        "the rebuild keyed its rows by the shard's frozen spelling"
    );
    assert!(
        !paths.iter().any(|p| p.contains('\\')),
        "a rebuild must not reproduce a separator no lookup spells: {paths:?}"
    );

    // The consequence, not just the column: the content→blob-oid map is built
    // from the same paths, so the bridge found the file the shard named.
    let mapped = rt
        .block_on(store.query("SELECT path FROM n_blob_oid_map", &[]))
        .unwrap();
    let mapped: Vec<String> = mapped.iter().map(|r| r.get_text(0).unwrap()).collect();
    assert_eq!(
        mapped,
        vec![KEYED.to_owned()],
        "a `\\`-keyed path names no file in the checkout, so nothing would map"
    );
}
