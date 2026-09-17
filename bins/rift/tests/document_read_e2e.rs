//! End to end, through the shipped binary: what an agent that opens a
//! document actually gets back.
//!
//! Nothing here is a stand-in. A real `.docx` from the converter's own corpus
//! is written into a throwaway repository, the hook is driven the way a client
//! drives one — one process, the payload on stdin, the decision read off
//! stdout — and the answer is checked against the file the family really
//! converted. The walk exists because the unit tests can prove what the gate
//! decides and not that it is WIRED: that the response leaves the binary in
//! the field the pinned channel allows, that the sidecar it names is a file an
//! agent can read, and that the conversion happened in a child process — the
//! binary under test, re-spawned on its hidden route — that this test did not
//! stub.
//!
//! Every walk here needs the binary to HAVE the seam — `--no-default-features`
//! ships a `rift` that converts nothing and answers no document read, exactly
//! as its stub promises — so the suite is the `doc` feature's and says so,
//! rather than failing a build that never promised a conversion. It runs in
//! every build that carries the seam: the default build, the release build
//! and every configuration CI tests.

#![cfg(feature = "doc")]
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

/// A path as a JSON string literal (quotes included): a Windows path carries
/// backslashes, which a raw `Display` splice turns into invalid escapes.
fn json_str(path: &Path) -> String {
    serde_json::Value::String(path.display().to_string()).to_string()
}

/// A hermetic repository with one commit and a live session, so the hooks
/// below land in a real family with a real journal.
fn seed_repo(tmp: &Path) -> PathBuf {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "doc@example.com"]);
    git(&["config", "user.name", "doc"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&["add", "a.txt"]);
    git(&["commit", "-q", "-m", "seed"]);
    let sessions = repo.join(".git").join("rift-sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("doc-1.json"),
        r#"{"schema":1,"session_id":"doc-1","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":true,"compaction_boundary":null,"updated_at_ms":1783000000000,"native_id_map":{}}"#,
    )
    .unwrap();
    repo
}

/// One document out of the converter's committed corpus, copied in beside the
/// tests that use it so a walk names a path in its own package.
fn plant(repo: &Path, name: &str) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    let target = repo.join(name);
    std::fs::copy(&source, &target).unwrap();
    target
}

/// Drive one real agent hook the way the family sends it and keep its stdout.
fn hook(repo: &Path, verb: &str, payload: &str, cache: &Path) -> serde_json::Value {
    hook_as(repo, "claude-code", verb, payload, cache)
}

/// The same, for a family that names its session in Rift's own envelope
/// rather than in a field of its native payload.
fn hook_as(repo: &Path, agent: &str, verb: &str, payload: &str, cache: &Path) -> serde_json::Value {
    let mut child = rift_cmd(repo)
        .args(["hooks", agent, verb])
        .env("RIFT_CACHE_DIR", cache)
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("RIFT_CAPTURE_SYNC", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "a hook never fails the agent: {agent} {verb}"
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.trim().is_empty() {
        return serde_json::Value::Null;
    }
    serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("one object on stdout: {text} {e}"))
}

/// A `Read` of one path, with nothing else in the call.
fn read_of(path: &Path) -> String {
    format!(
        r#"{{"session_id":"doc-1","tool_name":"Read","tool_input":{{"file_path":{}}}}}"#,
        json_str(path)
    )
}

