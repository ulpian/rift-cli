//! End-to-end: **every provenance surface names
//! the worktree its answer came from, and says `unknown` when nothing recorded
//! one.**
//!
//! Two claims, driven through the shipped binary against a real
//! `git worktree add`, because both are about what an operator and an agent
//! actually read:
//!
//! * the two **non-hook** writers — `rift session attach`
//!   and `rift import` — record a `session_worktree` observation with their own
//!   basis, so a transcript adopted by hand is provenanced like one a hook saw;
//! * the seven provenance verbs print the label, and print `unknown` — never
//!   `main` — for the data written before worktree provenance existed, which
//!   every existing repository is full of.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

/// A family: a main work tree and one linked member named `member`, each with
/// the policy file `rift init` would find, plus a redirected `HOME` so the
/// agent-default transcript lookup lands inside the fixture.
struct Family {
    _tmp: tempfile::TempDir,
    main: PathBuf,
    member: PathBuf,
    cache: PathBuf,
    home: PathBuf,
}

const POLICY: &str = "schema_version = 1\n\
     [project]\nname = \"fixture\"\n\
     [paths]\nsensitive_paths = [\"secrets/**\"]\n";

fn family() -> Family {
    let tmp = tempfile::tempdir().unwrap();
    // Named with an underscore: Claude Code folds `_` in a project path like
    // any other non-alphanumeric character, and a temporary directory that
    // happens to carry none would hide a fixture that folds it differently.
    let root = tmp.path().join("fixture_root");
    let main = root.join("main");
    std::fs::create_dir_all(&main).unwrap();
    git_in(&main, &["init", "-q", "-b", "main"]);
    std::fs::write(main.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    git_in(&main, &["add", "a.txt"]);
    git_in(&main, &["commit", "-q", "-m", "seed"]);
    std::fs::create_dir_all(main.join(".rift")).unwrap();
    std::fs::write(main.join(".rift/config.toml"), POLICY).unwrap();
    // The family is set up on this machine: its projection exists under the
    // cache root every spawn below is handed. The member shares it.
    let cache = root.join("cache");
    common::provision_here(&main, &cache);

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
        cache,
        home: root.join("home"),
        _tmp: tmp,
        main,
        member,
    }
}

/// Run `rift` from `cwd` with the family's own cache root and a redirected
/// `HOME`. `RIFT_CAPTURE_SYNC=1` on every spawn: the post-commit worker is
/// detached otherwise, and a test that raced it would be flaky about facts it
/// is asserting.
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

/// `rift --json …`, parsed. Fails loudly with both streams on a non-zero exit.
fn rift_json(f: &Family, cwd: &Path, args: &[&str]) -> serde_json::Value {
    let mut line = vec!["--json"];
    line.extend_from_slice(args);
    let (code, out, err) = rift(f, cwd, &line);
    assert_eq!(code, 0, "rift {args:?} exited {code}: {out}{err}");
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("rift {args:?} json: {e}: {out}"))
}

/// claude-code's per-repository transcript directory under the fixture's
/// `HOME`, folded by the shared rule the binary's own lookup mirrors.
fn transcript_dir(f: &Family, work_tree: &Path) -> PathBuf {
    common::claude_code_project_dir(&f.home, work_tree)
}

/// Write one minimal native transcript for `session` under `dir`.
fn plant_transcript(dir: &Path, session: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(format!("{session}.jsonl"));
    std::fs::write(
        &path,
        concat!(r#"{"type":"user","message":"out of band work"}"#, "\n"),
    )
    .unwrap();
    path
}

/// Every `session_worktree` row in the FAMILY's projection, as
/// `(session_id, worktree_name, basis)`.
fn observations(f: &Family) -> Vec<(String, String, String)> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        // The family adopts the main work tree's ids, so main's
        // own id names the one projection every member writes to.
        let repo_id = rift_git::repo_id_for(&f.main).unwrap();
        let db = rift_store::projection_db_under(&f.cache, &repo_id);
        assert!(db.exists(), "no family projection at {}", db.display());
        let store = rift_store::SqliteStore::open_read_only(&db).unwrap();
        rift_store::CanonicalStore::query(
            &store,
            "SELECT session_id, worktree_name, basis FROM session_worktree \
             ORDER BY first_observed_at, observation_id",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| {
            (
                r.get_text(0).unwrap(),
                r.get_text(1).unwrap(),
                r.get_text(2).unwrap(),
            )
        })
        .collect()
    })
}

