//! End-to-end: **the session verbs and `rift doctor` read the whole repository
//! family, from any member.**
//!
//! Session caches stay per member by design (they are a
//! session's private working state, and the credit discriminator
//! scopes them). That makes every family-wide question a question about a SET
//! of gitdirs, and the readers here asked it of one: `repo.work_tree` plus
//! `list_worktrees`, which from a linked worktree enumerates that worktree and
//! its siblings and omits the main one — the member where nearly every session
//! of a real repository lives. `rift session list` and the resume picker run
//! from a linked worktree therefore could not see main's sessions at all, and
//! `rift doctor` finalized crashed sessions only in the checkout it happened
//! to be run from.
//!
//! Every assertion here is driven through the shipped binary against a real
//! `git worktree add`, from both vantages, because "from a member" is the half
//! that was broken and a fixture that only ever runs in main cannot tell.
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

/// A family: a main work tree and one linked member, each initialized enough
/// for the verbs to run (`.rift/` present, exactly as `rift init` leaves it).
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
    // No hooks: this suite drives the verbs, and a post-commit archive would
    // make the fixture depend on capture succeeding.
    let nohooks = root.join("nohooks");
    std::fs::create_dir_all(&nohooks).unwrap();
    git_in(
        &main,
        &["config", "core.hooksPath", nohooks.to_str().unwrap()],
    );
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
///
/// `RIFT_CAPTURE_SYNC` on every spawn: the capture work a verb defers to a
/// detached worker would otherwise land between two of the same `doctor`
/// run's legs, which is a race a fixture must not have (observed: leg 1 read
/// zero checkpoints and leg 4 read one, in one invocation).
fn rift(f: &Family, cwd: &Path, args: &[&str]) -> (i32, String, String) {
    let out = rift_cmd(cwd)
        .args(args)
        .env("RIFT_CACHE_DIR", &f.cache)
        .env("RIFT_CONFIG_DIR", f.main.join(".rift-conf"))
        .env("RIFT_HEADLESS", "1")
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("RIFT_CAPTURE_SYNC", "1")
        .output()
        .expect("rift runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn json(f: &Family, cwd: &Path, args: &[&str]) -> serde_json::Value {
    let (code, out, err) = rift(f, cwd, args);
    assert_eq!(code, 0, "rift {args:?} failed: {out}{err}");
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("rift {args:?} json: {e}: {out}"))
}