/// **A document read is answered with the Markdown the family holds.**
///
/// The whole chain, with nothing stubbed: the shipped binary decides, its
/// re-spawned child parses, the family store keeps the answer, this member
/// gets a view of it, and the response carries the path to that view in the
/// one field the pinned channel allows to be amended — and not one byte of
/// the document.
#[test]
fn a_document_read_through_the_real_binary_is_answered_with_the_real_sidecar() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let document = plant(&repo, "spec.docx");

    let answer = hook(&repo, "PreToolUse", &read_of(&document), &cache);
    let amended = answer
        .pointer("/hookSpecificOutput/updatedInput/file_path")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("the read is pointed at the sidecar: {answer}"));
    assert_eq!(
        answer.pointer("/hookSpecificOutput/permissionDecision"),
        Some(&serde_json::Value::from("allow")),
        "a substitution declines nothing: {answer}"
    );
    assert_eq!(
        answer
            .pointer("/hookSpecificOutput/updatedInput")
            .and_then(serde_json::Value::as_object)
            .map(serde_json::Map::len),
        Some(1),
        "the one amended field is the path, and the call carried only that: {answer}"
    );

    // What the agent is pointed at is a file inside its own checkout, and it
    // is the document as Markdown.
    let sidecar = PathBuf::from(amended);
    // Canonical, because the gate resolves every path it names against the
    // checkout's canonical root and a temporary directory on this platform is
    // reached through a symlink.
    let here = repo.canonicalize().unwrap();
    // The served path is derived from the binary's cwd and is never verbatim,
    // so it is canonicalised too before the component comparison.
    let sidecar = sidecar
        .canonicalize()
        .expect("the sidecar the agent is pointed at exists");
    assert!(
        sidecar.starts_with(here.join(".rift").join("state").join("doc")),
        "{amended}"
    );
    let markdown = std::fs::read_to_string(&sidecar).unwrap();
    assert!(
        markdown.contains("Reading a document"),
        "the sidecar is the converted document: {markdown}"
    );
    assert!(
        !answer.to_string().contains("Reading a document"),
        "and a hook response carries a path, never the bytes at it: {answer}"
    );

    // The answer is the FAMILY's, in the common directory every member of the
    // repository shares — not a per-checkout cache.
    let key = sidecar.file_stem().unwrap().to_str().unwrap();
    let family = here
        .join(".git")
        .join("rift-family")
        .join("doc")
        .join(format!("{key}.md"));
    assert!(family.is_file(), "{}", family.display());

    // A scanned PDF converts to a report that it is page images, which is
    // worse than the page the agent asked for: the read is left alone, and
    // the family records the decision so nothing parses it twice.
    let scanned = plant(&repo, "scanned.pdf");
    let left_alone = hook(&repo, "PreToolUse", &read_of(&scanned), &cache);
    assert!(
        left_alone
            .pointer("/hookSpecificOutput/updatedInput")
            .is_none(),
        "a page image is not text: {left_alone}"
    );
    let decided: Vec<_> = std::fs::read_dir(here.join(".git").join("rift-family").join("doc"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "skip"))
        .collect();
    assert_eq!(decided.len(), 1, "the decision is the family's");
}

/// **A substitution that did not happen, through the real binary.**
///
/// The read's result comes back naming the document rather than the sidecar,
/// which is what a sibling hook's own amendment looks like from here. Rift
/// records it and stops amending this window: the next document read is
/// declined once, with the sidecar named in the reason the model reads, and
/// the repeat runs.
#[test]
fn a_dropped_substitution_turns_the_window_over_to_the_decline() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let document = plant(&repo, "spec.docx");

    let amended = hook(&repo, "PreToolUse", &read_of(&document), &cache);
    assert!(
        amended
            .pointer("/hookSpecificOutput/updatedInput/file_path")
            .is_some(),
        "{amended}"
    );

    // The result names the document: the amendment never reached the tool.
    let after = format!(
        r#"{{"session_id":"doc-1","tool_name":"Read","tool_input":{{"file_path":{}}},"tool_response":{{"filePath":{}}}}}"#,
        json_str(&document),
        json_str(&document)
    );
    hook(&repo, "PostToolUse", &after, &cache);

    let declined = hook(&repo, "PreToolUse", &read_of(&document), &cache);
    assert_eq!(
        declined.pointer("/hookSpecificOutput/permissionDecision"),
        Some(&serde_json::Value::from("deny")),
        "the window is answered by the decline now: {declined}"
    );
    let reason = declined
        .pointer("/hookSpecificOutput/permissionDecisionReason")
        .and_then(serde_json::Value::as_str)
        .unwrap();
    assert!(
        reason.contains(".rift/state/doc/"),
        "the reason names where to read instead: {reason}"
    );
    assert!(
        !reason.contains("Reading a document"),
        "and never the document's own words: {reason}"
    );
    assert!(
        declined
            .pointer("/hookSpecificOutput/updatedInput")
            .is_none(),
        "a decline never carries an amendment: {declined}"
    );

    // One decline per sidecar: the repeated call is the escape hatch the
    // sentence itself promises.
    let repeated = hook(&repo, "PreToolUse", &read_of(&document), &cache);
    assert!(
        repeated.pointer("/hookSpecificOutput/permissionDecision")
            != Some(&serde_json::Value::from("deny")),
        "the repeat runs: {repeated}"
    );
}