/// ★ Six provenance bases are defined; the agent hook decides between three of
/// them. These are the two a **human** decides, and they were the gap this file
/// closes.
///
/// Both verbs run in a LINKED MEMBER, and both must record that member — the
/// checkout the operator adopted the transcript from. Nothing is inferred about
/// where the agent originally worked: that would be a claim, and the honest
/// fact is that a human filed it here.
///
/// The `import` arm also proves the idempotence the relation is built for: the
/// observation is content-addressed over the observed state, so a second
/// import over an unchanged transcript store adds no row.
#[test]
fn attach_and_import_record_the_worktree_they_ran_in() {
    let f = family();

    // `attach` — an out-of-band session found at the agent's default location
    // for the MEMBER's work tree.
    let attach_dir = transcript_dir(&f, &f.member);
    plant_transcript(&attach_dir, "oob-1");
    let attached = rift_json(
        &f,
        &f.member,
        &["session", "attach", "oob-1", "--agent", "claude-code"],
    );
    assert!(
        attached["checkpoint_id"].is_string(),
        "attach archived: {attached}"
    );

    let rows = observations(&f);
    assert_eq!(rows.len(), 1, "one observation for one attach: {rows:?}");
    assert_eq!(rows[0].1, "member", "the member it was attached from");
    assert_eq!(rows[0].2, "attach", "and the basis says which verb");

    // `import` — a bulk read of a second session from an explicit source, run
    // from the same member.
    let import_dir = f.main.parent().unwrap().join("store");
    plant_transcript(&import_dir, "imp-1");
    let imported = rift_json(
        &f,
        &f.member,
        &[
            "import",
            "claude-code",
            "--path",
            import_dir.to_str().unwrap(),
        ],
    );
    assert_eq!(imported["imported"], 1, "{imported}");

    let rows = observations(&f);
    assert_eq!(rows.len(), 2, "attach and import, one row each: {rows:?}");
    let bases: Vec<&str> = rows.iter().map(|r| r.2.as_str()).collect();
    assert!(bases.contains(&"import"), "{rows:?}");
    assert!(
        rows.iter().all(|r| r.1 == "member"),
        "both ran in the member: {rows:?}"
    );

    // Re-import, unchanged: the id is the state's own, so nothing is added.
    let again = rift_json(
        &f,
        &f.member,
        &[
            "import",
            "claude-code",
            "--path",
            import_dir.to_str().unwrap(),
        ],
    );
    assert_eq!(again["already_imported"], 1, "{again}");
    assert_eq!(
        observations(&f).len(),
        2,
        "a repeated observation of the same state is not a second row"
    );
}

/// The family's projection, opened for writing so a test can plant the shape
/// every repository built before worktree provenance existed is in.
fn family_projection(f: &Family) -> (rift_store::SqliteStore, tokio::runtime::Runtime) {
    let repo_id = rift_git::repo_id_for(&f.main).unwrap();
    let db = rift_store::projection_db_under(&f.cache, &repo_id);
    let store = rift_store::open_capture_projection(&db).expect("open the family projection");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    (store, rt)
}

/// A 64-char hex id made of one repeated nibble.
fn hex_id(c: char) -> String {
    std::iter::repeat_n(c, 64).collect()
}

/// Plant a **pre-provenance** checkpoint and session: rows in every relation
/// the verbs read, and NO row in either v14 provenance relation. This is the
/// shape back-filling is forbidden for, and therefore the one every `unknown` in
/// this test is an honest answer about.
fn plant_without_worktree_provenance(f: &Family, at: i64) -> (String, String) {
    let (store, rt) = family_projection(f);
    let checkpoint = hex_id('c');
    let session = hex_id('5');
    let revision = hex_id('7');
    rt.block_on(async {
        plant_the_checkpoint(&store, &checkpoint, at).await;
        plant_a_session_in_it(&store, &f.main, &checkpoint, &session, at).await;
        plant_a_revision_attributed_to_it(&store, &checkpoint, &revision).await;
    });
    (checkpoint, session)
}

