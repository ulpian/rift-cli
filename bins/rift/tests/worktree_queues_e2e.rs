//! End-to-end: **a family's queues are reachable from
//! any member, and a member that goes away does not take its capture with it.**
//!
//! The capture and agent-hook queues stay under each member's own gitdir by
//! design — the credit discriminator — and that design has one
//! sharp edge: the only process that ever drains a member's queue is one
//! running in that member, and a member whose checkout has been removed has no
//! such process, ever again. Claude Code runs subagents in temporary worktrees
//! it deletes when they finish, so this is not a corner case; it is the ordinary
//! end of an ordinary session.
//!
//! Two answers, both driven through the shipped binary against a real
//! `git worktree add`: `rift doctor --fix` drains a sibling's queue from
//! wherever it is run, and an entry whose working tree is gone condenses
//! **commit-only** — the commit object is in the family's shared store even
//! when the tree is not — leaving an `archive_decision` that says so rather
//! than a silence.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};
use std::process::Command;

mod common;
use common::rift_cmd;

/// Hermetic `git`: no system/global config, fixed identity, every re-targeting
/// variable scrubbed.
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
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .expect("git is on PATH");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A family: a main work tree and one linked member, with a policy file in
/// each (the fixture leaves it untracked, exactly as `rift init` finds it).
struct Family {
    _tmp: tempfile::TempDir,
    main: PathBuf,
    member: PathBuf,
    /// The member's administrative gitdir, `<common>/worktrees/member`.
    admin: PathBuf,
    cache: PathBuf,
}

const POLICY: &str = "schema_version = 1\n\
     [project]\nname = \"fixture\"\n\
     [paths]\nsensitive_paths = [\"secrets/**\"]\n";

fn family() -> Family {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let main = root.join("main");
    std::fs::create_dir_all(&main).unwrap();
    git_in(&main, &["init", "-q", "-b", "main"]);
    std::fs::write(main.join("a.txt"), "a\n").unwrap();
    git_in(&main, &["add", "a.txt"]);
    git_in(&main, &["commit", "-q", "-m", "seed"]);
    std::fs::create_dir_all(main.join(".rift")).unwrap();
    std::fs::write(main.join(".rift/config.toml"), POLICY).unwrap();

    let member = root.join("member");
    git_in(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "member",
            member.to_str().unwrap(),
        ],
    );
    std::fs::create_dir_all(member.join(".rift")).unwrap();
    std::fs::write(member.join(".rift/config.toml"), POLICY).unwrap();

    Family {
        admin: main.join(".git").join("worktrees").join("member"),
        cache: root.join("cache"),
        _tmp: tmp,
        main,
        member,
    }
}

/// Run `rift` from `cwd` with the family's own cache root; return
/// `(code, stdout, stderr)`.
fn rift(f: &Family, cwd: &Path, args: &[&str]) -> (i32, String, String) {
    let out = rift_cmd(cwd)
        .args(args)
        .env("RIFT_CACHE_DIR", &f.cache)
        .env("RIFT_CONFIG_DIR", f.main.join(".rift-conf"))
        .env("RIFT_HEADLESS", "1")
        .output()
        .expect("rift runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn doctor_json(f: &Family, cwd: &Path, fix: bool) -> serde_json::Value {
    let mut args = vec!["--json", "doctor"];
    if fix {
        args.push("--fix");
    }
    let (code, out, err) = rift(f, cwd, &args);
    assert_eq!(code, 0, "doctor failed: {out}{err}");
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("doctor json: {e}: {out}"))
}

/// A commit made in the member, and a capture-queue entry for it planted in
/// the member's own run state — the shape `post-commit` leaves behind when it
/// defers the condense to the detached worker.
fn plant_member_commit(f: &Family) -> String {
    std::fs::write(f.member.join("w.txt"), "work\n").unwrap();
    git_in(&f.member, &["add", "w.txt"]);
    git_in(&f.member, &["commit", "-q", "-m", "member work"]);
    let commit = git_in(&f.member, &["rev-parse", "HEAD"]).trim().to_owned();

    let dir = common::capture_queue_dir(&f.admin, &f.cache);
    std::fs::create_dir_all(&dir).unwrap();
    let entry = serde_json::json!({
        "schema_version": 1,
        "commit": commit,
        "work_tree": f.member.display().to_string(),
        "session_id": null,
        "agent": null,
        "session_state": null,
        "queued_at_ms": 1_700_000_000_000_i64,
        "attempts": 0,
    });
    std::fs::write(
        dir.join(format!("{commit}.json")),
        serde_json::to_vec(&entry).unwrap(),
    )
    .unwrap();
    commit
}

/// One scalar out of the main work tree's projection, or 0 when there is none.
fn main_projection_i64(f: &Family, sql: &str) -> i64 {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        let repo_id = rift_git::repo_id_for(&f.main).unwrap();
        let db = rift_store::projection_db_under(&f.cache, &repo_id);
        if !db.exists() {
            return 0;
        }
        let store = rift_store::open_capture_projection(db).unwrap();
        rift_store::CanonicalStore::query_opt_i64(&store, sql, &[])
            .await
            .unwrap()
            .unwrap_or(0)
    })
}