/// **A Codex shell read of a document, through the real binary.**
///
/// This family's calls cannot be amended, so the whole of what it can be
/// offered is the sentence: declined once, naming a sidecar that really
/// exists, on the envelope its own pinned artifact spells — and the repeat
/// runs, which is what the sentence itself promises.
///
/// The second half of the walk is the one a unit test cannot make: the
/// declined call must leave this window claiming nothing. A `cat` of a text
/// document is a read of lines as well as a read of a document, and if the
/// declined call were also recorded as lines delivered, the very next
/// identical command would be answered as a repeat of a read that never
/// happened.
#[test]
fn a_codex_cat_of_a_document_is_declined_once_and_claims_no_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let here = repo.canonicalize().unwrap();
    let document = here.join("rows.csv");
    let rows: String = (1..=60).fold(String::from("name,value\n"), |mut rows, n| {
        use std::fmt::Write as _;
        let _ = writeln!(rows, "row {n},{n}");
        rows
    });
    std::fs::write(&document, rows).unwrap();
    let cat = |turn: u32| {
        format!(
            r#"{{"schema":1,"session":"doc-1","turn":{turn},"payload":{{"tool_name":"Bash","tool_input":{{"command":"cat {}"}}}}}}"#,
            common::shell_spelling(&document)
        )
    };

    let declined = hook_as(&repo, "codex", "PreToolUse", &cat(1), &cache);
    assert_eq!(
        declined.pointer("/hookSpecificOutput/permissionDecision"),
        Some(&serde_json::Value::from("deny")),
        "the one channel this family has is the sentence: {declined}"
    );
    let reason = declined
        .pointer("/hookSpecificOutput/permissionDecisionReason")
        .and_then(serde_json::Value::as_str)
        .unwrap();
    assert!(
        reason.contains(".rift/state/doc/"),
        "the reason names where to read instead: {reason}"
    );
    assert!(
        !reason.contains("row 42"),
        "and never the document's own rows: {reason}"
    );
    assert!(
        declined
            .pointer("/hookSpecificOutput/updatedInput")
            .is_none(),
        "a decline never carries an amendment: {declined}"
    );

    // The sidecar the sentence names is a file the agent can read, and it is
    // the document as Markdown.
    let named = reason
        .split_whitespace()
        .find(|word| word.starts_with(".rift/state/doc/"))
        .expect("the sentence names the view in full");
    let sidecar = here.join(named.trim_end_matches(['.', ',']));
    assert!(
        std::fs::read_to_string(&sidecar)
            .unwrap_or_default()
            .contains("row 42"),
        "{}",
        sidecar.display()
    );

    // The repeat runs — untouched, and not as a repeat of a read that never
    // happened: nothing was declined a second time and nothing was replaced.
    let repeat = hook_as(&repo, "codex", "PreToolUse", &cat(2), &cache);
    assert!(
        repeat.pointer("/hookSpecificOutput/permissionDecision")
            != Some(&serde_json::Value::from("deny")),
        "the repeat is the escape hatch the sentence promises: {repeat}"
    );
    assert!(
        repeat.pointer("/hookSpecificOutput/updatedInput").is_none(),
        "and the declined call left this window holding no lines of the file: {repeat}"
    );
}