/// The checkpoint row, with a fixed merkle root and no predecessor.
async fn plant_the_checkpoint(store: &rift_store::SqliteStore, checkpoint: &str, at: i64) {
    use rift_core::Sha256Id;
    rift_store::record_checkpoint(
        store,
        &rift_store::CheckpointRecord {
            checkpoint_id: Sha256Id::parse_hex(checkpoint).unwrap(),
            strategy: "manual-commit".to_owned(),
            created_at: at,
            merkle_root: Sha256Id::parse_hex(&hex_id('9')).unwrap(),
            prev: None,
            schema_version: 1,
        },
    )
    .await
    .unwrap();
}

/// A session bound to the family, its membership in the checkpoint, and one
/// captured event of it.
async fn plant_a_session_in_it(
    store: &rift_store::SqliteStore,
    main: &Path,
    checkpoint: &str,
    session: &str,
    at: i64,
) {
    rift_store::record_session(
        store,
        &rift_store::SessionRow {
            session_id: session.to_owned(),
            agent: "claude-code".to_owned(),
            model_primary: "claude-sonnet-4-5".to_owned(),
            repo_id: rift_git::repo_id_for(main).unwrap().as_str().to_owned(),
            started_at: at,
            schema_version: 1,
        },
    )
    .await
    .unwrap();
    rift_store::record_session_in_checkpoint(
        store,
        &rift_store::SessionInCheckpointRow {
            checkpoint_id: checkpoint.to_owned(),
            session_id: session.to_owned(),
            ordinal: 0,
            schema_version: 1,
        },
    )
    .await
    .unwrap();
    rift_store::record_captured_event(
        store,
        &rift_store::CapturedEventRow {
            idempotency_key: hex_id('e'),
            session_id: session.to_owned(),
            turn_ordinal: 0,
            event_ordinal: 0,
            kind: "user_prompt".to_owned(),
            trust: "trusted".to_owned(),
            payload_sha256: hex_id('a'),
            occurred_at_ms: at,
            schema_version: 1,
        },
    )
    .await
    .unwrap();
}

/// A file revision and the checkpoint's attribution over it, so `blame`-shaped
/// readers have a row to join.
async fn plant_a_revision_attributed_to_it(
    store: &rift_store::SqliteStore,
    checkpoint: &str,
    revision: &str,
) {
    rift_store::record_file_revision(
        store,
        &rift_store::FileRevisionRow {
            file_revision_id: revision.to_owned(),
            path: "f.rs".to_owned(),
            blob_oid: hex_id('b'),
            content_sha256: hex_id('d'),
            schema_version: 1,
        },
    )
    .await
    .unwrap();
    rift_store::record_attribution(
        store,
        &rift_store::AttributionRow {
            checkpoint_id: checkpoint.to_owned(),
            file_revision_id: revision.to_owned(),
            agent_added: 2,
            human_added: 1,
            human_modified_agent: 0,
            agent_removed_by_human: 0,
            confidence_milli: 850,
            method_version: "v1".to_owned(),
            supersedes_pk: None,
            schema_version: 1,
        },
    )
    .await
    .unwrap();
}

