//! End-to-end: **the `rift worktree` lifecycle verbs, and the one sweep that may delete a cache directory.**
//!
//! Driven through the shipped binary against a real `git worktree add`,
//! because every claim here is about what git and the filesystem actually did:
//!
//! * `list` tells a provisioned member from an unadopted one and a live
//!   checkout from a `prunable` administrative entry;
//! * `status` names one member, what is wired in it, and the worktree
//!   observations recorded there;
//! * `prune` **reports** and executes nothing without `--yes` — the pin on the
//!   rule that a question is never answered with an act;
//! * `rift projects prune --orphan-projections` names the per-path
//!   projections the family mint orphaned and, even with `--yes`, cannot reach
//!   the family's own store.
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

/// Hermetic `git`: no system/global config, a fixed identity, every
/// re-targeting variable scrubbed.
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

/// A family: a main work tree and two linked members, `wired` (provisioned)
/// and `bare` (never adopted).
struct Family {
    _tmp: tempfile::TempDir,
    main: PathBuf,
    wired: PathBuf,
    bare: PathBuf,
    cache: PathBuf,
    home: PathBuf,
}

const POLICY: &str = "schema_version = 1\n[project]\nname = \"fixture\"\n";

fn add_member(main: &Path, root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    git_in(
        main,
        &["worktree", "add", "-q", "-b", name, path.to_str().unwrap()],
    );
    std::fs::create_dir_all(path.join(".rift")).unwrap();
    std::fs::write(path.join(".rift/config.toml"), POLICY).unwrap();
    path
}

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
    // The family is set up on this machine: its projection exists under the
    // cache root every spawn below is handed. Members share it.
    let cache = root.join("cache");
    common::provision_here(&main, &cache);

    let wired = add_member(&main, &root, "wired");
    let bare = add_member(&main, &root, "bare");
    // The provisioned oracle, exactly as the dashboard reads it: the
    // native-integration manifest's presence, in `wired` only.
    std::fs::create_dir_all(wired.join(".rift/state")).unwrap();
    std::fs::write(
        wired.join(".rift/state/native-integrations.json"),
        "{\"schema_version\":1}\n",
    )
    .unwrap();

    Family {
        cache,
        home: root.join("home"),
        _tmp: tmp,
        main,
        wired,
        bare,
    }
}

fn rift(f: &Family, cwd: &Path, args: &[&str]) -> (i32, String, String) {
    let out = rift_cmd(cwd)
        .args(args)
        .env("RIFT_CACHE_DIR", &f.cache)
        .env("RIFT_CONFIG_DIR", f.main.join(".rift-conf"))
        .env("RIFT_CAPTURE_SYNC", "1")
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("RIFT_HEADLESS", "1")
        .env("HOME", &f.home)
        .output()
        .expect("rift runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn rift_json(f: &Family, cwd: &Path, args: &[&str]) -> serde_json::Value {
    let mut line = vec!["--json"];
    line.extend_from_slice(args);
    let (code, out, err) = rift(f, cwd, &line);
    assert_eq!(code, 0, "rift {args:?} exited {code}: {out}{err}");
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("rift {args:?} json: {e}: {out}"))
}

/// One member row out of `rift worktree list --json`, by name.
fn member<'a>(doc: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    doc["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == name)
        .unwrap_or_else(|| panic!("no member {name} in {doc}"))
}