/// **A sibling's queue is drainable from anywhere.** The member's capture queue
/// lives under the member's gitdir and nothing running in the main work tree
/// used to look at it, so a member that stopped committing stranded its
/// deferred condense forever. `rift doctor` names it; `--fix` drains it.
#[test]
fn doctor_fix_drains_a_siblings_capture_queue() {
    let f = family();
    let commit = plant_member_commit(&f);
    let queued = common::capture_queue_dir(&f.admin, &f.cache).join(format!("{commit}.json"));
    assert!(queued.is_file(), "the entry is planted under the member");

    // Report first: doctor run from MAIN sees the member's backlog and leaves
    // it alone, because a diagnosis that repairs on sight leaves nothing to
    // look at.
    let doc = doctor_json(&f, &f.main, false);
    let siblings = doc["capture_queue"]["siblings"].as_array().unwrap();
    assert_eq!(siblings.len(), 1, "the member is named: {doc}");
    assert_eq!(siblings[0]["name"], "member");
    assert_eq!(siblings[0]["pending"], 1);
    assert_eq!(siblings[0]["prunable"], false);
    assert!(queued.is_file(), "a report-only doctor drains nothing");

    // …then the fix, still run from MAIN.
    let doc = doctor_json(&f, &f.main, true);
    assert!(!queued.exists(), "--fix drained the sibling's queue: {doc}");
    assert!(
        doc["capture_queue"]["siblings"]
            .as_array()
            .unwrap()
            .is_empty(),
        "and nothing is owed afterwards: {doc}"
    );
}

/// **A removed worktree's queued condense degrades to commit-only**
/// The checkout is gone — a subagent's worktree deleted when it
/// finished — but the commit is in the family's shared object store, so the
/// C-diff facts still land and an `archive_decision` records the degrade with
/// reason `worktree-missing:commit-only`.
///
/// The two negatives are the point. No checkpoint is archived: a shard written
/// from a surviving member would carry that member's worktree token for work
/// that happened in another one, which the provenance rules forbid. And the entry is
/// settled rather than retried: nothing about a missing directory improves on
/// the third attempt.
#[test]
fn a_removed_worktrees_queued_condense_degrades_to_commit_only_and_records_completeness() {
    let f = family();
    let commit = plant_member_commit(&f);
    let queued = common::capture_queue_dir(&f.admin, &f.cache).join(format!("{commit}.json"));

    // The worktree goes; its administrative entry (and its queue) stay, which
    // is exactly what `rm -rf` on a checkout leaves behind.
    std::fs::remove_dir_all(&f.member).unwrap();
    assert!(
        queued.is_file(),
        "the queued condense outlived the checkout"
    );

    let doc = doctor_json(&f, &f.main, true);
    assert!(
        !queued.exists(),
        "the entry was settled, not retried: {doc}"
    );
    assert!(
        !queued.with_extension("error").exists(),
        "and settled cleanly — a missing checkout is not a failure to mark"
    );

    // The degrade is a row, not a silence.
    assert_eq!(
        main_projection_i64(
            &f,
            "SELECT COUNT(*) FROM archive_decision \
             WHERE reason = 'worktree-missing:commit-only' AND archived = 0",
        ),
        1,
        "the commit-only decision is recorded in the family's projection"
    );
    assert_eq!(
        main_projection_i64(
            &f,
            "SELECT COUNT(*) FROM archive_decision WHERE archived = 1"
        ),
        0,
        "no checkpoint is archived from a surviving member"
    );
    // Commit-only means the commit's own facts DID land: the object store is
    // what worktrees share, so the diff survives the tree.
    assert!(
        main_projection_i64(&f, "SELECT COUNT(*) FROM file_revision") > 0,
        "the commit's file revisions were captured through a surviving member"
    );

    // And the family says the member is prunable — reported, never pruned.
    assert_eq!(doc["prunable_worktrees"]["prunable"], 1, "{doc}");
    assert_eq!(doc["prunable_worktrees"]["pruned"], false);
    assert_eq!(
        doc["prunable_worktrees"]["found"][0]["name"], "member",
        "{doc}"
    );
    assert!(
        f.admin.is_dir(),
        "doctor lists a prunable member; it never removes one"
    );
}

/// The push queue's location is reported where a developer will look for it,
/// and a queue an older build left under a member's gitdir is adopted by the
/// safe `--fix` rather than left stranded.
#[test]
fn doctor_reports_the_push_queue_location_and_adopts_a_legacy_one() {
    let f = family();
    let legacy = f.admin.join("rift-push-queue");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(
        legacy.join("queue.jsonl"),
        "{\"schema_version\":1,\"ref_name\":\"refs/rift/chain/v1\",\
          \"chain_seq\":1,\"enqueued_at_ms\":1700000000000}\n",
    )
    .unwrap();

    let doc = doctor_json(&f, &f.main, false);
    let dir = doc["push_queue"]["queue_dir"].as_str().unwrap();
    // Component-wise, not textual: the report carries the canonical spelling,
    // which on Windows is verbatim and `\`-joined.
    assert!(
        std::path::Path::new(dir).ends_with(std::path::Path::new(".git").join("rift-push-queue")),
        "the queue is named under the common dir: {dir}"
    );
    // A report string is for a person: the verbatim prefix the canonical
    // spelling carries on Windows is display noise, and the report drops it.
    assert!(
        !dir.starts_with(r"\\?\"),
        "the report spells the queue plainly: {dir}"
    );
    assert_eq!(
        doc["push_queue"]["legacy_dirs"].as_array().unwrap().len(),
        1,
        "the stranded per-worktree queue is reported: {doc}"
    );
    assert_eq!(doc["push_queue"]["pending"], 0, "and not yet moved: {doc}");
    assert!(legacy.is_dir());

    let doc = doctor_json(&f, &f.main, true);
    assert_eq!(doc["push_queue"]["adopted"], 1, "{doc}");
    assert_eq!(doc["push_queue"]["pending"], 1, "{doc}");
    assert!(
        doc["push_queue"]["legacy_dirs"]
            .as_array()
            .unwrap()
            .is_empty(),
        "adopted once: nothing is left per worktree: {doc}"
    );
    assert!(!legacy.exists());
}