/// ★ The RENDERING clause, at every surface it covers: the seven
/// provenance verbs print the worktree they came from, and print `unknown` —
/// **never `main`** — for data no observation and no shard member covers.
///
/// The two arms are one fixture, so neither can pass by the other's data being
/// absent: a session attached in the LINKED member (which the capture path
/// labels) beside a checkpoint and session planted in the pre-provenance shape
/// (rows in every relation the verbs read, and none in either v14 one). Every
/// assertion is against the HUMAN output, because that clause is about what an
/// operator reads; the `--json` twin is asserted beside it so the two cannot
/// drift.
///
/// `main` is asserted ABSENT rather than merely unequal: the failure this
/// guards is a reader that defaults an absent label to the main work tree,
/// which would print exactly the right-looking word.
#[test]
fn every_provenance_verb_carries_worktree_or_unknown() {
    let f = family();

    let named_checkpoint = attach_and_claim_in_the_member(&f);
    let named_session = "oob-1";

    // The UNKNOWN arm, planted in the same projection.
    let (old_checkpoint, old_session) = plant_without_worktree_provenance(&f, 1_700_000_000_000);

    checkpoint_verbs_label_both_arms(&f, &named_checkpoint, &old_checkpoint);
    blame_and_why_label_the_claimed_checkpoint(&f);
    window_verbs_spread_over_both_arms(&f);
    session_verbs_label_both_arms(&f, named_session, &old_session);
}

/// The NAMED arm: a real archive from the linked member, and a commit in the
/// member that CLAIMS it, so `blame`/`why` resolve. Returns the checkpoint id.
fn attach_and_claim_in_the_member(f: &Family) -> String {
    plant_transcript(&transcript_dir(f, &f.member), "oob-1");
    let attached = rift_json(
        f,
        &f.member,
        &["session", "attach", "oob-1", "--agent", "claude-code"],
    );
    let named_checkpoint = attached["checkpoint_id"].as_str().unwrap().to_owned();

    std::fs::write(f.member.join("f.rs"), "one\ntwo\nthree\n").unwrap();
    git_in(&f.member, &["add", "f.rs"]);
    git_in(
        &f.member,
        &[
            "commit",
            "-q",
            "-m",
            &format!("work\n\nRift-Checkpoint: {named_checkpoint}"),
        ],
    );
    named_checkpoint
}

/// Verbs 1-2, `rift checkpoint explain` and `rift checkpoint show`, both arms
/// wherever each verb can reach them. `explain` resolves its target through the
/// SHARD catalogue, and every shard this capture path writes carries the
/// member, so its unknown arm is unreachable from a live archive and is pinned
/// instead where the shape exists: the frozen `explain.golden` (built from
/// hand-made memberless archives) and
/// `the_explain_header_names_the_member_or_says_it_is_unknown`. Stated here
/// rather than left as a gap in the table.
fn checkpoint_verbs_label_both_arms(f: &Family, named_checkpoint: &str, old_checkpoint: &str) {
    for verb in [vec!["checkpoint", "explain"], vec!["checkpoint", "show"]] {
        let mut named = verb.clone();
        named.push(named_checkpoint);
        let (code, out, err) = rift(f, &f.member, &named);
        assert_eq!(code, 0, "{verb:?}: {out}{err}");
        assert!(out.contains("archived_in: member"), "{verb:?}: {out}");
    }
    let (code, out, err) = rift(f, &f.member, &["checkpoint", "show", old_checkpoint]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("archived_in: unknown"), "{out}");
    assert!(!out.contains("archived_in: main"), "{out}");
}

/// Verbs 3-4, `rift blame` and `rift why` — the checkpoint the commit claims.
fn blame_and_why_label_the_claimed_checkpoint(f: &Family) {
    for verb in [vec!["blame", "f.rs"], vec!["why", "f.rs:2"]] {
        let (code, out, err) = rift(f, &f.member, &verb);
        assert_eq!(code, 0, "{verb:?}: {out}{err}");
        assert!(out.contains("archived_in: member"), "{verb:?}: {out}");
    }
}

/// Verbs 5-7, the three window verbs: the spread names BOTH arms at once, which is
/// the shape a real repository is in during the upgrade.
fn window_verbs_spread_over_both_arms(f: &Family) {
    for verb in [
        vec!["dispatch", "--for-agent"],
        vec!["recap"],
        vec!["review"],
    ] {
        let doc = rift_json(f, &f.member, &verb);
        let spread: Vec<&str> = doc["worktrees"]
            .as_array()
            .unwrap_or_else(|| panic!("{verb:?} carries no worktrees: {doc}"))
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        assert!(spread.contains(&"member"), "{verb:?}: {spread:?}");
        assert!(spread.contains(&"unknown"), "{verb:?}: {spread:?}");
        assert!(
            !spread.contains(&"main"),
            "{verb:?} named the main work tree for data no shard placed there: {spread:?}"
        );
    }
    // …and the human twin says the same thing, in the two verbs that render one.
    for verb in [vec!["dispatch"], vec!["recap"]] {
        let (code, out, err) = rift(f, &f.member, &verb);
        assert_eq!(code, 0, "{verb:?}: {out}{err}");
        assert!(
            out.contains("worktrees   member, unknown"),
            "{verb:?}: {out}"
        );
    }
}