/// ★ `rift worktree list` separates the three states an
/// operator acts on differently — a provisioned member, a linked member with
/// nothing wired in it, and an administrative entry whose checkout is gone.
///
/// The last one is the case the verb exists for. Claude Code deletes the
/// worktrees it runs isolated subagents in, so a family accumulates entries
/// with no checkout in the ordinary course of work, and `prunable` is the only
/// row state that tells an operator so.
#[test]
fn worktree_list_marks_prunable_and_provisioned() {
    let f = family();
    let listed = rift_json(&f, &f.main, &["worktree", "list"]);

    assert_eq!(member(&listed, "wired")["provisioned"], true, "{listed}");
    assert_eq!(member(&listed, "wired")["kind"], "linked", "{listed}");
    assert_eq!(member(&listed, "bare")["provisioned"], false, "{listed}");
    assert_eq!(listed["not_adopted"], 1, "{listed}");
    assert_eq!(listed["prunable"], 0, "{listed}");
    // Main is in the list from every vantage and is never asked whether it was
    // adopted — `rift init` provisions it.
    let main_row = listed["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["kind"] == "main")
        .unwrap_or_else(|| panic!("main is missing: {listed}"));
    assert_eq!(main_row["branch"], "main", "{listed}");

    // The checkout goes; the administrative entry stays. Git's own `prunable`.
    std::fs::remove_dir_all(&f.bare).unwrap();
    let after = rift_json(&f, &f.main, &["worktree", "list"]);
    assert_eq!(member(&after, "bare")["prunable"], true, "{after}");
    assert_eq!(member(&after, "wired")["prunable"], false, "{after}");
    assert_eq!(after["prunable"], 1, "{after}");
    // …and the same answer from a MEMBER's vantage, not just from main.
    let from_member = rift_json(&f, &f.wired, &["worktree", "list"]);
    assert_eq!(member(&from_member, "bare")["prunable"], true);
    assert_eq!(
        from_member["members"].as_array().unwrap().len(),
        listed["members"].as_array().unwrap().len(),
        "a member sees the whole family, main included"
    );
}

/// ★ `rift worktree prune` REPORTS. Without `--yes` it runs
/// nothing at all — the administrative entry it named is still there
/// afterwards — and with `--yes` it runs `git worktree prune` through the
/// argv-safe seam.
///
/// The asymmetry is the point: reporting is free, removing an administrative
/// entry is not, and a verb asked what is prunable must not answer with an act.
#[test]
fn prune_without_yes_only_reports() {
    let f = family();
    std::fs::remove_dir_all(&f.bare).unwrap();
    let admin = f.main.join(".git").join("worktrees").join("bare");
    assert!(
        admin.is_dir(),
        "the administrative entry exists to begin with"
    );

    let reported = rift_json(&f, &f.main, &["worktree", "prune"]);
    assert_eq!(reported["pruned"], false, "{reported}");
    assert_eq!(reported["prunable"].as_array().unwrap().len(), 1);
    assert_eq!(reported["prunable"][0]["name"], "bare", "{reported}");
    assert!(
        admin.is_dir(),
        "reporting removed an administrative entry: {reported}"
    );

    let done = rift_json(&f, &f.main, &["worktree", "prune", "--yes"]);
    assert_eq!(done["pruned"], true, "{done}");
    assert!(!admin.exists(), "--yes did not run git worktree prune");
    // The surviving member is untouched, and the family is one smaller.
    assert!(f.wired.is_dir());
    let listed = rift_json(&f, &f.main, &["worktree", "list"]);
    assert_eq!(listed["prunable"], 0, "{listed}");
    assert!(
        listed["members"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["name"] != "bare"),
        "{listed}"
    );
}

/// `rift worktree status` answers for ONE member — its row,
/// what `rift-agent-install` can verify without writing anything, and the
/// worktree observations recorded there.
#[test]
fn worktree_status_names_the_member_and_what_is_wired_in_it() {
    let f = family();
    // From inside the member (no PATH): the member it is standing in.
    let here = rift_json(&f, &f.wired, &["worktree", "status"]);
    assert_eq!(here["name"], "wired", "{here}");
    assert_eq!(here["provisioned"], true, "{here}");
    assert!(here["observations"].is_array(), "{here}");

    // …and by PATH from main, which must give the same member.
    let there = rift_json(
        &f,
        &f.main,
        &["worktree", "status", f.wired.to_str().unwrap()],
    );
    assert_eq!(there["name"], "wired", "{there}");
    assert_eq!(there["branch"], "wired", "{there}");

    // A member with nothing wired says so rather than inventing a state.
    let bare = rift_json(&f, &f.bare, &["worktree", "status"]);
    assert_eq!(bare["provisioned"], false, "{bare}");
    assert_eq!(bare["wiring"].as_array().unwrap().len(), 0, "{bare}");
}

/// The orphan sweep names the per-path projections the family mint
/// left behind, removes nothing without `--yes`, and **cannot** reach the
/// family's own store.
///
/// The family's directory is excluded by IDENTITY — the legacy derivation is
/// run per member and any path equal to the family's is skipped — so the
/// exclusion is planted here as a real directory under the family's own id and
/// asserted to survive a `--yes` run that deletes its neighbour.
#[test]
fn the_orphan_sweep_spares_the_family_store_it_cannot_see() {
    let f = family();
    // Touch the family store so it exists on disk under the adopted ids.
    let (code, out, err) = rift(&f, &f.main, &["--json", "status"]);
    assert_eq!(code, 0, "{out}{err}");
    let family_id = rift_git::repo_id_for(&f.main).unwrap();
    let family_dir = rift_store::projection_db_under(&f.cache, &family_id)
        .parent()
        .unwrap()
        .to_path_buf();
    assert!(family_dir.is_dir(), "the family store is on disk");

    // The orphan: a per-path projection under the RETIRED derivation for a
    // member, exactly as every member left one before the family mint.
    let legacy = rift_store::capture_repo_id(&f.wired).unwrap();
    let orphan_dir = rift_store::projection_db_under(&f.cache, &legacy)
        .parent()
        .unwrap()
        .to_path_buf();
    assert_ne!(orphan_dir, family_dir, "the two ids must differ");
    std::fs::create_dir_all(&orphan_dir).unwrap();
    std::fs::write(orphan_dir.join("projection.db"), b"not a database").unwrap();

    // Reported, nothing removed.
    let listed = rift_json(&f, &f.main, &["projects", "prune", "--orphan-projections"]);
    assert_eq!(listed["orphans"], 1, "{listed}");
    assert_eq!(listed["deleted"], false, "{listed}");
    assert_eq!(listed["found"][0]["member"], "wired", "{listed}");
    assert!(orphan_dir.is_dir(), "a report removed a directory");

    // Removed on the word, and the family's own store is still there.
    let swept = rift_json(
        &f,
        &f.main,
        &["projects", "prune", "--orphan-projections", "--yes"],
    );
    assert_eq!(swept["removed"], 1, "{swept}");
    assert!(!orphan_dir.exists(), "the orphan survived --yes: {swept}");
    assert!(
        family_dir.is_dir(),
        "THE FAMILY'S OWN STORE WAS SWEPT — the exclusion is by identity and \
         must hold even under --yes"
    );
    // A second run finds nothing: the enumeration is over what is on disk.
    let again = rift_json(&f, &f.main, &["projects", "prune", "--orphan-projections"]);
    assert_eq!(again["orphans"], 0, "{again}");
}

/// One `memd_audit_event` row in the FAMILY's recall ledger — the store the
/// family adopts main's recall id for, written from outside the
/// binary so the assertion is about which path the verb READS, not about which
/// path some other command happened to create.
fn seed_family_recall(f: &Family, verb: &str, at: i64) -> PathBuf {
    let recall_id = rift_store::recall_repo_id(&f.main).unwrap();
    let db = rift_store::recall_db_under(&f.cache, &recall_id);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(rift_store::CanonicalStore::insert_or_ignore(
        &store,
        "memd_audit_event",
        &[
            "audit_id",
            "verb",
            "handle",
            "reason",
            "tokens_returned",
            "tokens_avoided",
            "recorded_at",
            "schema_version",
        ],
        &[
            rift_store::Value::text("a".repeat(64)),
            rift_store::Value::text(verb.to_owned()),
            rift_store::Value::Null,
            rift_store::Value::Null,
            rift_store::Value::Int(120),
            rift_store::Value::Int(900),
            rift_store::Value::Int(at),
            rift_store::Value::Int(1),
        ],
    ))
    .unwrap();
    db
}

/// ★ The family rule in the plane that was routed last: `rift recap` (and its three
/// siblings through `Planes::open`) reads the **family's** recall ledger, so a
/// linked member sees what the family's sessions produced instead of an empty
/// store.
///
/// The ledger is seeded under the family's recall id and NOWHERE else, and the
/// member's own per-path id is asserted to name a different, absent file — so
/// the only way the member can answer is by resolving the family. Before this
/// change it answered `serve_calls: null` over a ledger that was there all
/// along, which is the double identity the family model exists to remove surviving in
/// one plane.
#[test]
fn a_member_reads_the_familys_recall_ledger_not_an_empty_one() {
    let f = family();
    let at = 1_700_000_000_000_i64;
    let family_db = seed_family_recall(&f, "memd.manifest_served", at);

    // The retired per-path derivation for the MEMBER names a different file,
    // and nothing has written it. This is what the verb used to read.
    let member_db =
        rift_store::recall_db_under(&f.cache, &rift_store::recall_repo_id(&f.wired).unwrap());
    assert_ne!(member_db, family_db, "the two id spaces must differ");
    assert!(!member_db.exists(), "the per-path store is the empty one");

    let window = ["--since", "2023-01-01", "--until", "2026-12-31"];
    let mut args = vec!["recap"];
    args.extend_from_slice(&window);
    let from_member = rift_json(&f, &f.wired, &args);
    let serves = from_member["serve_calls"]
        .as_array()
        .unwrap_or_else(|| panic!("the member read no ledger at all: {from_member}"));
    assert_eq!(serves.len(), 1, "{from_member}");
    assert_eq!(serves[0]["calls"], 1, "{from_member}");
    // The verb name is CITED in the document (it is not Rift's own closed
    // vocabulary), so the name itself is read off the human surface, which is
    // where a citation is meant to resolve.
    let (code, human, err) = rift(&f, &f.wired, &args);
    assert_eq!(code, 0, "{human}{err}");
    assert!(human.contains("memd.manifest_served"), "{human}");
    // The number is the mission sentence: the row's `tokens_avoided` reaches
    // the member's report, so savings compound across members rather than
    // stopping at the checkout that produced them.
    assert_eq!(from_member["savings"]["available"], true, "{from_member}");
    assert_eq!(
        from_member["savings"]["tokens_avoided_estimated_upper_bound"], 900,
        "{from_member}"
    );

    // Main answers identically — the point is that the two agree now, where
    // main was right and the member was silently empty.
    let from_main = rift_json(&f, &f.main, &args);
    assert_eq!(
        from_main["serve_calls"], from_member["serve_calls"],
        "a member and the main work tree read ONE ledger"
    );
    // …and the read created nothing: a report never mints a store.
    assert!(!member_db.exists(), "the read created the per-path store");
}

/// The refusal ordering that makes `Planes::open`'s error arm unreachable in
/// the product, asserted rather than assumed.
///
/// All four verbs run under `projection_read`, which resolves the family for
/// the CAPTURE projection first, so a family the recall arm could fail on has
/// already refused the verb — with the same words, from the same resolver.
/// A repository with no `.rift/` is the cheapest vantage that cannot resolve
/// one, and the verb must refuse there rather than report over an empty store.
#[test]
fn a_verb_refuses_before_the_recall_plane_is_ever_resolved() {
    let f = family();
    let outside = f.main.parent().unwrap().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();

    let (code, out, err) = rift(&f, &outside, &["--json", "recap"]);
    assert_ne!(code, 0, "a non-repository must not report: {out}{err}");
    let said = format!("{out}{err}");
    assert!(
        !said.contains("no ledger in this repository"),
        "the recall plane was consulted before the repository was: {said}"
    );
}
