//! End to end, through the shipped binary: the pull the manifest's own hint
//! teaches, answered on a live projection.
//!
//! The claim under test is the whole point of the held-revision channel — an
//! agent that read a file, watched it change, and asks what changed gets the
//! changed lines instead of the file. Nothing here is a fixture of the
//! answer: a real repository, real hooks driven one process per event, a real
//! commit archiving the read facts, and the verb spelled exactly as the hint
//! spells it. What is asserted is the shape and the direction of the saving,
//! never the tempdir's own spelling or a token count that would pin the
//! tokenizer's table.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

mod common;
use common::rift_cmd;

/// Run `git` in `dir` with a fixed identity and no ambient configuration.
///
/// `home` is threaded through because git runs Rift's own installed hooks, and
/// they must land in the same isolated cache and project registry — and drain
/// inline. Without `RIFT_CAPTURE_SYNC` here, git's post-commit hook detaches a
/// worker that holds the capture queue's lock while the test's own explicit,
/// inline post-commit finds nothing to drain: the read facts then reach the
/// shard after the assertions, or not at all. That race is what this argument
/// removes.
fn git(dir: &Path, home: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join("absent-gitconfig"))
        .env("RIFT_CACHE_DIR", home.join("cache"))
        .env("RIFT_CONFIG_DIR", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("RIFT_CAPTURE_SYNC", "1")
        .env("GIT_AUTHOR_NAME", "rift-test")
        // Not address-shaped on purpose: git does not validate this field, and
        // a new address-shaped literal in the tree is a new hit for the
        // redaction false-positive census, which is a real measurement of this
        // repository's own text and not a thing a test fixture should move.
        .env("GIT_AUTHOR_EMAIL", "rift-test")
        .env("GIT_COMMITTER_NAME", "rift-test")
        .env("GIT_COMMITTER_EMAIL", "rift-test")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("git is on PATH");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// One `rift` run from `repo`, with the hermetic environment every spawn in
/// this suite shares. Returns (exit code, stdout).
fn rift(repo: &Path, home: &Path, args: &[&str], stdin: &str) -> (i32, String) {
    let mut child = rift_cmd(repo)
        .args(args)
        .env("RIFT_CACHE_DIR", home.join("cache"))
        .env("RIFT_CONFIG_DIR", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("RIFT_CAPTURE_SYNC", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn rift");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// Twenty small indented blocks: a change inside one has an edge to grow to,
/// and the rest of the file is big enough that not sending it is worth more
/// than the answer costs — which is the claim being measured.
fn source_file(first: &str) -> String {
    let mut out = String::from("use std::io;\n\n");
    for b in 0..20 {
        out.push_str(&format!("fn f{b}() {{\n"));
        for n in 1..=6 {
            let v = if b == 0 && n == 1 {
                first
            } else {
                &n.to_string()
            };
            out.push_str(&format!("    let x{b}_{n} = {v};\n"));
        }
        out.push_str("}\n\n");
    }
    out
}

/// The whole channel, end to end: a session reads a committed file, the file
/// is edited and committed, and the pull the hint teaches serves the changed
/// lines of the file as it stands — in current numbering, with the unchanged
/// runs counted rather than sent, and a saving rather than a cost.
///
/// Two things this cannot pass without, which is why it is an e2e and not a
/// unit test: the revision the caller holds has to be reachable as a git blob
/// (the ingest bridges it), and the CLI has to be answering over the real
/// repository rather than an empty substrate.
#[test]
fn the_pull_verb_serves_hunks_from_the_cli_on_a_live_projection() {
    let tmp = tempfile::tempdir().unwrap();
    // Canonical from the start. A read fact is recorded against the path the
    // hook was handed and stored relative to the repository root the capture
    // side resolved; on this platform a tempdir is reached through a symlink,
    // so an uncanonicalised root and a canonical file path do not share a
    // prefix and the read lands under a path no verb can ask about.
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    let held_hash = seed_and_initialise(&repo, &home);
    let session = "01PULLVERBE2ESESSIONAAAAAA";
    a_session_reads_the_committed_file(&repo, &home, session);
    seal_the_read_then_edit_the_file(&repo, &home);

    let doc = pull_as_the_hint_spells_it(&repo, &home, &held_hash);
    the_changed_lines_are_served(&doc, &held_hash);
    a_hand_edit_is_not_credited_to_the_agent(&doc, session);
    the_answer_is_a_saving(&doc);
}

/// Commit the held revision of `src/a.rs`, then `rift init` and
/// `rift recall init`; returns the held revision's sha256.
fn seed_and_initialise(repo: &Path, home: &Path) -> String {
    git(repo, home, &["init", "-q", "-b", "main"]);
    let held_text = source_file("1");
    std::fs::write(repo.join("src/a.rs"), &held_text).unwrap();
    git(repo, home, &["add", "-A"]);
    git(repo, home, &["commit", "-qm", "seed"]);
    let held_hash = sha256_hex(&held_text);

    assert_eq!(rift(repo, home, &["init"], "").0, 0, "rift init");
    assert_eq!(
        rift(repo, home, &["recall", "init"], "").0,
        0,
        "rift recall init"
    );
    held_hash
}

/// Lowercase hex sha256 of `text`: the revision a caller holds.
fn sha256_hex(text: &str) -> String {
    use core::fmt::Write as _;
    use sha2::{Digest as _, Sha256};
    let d = Sha256::digest(text.as_bytes());
    d.iter().fold(String::with_capacity(64), |mut acc, b| {
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

/// A session reads the committed file, then the turn ends: one process per
/// hook event.
fn a_session_reads_the_committed_file(repo: &Path, home: &Path, session: &str) {
    let read = format!(
        r#"{{"schema":1,"session":"{session}","turn":1,"payload":{{"tool_name":"Read","tool_input":{{"file_path":{}}}}}}}"#,
        serde_json::Value::String(repo.join("src/a.rs").display().to_string())
    );
    for (args, body) in [
        (
            vec!["hooks", "claude-code", "SessionStart"],
            format!(
                r#"{{"session_id":"{session}","hook_event_name":"SessionStart","source":"startup"}}"#
            ),
        ),
        (vec!["hooks", "claude-code", "PostToolUse"], read),
        (
            vec!["hooks", "claude-code", "Stop"],
            format!(r#"{{"session_id":"{session}","hook_event_name":"Stop"}}"#),
        ),
    ] {
        assert_eq!(rift(repo, home, &args, &body).0, 0, "{args:?}");
    }
}

/// A commit seals the read facts into a checkpoint, the file is edited, and a
/// second commit puts the revision the session holds into history — which is
/// the only place a basis it never kept a copy of can come from.
fn seal_the_read_then_edit_the_file(repo: &Path, home: &Path) {
    let git_dir = repo.join(".git");
    let post_commit = vec![
        "hooks",
        "git",
        "post-commit",
        "--git-dir",
        git_dir.to_str().unwrap(),
    ];
    git(repo, home, &["commit", "-q", "--allow-empty", "-m", "seal"]);
    assert_eq!(rift(repo, home, &post_commit, "").0, 0, "post-commit");
    std::fs::write(repo.join("src/a.rs"), source_file("91")).unwrap();
    git(repo, home, &["add", "-A"]);
    git(repo, home, &["commit", "-qm", "edit"]);
    assert_eq!(rift(repo, home, &post_commit, "").0, 0, "post-commit");
}

/// The pull, spelled as the manifest's hint spells it: the path and sixteen
/// hex of the revision the caller holds.
fn pull_as_the_hint_spells_it(repo: &Path, home: &Path, held_hash: &str) -> serde_json::Value {
    let (code, stdout) = rift(
        repo,
        home,
        &[
            "recall",
            "changed-hunks",
            "src/a.rs",
            "--known-hash",
            &held_hash[..16],
            "--json",
        ],
        "",
    );
    assert_eq!(code, 0, "the pull runs: {stdout}");
    serde_json::from_str(&stdout).expect("json")
}

/// The changed lines are served in the numbering a reader sees now, with the
/// rest of the file counted rather than sent.
fn the_changed_lines_are_served(doc: &serde_json::Value, held_hash: &str) {
    let result = &doc["result"];
    assert_eq!(
        result["state"], "exact_included",
        "the changed lines are served: {doc:#}"
    );
    assert_eq!(result["basis_sha256"], held_hash);
    assert_ne!(
        result["current_hash"], held_hash,
        "the file moved, so the current revision is not the held one"
    );

    let content = result["content"].as_str().expect("the changed lines");
    assert!(content.contains("let x0_1 = 91;"), "{content}");
    assert!(content.contains("unchanged lines"), "{content}");
    assert!(!content.contains("let x9_1"), "{content}");
    let hunks = result["hunks"].as_array().expect("hunks");
    assert_eq!(hunks.len(), 1, "one changed place: {hunks:?}");
    assert!(hunks[0]["lo"].as_u64().unwrap() >= 1);
}

/// The story rides with the delta, and it names no one the capture does not
/// credit. What is asserted is the SHAPE and the source of each fact, never a
/// wall-clock instant or a token figure.
fn a_hand_edit_is_not_credited_to_the_agent(doc: &serde_json::Value, session: &str) {
    let p = &doc["result"]["provenance"];
    assert!(p.is_object(), "a delta carries where it came from: {doc:#}");
    // The bracket's `from` end is the checkpoint that recorded the revision
    // this caller holds. There is no `to`: nobody has read the edited file, so
    // no checkpoint recorded its current revision, and the answer says that by
    // leaving the key out rather than by naming a checkpoint that never saw
    // these bytes.
    assert!(
        p["checkpoints"]["from"]
            .as_str()
            .is_some_and(|c| !c.is_empty()),
        "the change is anchored to the checkpoint that recorded the held revision: {p:#}"
    );
    assert!(
        p["checkpoints"].get("to").is_none(),
        "a revision no checkpoint recorded is not attributed to one: {p:#}"
    );
    assert_eq!(
        p["why"].as_str(),
        Some("rift why src/a.rs:4 --json"),
        "the handle to why is the exact command, at the first changed line"
    );
    // The edit was made by hand after the session's turn, so no checkpoint
    // recorded it and no attribution credits an agent with it. The checkpoint
    // that recorded the held revision did not make the change, so its session,
    // agent, time and cost stay out, and the object says the author is unknown
    // rather than naming whoever ran last.
    for key in [
        "session",
        "agent",
        "token_usage",
        "cost_micros",
        "recorded_at_ms",
    ] {
        assert!(
            p.get(key).is_none(),
            "a hand edit is not credited to the session {session}: `{key}` in {p:#}"
        );
    }
    assert_eq!(p["author"].as_str(), Some("unknown"), "{p:#}");
    // The commit half is a git fact: the oid the recall projection mirrored,
    // and the subject this checkout's history carries for it.
    assert_eq!(
        p["commit"]["subject"].as_str(),
        Some("seal"),
        "the commit subject rides as a git fact — the commit whose archive \n         recorded the revision the caller holds: {p:#}"
    );
}

/// The saving, which is the reason the verb exists: what the caller did not
/// have to re-read, minus what it was sent, and it is a saving.
fn the_answer_is_a_saving(doc: &serde_json::Value) {
    let avoided = doc["estimated_tokens_avoided"].as_u64().expect("avoided");
    let returned = doc["estimated_tokens_returned"].as_u64().expect("returned");
    assert!(avoided > 0, "tokens_avoided > 0: {doc:#}");
    assert!(
        avoided > returned,
        "the answer is smaller than the file it replaced: {avoided} vs {returned}"
    );
}