/// Verb 8, `rift session list|info|show` — the observation side of D4, with
/// the `--json` twin of the two session shapes so the surfaces cannot drift.
fn session_verbs_label_both_arms(f: &Family, named_session: &str, old_session: &str) {
    let (code, out, err) = rift(f, &f.member, &["session", "list"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("worktree=member"), "{out}");
    assert!(out.contains("worktree=unknown"), "{out}");

    let (code, out, err) = rift(f, &f.member, &["session", "info", named_session]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("worktree: member"), "{out}");
    assert!(out.contains("observed in member"), "{out}");
    assert!(out.contains("basis attach"), "{out}");

    let (code, out, err) = rift(f, &f.member, &["session", "show", old_session]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("worktree: unknown"), "{out}");
    assert!(!out.contains("worktree: main"), "{out}");

    let info = rift_json(f, &f.member, &["session", "info", named_session]);
    assert_eq!(info["worktree"], "member", "{info}");
    assert_eq!(info["worktree_history"][0]["basis"], "attach", "{info}");
    let show = rift_json(f, &f.member, &["session", "show", old_session]);
    assert_eq!(show["worktree"], serde_json::Value::Null, "{show}");
}

/// Drive one real Claude Code hook from `cwd` with `payload` on stdin.
fn claude_hook(f: &Family, cwd: &Path, verb: &str, payload: &serde_json::Value) {
    let mut child = rift_cmd(cwd)
        .args(["hooks", "claude-code", verb])
        .env("RIFT_CACHE_DIR", &f.cache)
        .env("RIFT_CONFIG_DIR", f.main.join(".rift-conf"))
        .env("RIFT_CAPTURE_SYNC", "1")
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("RIFT_HEADLESS", "1")
        .env("HOME", &f.home)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    assert!(
        child.wait().unwrap().success(),
        "a hook never fails the agent"
    );
}

/// ★ An agent sitting in the main checkout that reads a member's file through
/// `cd <member> && sed …` read in the member, and every surface that names a
/// session's checkout says so: the session's current worktree, its history
/// with the basis that decided it, and the member's own observations.
#[test]
fn a_read_after_cd_into_a_member_is_the_members_on_every_surface() {
    let f = family();
    let main = f.main.display().to_string();
    let start = serde_json::json!({
        "session_id": "cd-1", "cwd": main, "hook_event_name": "SessionStart",
    });
    claude_hook(&f, &f.main, "SessionStart", &start);
    let read = serde_json::json!({
        "session_id": "cd-1", "cwd": main, "hook_event_name": "PostToolUse",
        "tool_name": "Bash", "tool_use_id": "t-1",
        "tool_input": { "command": format!("cd {} && sed -n '1,2p' a.txt", f.member.display()) },
        "tool_response": { "stdout": "one\ntwo\n" },
    });
    claude_hook(&f, &f.main, "PostToolUse", &read);

    let info = rift_json(&f, &f.main, &["session", "info", "cd-1"]);
    assert_eq!(info["worktree"], "member", "{info}");
    let history = info["worktree_history"].as_array().unwrap();
    assert_eq!(history.first().unwrap()["name"], "main", "{info}");
    assert_eq!(history.last().unwrap()["basis"], "shell_cd", "{info}");

    let status = rift_json(&f, &f.member, &["worktree", "status"]);
    let bases: Vec<&str> = status["observations"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|o| o["basis"].as_str())
        .collect();
    assert_eq!(bases, ["shell_cd"], "{status}");
}
