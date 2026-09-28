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

use rift_git::{Family, FamilyState, MintPolicy};
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
    let (_tmp, db) = fixture_rebuilt_store();
    let paths = projected_paths(&db, "SELECT path FROM n_file_revision ORDER BY path ASC");
    assert_eq!(
        paths,
        vec![KEYED.to_owned()],
        "the rebuild keyed its rows by the shard's frozen spelling"
    );
    assert!(
        !paths.iter().any(|p| p.contains('\\')),
        "a rebuild must not reproduce a separator no lookup spells: {paths:?}"
    );
}

/// The consequence, not just the column: the content→blob-oid map is built
/// from the same paths, so the bridge found the file the shard named.
#[test]
fn rebuild_maps_the_slash_spelled_path_to_its_blob() {
    let (_tmp, db) = fixture_rebuilt_store();
    let mapped = projected_paths(&db, "SELECT path FROM n_blob_oid_map");
    assert_eq!(
        mapped,
        vec![KEYED.to_owned()],
        "a `\\`-keyed path names no file in the checkout, so nothing would map"
    );
}

/// The fixture repository rebuilt by the shipped binary under redirected
/// roots: the tempdir that holds it all (kept alive by the caller) and the
/// store the rebuild wrote.
fn fixture_rebuilt_store() -> (tempfile::TempDir, PathBuf) {
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
    let family = mint_fixture_family(&repo, &families);
    run_rebuild(tmp.path(), &repo, &home);

    // The store the rebuild wrote — resolved under the redirected cache root,
    // so this can only ever name the fixture's own projection.
    let db: PathBuf = rift_store::recall_db_for_family_under(&cache, &family);
    assert!(
        db.exists(),
        "the rebuild wrote no store at {}",
        db.display()
    );
    (tmp, db)
}

/// Mint the family through the SAME override the child will read, so the
/// manifest the binary resolves is this fixture's and never the machine's.
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

/// Run the shipped `rift-memd rebuild` over `repo` with every root it could
/// resolve on the developer's machine redirected under `home`.
fn run_rebuild(cwd: &Path, repo: &Path, home: &Path) {
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
}

/// The first column of every row `sql` selects from the store the shipped
/// binary produced: the `path`s a rebuild keyed.
fn projected_paths(db: &Path, sql: &str) -> Vec<String> {
    let store = rift_store::open_migrated(db, &rift_store::recall_migrations()).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let rows = rt.block_on(store.query(sql, &[])).unwrap();
    rows.iter().map(|r| r.get_text(0).unwrap()).collect()
}