/// A real agent session, started through the shipped hook from `cwd` — which
/// is what stamps the cache's `work_tree` discriminator and files the session
/// row in the family's projection.
fn start_session(f: &Family, cwd: &Path, id: &str) {
    let payload = format!(r#"{{"session_id":"{id}","hook_event_name":"SessionStart"}}"#);
    let mut child = rift_cmd(cwd)
        .args(["hooks", "claude-code", "SessionStart"])
        .env("RIFT_CACHE_DIR", &f.cache)
        .env("RIFT_CONFIG_DIR", f.main.join(".rift-conf"))
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("RIFT_CAPTURE_SYNC", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("rift runs");
    use std::io::Write as _;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "a hook never fails: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The session ids `rift session list` reported, in row order.
fn listed_ids(doc: &serde_json::Value) -> Vec<String> {
    doc["sessions"]
        .as_array()
        .expect("sessions array")
        .iter()
        .map(|r| r["session_id"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// The state `rift session list` gave one session. The value discriminates the
/// SOURCE of the row, which is what makes the journal assertions here real:
/// a row read from a member's journal carries the journal's live phase
/// (`active`), while one recovered from the projection because no journal
/// holds it any more is reported `archived`.
fn listed_state(doc: &serde_json::Value, id: &str) -> String {
    doc["sessions"]
        .as_array()
        .expect("sessions array")
        .iter()
        .find(|r| r["session_id"] == id)
        .unwrap_or_else(|| panic!("{id} is listed: {doc}"))["state"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// **`rift session list` from a member includes main's sessions**, and its
/// enrichment reads the family's one projection — the two halves of a family-wide reader, neither of which is worth
/// anything alone.
#[test]
fn session_list_from_a_member_includes_mains_sessions() {
    let f = family();
    start_session(&f, &f.main, "01MAINSESSION");
    start_session(&f, &f.member, "01MEMBERSESSION");
    assert!(
        common::sessions_dir(&f.main.join(".git"), &f.cache)
            .join("01MAINSESSION.json")
            .is_file(),
        "main's journal entry is in the main member's run state"
    );
    assert!(
        common::sessions_dir(&f.admin, &f.cache)
            .join("01MEMBERSESSION.json")
            .is_file(),
        "the member's is in its own member's run state"
    );

    // The vantage that was broken: from the member, main's journal was never
    // read, because `list_worktrees` does not list the main work tree.
    let doc = json(&f, &f.member, &["--json", "session", "list"]);
    let from_member = listed_ids(&doc);
    assert!(
        from_member.contains(&"01MAINSESSION".to_owned()),
        "main's session is listed from the member: {from_member:?}"
    );
    assert_eq!(
        listed_state(&doc, "01MAINSESSION"),
        "active",
        "and it came from main's JOURNAL, not from the projection's archived \
         tail — which is the half a family-wide reader adds: {doc}"
    );
    assert!(
        from_member.contains(&"01MEMBERSESSION".to_owned()),
        "and so is the member's own: {from_member:?}"
    );

    // …and the reading is symmetric: the same set from either vantage.
    let from_main = listed_ids(&json(&f, &f.main, &["--json", "session", "list"]));
    let mut a = from_member.clone();
    let mut b = from_main.clone();
    a.sort();
    b.sort();
    assert_eq!(
        a, b,
        "one family, one set of sessions, whichever member asks"
    );

    // Each session is listed ONCE from each vantage — the journal is unioned
    // across members, never re-read per worktree — one credit each.
    // The member's own session used to come back TWICE from its own vantage,
    // because `repo.work_tree` and `list_worktrees` both name it and only the
    // projection pass consulted the seen-set.
    for id in ["01MAINSESSION", "01MEMBERSESSION"] {
        assert_eq!(
            from_member.iter().filter(|listed| *listed == id).count(),
            1,
            "{id} is listed exactly once: {from_member:?}"
        );
    }

    // The enrichment half: delete main's journal entry, so the row can only
    // come from the projection. A member reading its OWN per-path projection
    // would find nothing there; reading the family's, it still finds main's
    // session.
    std::fs::remove_file(
        common::sessions_dir(&f.main.join(".git"), &f.cache).join("01MAINSESSION.json"),
    )
    .unwrap();
    let after = listed_ids(&json(&f, &f.member, &["--json", "session", "list"]));
    assert!(
        after.contains(&"01MAINSESSION".to_owned()),
        "the projection-only row comes from the FAMILY store, read from the member: {after:?}"
    );
}

/// **The resume picker lists every member's stopped sessions.** A stopped
/// session is the thing you resume, and until now the picker could not offer
/// the ones in the main work tree to anyone standing in a linked worktree — the
/// exact case this surface exists for.
///
/// Each row is labelled with the member it belongs to, which is the credit
/// discriminator doing its job on a read: a session driven in one worktree is
/// never presented as another's.
#[test]
fn resume_picker_lists_every_members_stopped_sessions() {
    let f = family();
    start_session(&f, &f.main, "01MAINSESSION");
    start_session(&f, &f.member, "01MEMBERSESSION");

    let doc = json(&f, &f.member, &["--json", "session", "resume"]);
    let rows = doc["resumable"].as_array().expect("resumable array");
    let find = |id: &str| {
        rows.iter()
            .find(|r| r["session_id"] == id)
            .unwrap_or_else(|| panic!("{id} is offered from the member: {doc}"))
    };

    let on_main = find("01MAINSESSION");
    let on_member = find("01MEMBERSESSION");
    let names = |v: &serde_json::Value| {
        let p = v["worktree"].as_str().unwrap_or_default().to_owned();
        std::fs::canonicalize(&p).unwrap_or_else(|_| PathBuf::from(p))
    };
    assert_eq!(
        names(on_main),
        std::fs::canonicalize(&f.main).unwrap(),
        "main's session is labelled with main's work tree: {doc}"
    );
    assert_eq!(
        names(on_member),
        std::fs::canonicalize(&f.member).unwrap(),
        "and the member's with the member's: {doc}"
    );
    assert_eq!(
        on_member["branch"], "member",
        "the branch is read from the member's own checkout: {doc}"
    );

    // Symmetric, and the branch labels prove the rows were not merged: from
    // main the same two rows come back, each still naming its own member.
    let from_main = json(&f, &f.main, &["--json", "session", "resume"]);
    let main_rows = from_main["resumable"].as_array().unwrap();
    assert_eq!(
        main_rows.len(),
        2,
        "two members, two stopped sessions, no duplicate row: {from_main}"
    );
    assert_eq!(main_rows.len(), rows.len(), "{from_main}");
    assert_eq!(
        main_rows
            .iter()
            .find(|r| r["session_id"] == "01MEMBERSESSION")
            .map(|r| r["branch"].clone()),
        Some(serde_json::json!("member")),
        "{from_main}"
    );
}

/// Age a member's session cache so the sweep sees it as crashed: a cache is
/// rewritten on every prompt, tool call and turn end, so one still claiming a
/// live phase after a full idle window means the agent is gone.
fn age_session(git_dir: &Path, cache: &Path, id: &str, updated_at_ms: i64) {
    let path = common::read_path(&common::sessions_dir(git_dir, cache), &format!("{id}.json"));
    let mut cache: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    cache["updated_at_ms"] = serde_json::json!(updated_at_ms);
    std::fs::write(&path, serde_json::to_vec(&cache).unwrap()).unwrap();
}

fn session_state(git_dir: &Path, cache: &Path, id: &str) -> String {
    let path = common::read_path(&common::sessions_dir(git_dir, cache), &format!("{id}.json"));
    let cache: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    cache["state"].as_str().unwrap_or_default().to_owned()
}

/// **`rift doctor` sweeps stale sessions in every member.**
///
/// Doctor is the only pass that runs without an agent, which is what makes it
/// the backstop for a crashed session — and it read one gitdir, so the backstop
/// only ever reached the checkout it was run in. A session crashed in a linked
/// worktree needed a doctor run *inside that worktree*, and a worktree nobody
/// opens again is one nobody runs doctor in: exactly the subagent case this
/// program exists for.
#[test]
fn doctor_sweeps_stale_sessions_in_every_member() {
    let f = family();
    start_session(&f, &f.main, "01MAINSESSION");
    start_session(&f, &f.member, "01STALEMEMBER");
    // Something for the condense to anchor to, made in the member — the
    // commit whose work the crashed session was doing.
    std::fs::write(f.member.join("w.txt"), "work\n").unwrap();
    git_in(&f.member, &["add", "w.txt"]);
    git_in(&f.member, &["commit", "-q", "-m", "member work"]);
    age_session(&f.admin, &f.cache, "01STALEMEMBER", 1_700_000_000_000);

    // REPORT, from MAIN: the member's crashed session is a finding here, and
    // main's own live session is not.
    let doc = json(&f, &f.main, &["--json", "doctor"]);
    let stuck: Vec<String> = doc["sessions"]["stuck"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        stuck,
        vec!["01STALEMEMBER".to_owned()],
        "the member's crashed session is a finding from main, and main's live one is not: {doc}"
    );
    assert_eq!(
        session_state(&f.admin, &f.cache, "01STALEMEMBER"),
        "active",
        "a report-only doctor finalizes nothing"
    );
    let named: Vec<&serde_json::Value> = doc["sessions"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["name"] == "member")
        .collect();
    assert_eq!(named.len(), 1, "the member is named as the holder: {doc}");
    assert_eq!(named[0]["stuck"], 1, "{doc}");

    // FIX, still from MAIN: the member's session is finalized in the member,
    // and main's live session is left alone.
    let fixed = json(&f, &f.main, &["--json", "doctor", "--fix"]);
    assert!(
        fixed["sessions"]["members_swept"].as_i64().unwrap_or(0) >= 2,
        "both members were swept: {fixed}"
    );
    assert_ne!(
        session_state(&f.admin, &f.cache, "01STALEMEMBER"),
        "active",
        "the crashed session was swept in the member, from main: {fixed}"
    );
    assert_eq!(
        session_state(&f.main.join(".git"), &f.cache, "01MAINSESSION"),
        "active",
        "a session inside its idle window is untouched: {fixed}"
    );
    let after = json(&f, &f.main, &["--json", "doctor"]);
    assert!(
        after["sessions"]["stuck"].as_array().unwrap().is_empty(),
        "and nothing is stuck afterwards: {after}"
    );
}

/// **A member's legacy per-path projection is listed, never merged, never
/// deleted.** Adoption keyed the family on the ids the MAIN work
/// tree already used, so main's database is the family's and is not an orphan;
/// a member's, minted under the retired path rule, holds facts nothing reads
/// any more and cannot be folded in.
#[test]
fn doctor_lists_a_members_orphan_projection_and_never_deletes_it() {
    let f = family();
    start_session(&f, &f.main, "01MAINSESSION");

    // The database the retired rule would have minted for the member.
    let orphan = rift_store::projection_db_under(
        &f.cache,
        &rift_git::repo_id_for(&std::fs::canonicalize(&f.member).unwrap()).unwrap(),
    );
    std::fs::create_dir_all(orphan.parent().unwrap()).unwrap();
    rift_store::open_capture_projection(orphan.clone()).unwrap();
    assert!(orphan.is_file());

    for vantage in [&f.main, &f.member] {
        let doc = json(&f, vantage, &["--json", "doctor"]);
        let leg = &doc["orphan_projections"];
        assert_eq!(
            leg["orphans"], 1,
            "the member's is the only orphan — main's IS the family's: {doc}"
        );
        assert_eq!(leg["found"][0]["member"], "member", "{doc}");
        assert_eq!(leg["found"][0]["events"], 0, "{doc}");
        assert_eq!(
            leg["found"][0]["path"],
            serde_json::json!(orphan.display().to_string()),
            "{doc}"
        );
        assert_eq!(leg["deleted"], false, "{doc}");
    }

    // …and `--fix` removes nothing: the facts inside are unrecoverable, so the
    // decision stays with the owner (the phantom-gitdir leg's rule).
    let fixed = json(&f, &f.main, &["--json", "doctor", "--fix"]);
    assert_eq!(fixed["orphan_projections"]["orphans"], 1, "{fixed}");
    assert!(orphan.is_file(), "--fix never deletes a projection");
}

/// The memory chain is read the same from any member: the substrate's
/// checkpoints are the family's refs and the recall projection is the family's
/// file. A fact test — the storage was routed through the family; this pins
/// that the readout moved with it, because a member reporting `0 checkpoints`
/// while the family's refs held 134 is the measured symptom the family model
/// exists to remove.
#[test]
fn the_memory_chain_reads_the_same_from_every_member() {
    let f = family();
    start_session(&f, &f.main, "01MAINSESSION");
    std::fs::write(f.main.join("b.txt"), "b\n").unwrap();
    git_in(&f.main, &["add", "b.txt"]);
    git_in(&f.main, &["commit", "-q", "-m", "work"]);
    // The archive, run explicitly (this fixture disables git's hook path so
    // the capture it drives is the one the test asked for).
    let (code, out, err) = rift(
        &f,
        &f.main,
        &[
            "hooks",
            "git",
            "post-commit",
            "--git-dir",
            f.main.join(".git").to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "a hook never fails git: {out}{err}");

    let from_main = json(&f, &f.main, &["--json", "doctor"]);
    let from_member = json(&f, &f.member, &["--json", "doctor"]);
    let checkpoints = from_main["memory_chain"]["checkpoints"].as_i64().unwrap();
    assert!(
        checkpoints > 0,
        "the fixture archived a checkpoint: {from_main}"
    );
    assert_eq!(
        from_member["memory_chain"]["checkpoints"], checkpoints,
        "the member reads the family's checkpoints, not an empty per-path store: {from_member}"
    );
    assert_eq!(
        from_member["memory_chain"]["ingested_reads"], from_main["memory_chain"]["ingested_reads"],
        "and the family's one recall projection: {from_member}"
    );
    assert_eq!(
        from_member["soak"]["projections"], 1,
        "one family, one projection, counted once: {from_member}"
    );
}

/// **A crashed session in a worktree that is GONE is named, and never promised
/// a fix that cannot happen.** This is the program's central case: Claude Code
/// runs subagents in temporary worktrees it deletes when they finish, so the
/// administrative entry (and the journal under it) outlives the checkout. The
/// sweep condenses a session against the tree it was worked in, and there is no
/// tree — so the leg counts these apart and says so, rather than reporting them
/// under a headline that offers `--fix`.
#[test]
fn a_stale_session_in_a_removed_worktree_is_named_stranded_not_promised_a_fix() {
    let f = family();
    start_session(&f, &f.main, "01MAINSESSION");
    start_session(&f, &f.member, "01GONEMEMBER");
    age_session(&f.admin, &f.cache, "01GONEMEMBER", 1_700_000_000_000);
    std::fs::remove_dir_all(&f.member).unwrap();

    let doc = json(&f, &f.main, &["--json", "doctor"]);
    assert_eq!(
        doc["sessions"]["stuck"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_owned())
            .collect::<Vec<_>>(),
        vec!["01GONEMEMBER".to_owned()],
        "the journal outlived the checkout and the session is still a finding: {doc}"
    );
    assert_eq!(
        doc["sessions"]["stranded"], 1,
        "and it is counted apart, because no sweep can reach it: {doc}"
    );
    assert_eq!(doc["prunable_worktrees"]["prunable"], 1, "{doc}");

    // `--fix` sweeps the members that still have a checkout, leaves the gone
    // one alone, and does not pretend otherwise.
    let fixed = json(&f, &f.main, &["--json", "doctor", "--fix"]);
    assert_eq!(
        fixed["sessions"]["members_swept"], 1,
        "only the surviving member was swept: {fixed}"
    );
    assert_eq!(
        fixed["sessions"]["members_unswept"], 0,
        "and the gone one is skipped, not reported unreadable: {fixed}"
    );
    assert_eq!(
        session_state(&f.admin, &f.cache, "01GONEMEMBER"),
        "active",
        "nothing condensed it, and nothing deleted its journal"
    );
    let after = json(&f, &f.main, &["--json", "doctor"]);
    assert_eq!(after["sessions"]["stranded"], 1, "{after}");
}

/// **The vantage's own gitdir is never reported as a neighbour's.** Every
/// family-wide leg has to ask "is this member the one I am standing in?", and
/// getting that wrong reports one queue entry twice — once as "capture queue",
/// once as a "sibling queue" — in two lines that read as two findings.
///
/// This drives the invariant end to end from a shell, where the kernel has
/// already resolved the process cwd so both spellings of the gitdir agree. The
/// case where they do NOT — the palette dispatching against a project
/// registered through a symlink — is pinned in the unit test
/// `main::tests::the_vantage_gitdir_is_spelled_the_way_a_members_is`, which is
/// the half a cwd-based fixture cannot reach.
#[test]
fn the_vantages_own_queue_is_never_reported_as_a_siblings() {
    let f = family();
    start_session(&f, &f.main, "01MAINSESSION");
    // A commit in MAIN with its condense deferred — the shape `post-commit`
    // leaves under the vantage's own gitdir.
    std::fs::write(f.main.join("c.txt"), "c\n").unwrap();
    git_in(&f.main, &["add", "c.txt"]);
    git_in(&f.main, &["commit", "-q", "-m", "main work"]);
    let commit = git_in(&f.main, &["rev-parse", "HEAD"]).trim().to_owned();
    let dir = common::capture_queue_dir(&f.main.join(".git"), &f.cache);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{commit}.json")),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "commit": commit,
            "work_tree": f.main.display().to_string(),
            "session_id": null,
            "agent": null,
            "session_state": null,
            "queued_at_ms": 1_700_000_000_000_i64,
            "attempts": 0,
        }))
        .unwrap(),
    )
    .unwrap();

    let doc = json(&f, &f.main, &["--json", "doctor"]);
    assert_eq!(
        doc["capture_queue"]["pending"], 1,
        "the vantage's own queue is the headline: {doc}"
    );
    assert!(
        doc["capture_queue"]["siblings"]
            .as_array()
            .unwrap()
            .is_empty(),
        "and it is not ALSO a sibling of itself: {doc}"
    );
    // The same question, in the session leg: main's live session is the
    // vantage's, so it is not attributed to a member "elsewhere".
    assert_eq!(doc["sessions"]["live"], 1, "{doc}");
    assert!(
        !doc.to_string().contains("in 'main'"),
        "the vantage is the headline, never an elsewhere line: {doc}"
    );
}
