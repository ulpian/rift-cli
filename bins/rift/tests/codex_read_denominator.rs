//! End to end, through the shipped binary: **a Codex session's reads reach
//! the savings denominator.**
//!
//! Codex has no read tool. Its context window fills through the shell —
//! `cat`, `head -n N`, `sed -n 'A,Bp'` — and until the ingest learned to read
//! those command lines the capture projection held not one Codex read fact.
//! The consequence was not a missing column but a missing *denominator*: the
//! savings share divides avoided tokens by avoided plus read, so a family that
//! decides and is never observed reading divides by its own numerator, prints
//! 100 %, and is dropped from the honest variant by name. That is the state
//! this walk starts in and the state it leaves.
//!
//! Nothing here is a stand-in on the read side: a real file is committed into
//! a throwaway repository, the hook is driven the way the family drives one —
//! one process, the payload on stdin — and the count is read back out of the
//! projection the binary itself wrote. The decision side is seeded, because
//! what is under test is the denominator, not the levers that fill the
//! numerator.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rift_git::{FamilyState, MintPolicy};
use rift_store::{CanonicalStore, SqliteStore, Value};

mod common;
use common::rift_cmd;

/// What the seeded Codex decision claims to have avoided.
const AVOIDED_CODEX: i64 = 700;

/// A run of the corpus file's lines, 1-based and inclusive at both ends.
fn lines(range: std::ops::RangeInclusive<u32>) -> String {
    let mut out = String::new();
    for n in range {
        out.push_str(&format!("// line {n} of the file under read\n"));
    }
    out
}

/// The file the session reads, long enough that a range is visibly cheaper
/// than the whole of it.
fn corpus() -> String {
    lines(1..=200)
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
}

/// A hermetic repository with the file the session reads, so the hook below
/// lands in a real family with a real projection.
///
/// Deliberately unborn: nothing here needs a commit. The read fact's identity
/// is the working file's bytes, and a committed blob is only the *fallback*
/// the worker reaches for once those bytes have moved on — which is a
/// different walk from this one.
fn seed_repo(tmp: &Path) -> PathBuf {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git init");
    std::fs::write(repo.join("big.rs"), corpus()).unwrap();
    repo
}

/// Drive one real Codex hook the way the family sends it: Rift's own envelope
/// carries the session, the client's payload rides inside it.
fn codex_hook(repo: &Path, cache: &Path, turn: u32, command: &str) {
    let payload = serde_json::json!({
        "schema": 1,
        "session": "codex-1",
        "turn": turn,
        "payload": {
            "tool_name": "Bash",
            "tool_input": { "command": command },
        },
    })
    .to_string();
    let mut child = rift_cmd(repo)
        .args(["hooks", "codex", "PostToolUse"])
        .env("RIFT_CACHE_DIR", cache)
        .env("RIFT_CONFIG_DIR", repo.join(".rift-conf"))
        .env("RIFT_NO_KEYCHAIN", "1")
        // The count is owed to the detached worker; under this variable the
        // caller drains it, which is what lets a test observe the figure.
        .env("RIFT_CAPTURE_SYNC", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
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
    assert!(out.status.success(), "a hook never fails the agent");
}

/// `rift savings --json` from inside the family, with its own cache root.
fn savings(repo: &Path, cache: &Path) -> serde_json::Value {
    let out = rift_cmd(repo)
        .args(["savings", "--json"])
        .env("RIFT_CACHE_DIR", cache)
        .env("RIFT_CONFIG_DIR", repo.join(".rift-conf"))
        .env("RIFT_HEADLESS", "1")
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("RIFT_CAPTURE_SYNC", "1")
        .output()
        .expect("rift runs");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(0), "rift savings failed: {text}");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("rift savings json: {e}: {text}"))
}

/// One Codex decision in the serving relations, so the family has a numerator
/// and can therefore be *excluded* for having no denominator.
fn seed_codex_decision(cache: &Path, family: &impl rift_core::id::FamilyIds) {
    let recall_db = rift_store::recall_db_for_family_under(cache, family);
    std::fs::create_dir_all(recall_db.parent().unwrap()).unwrap();
    let recall = rift_store::open_migrated(&recall_db, &rift_store::recall_migrations()).unwrap();
    let rt = rt();
    rt.block_on(recall.insert(
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
            Value::text("a0"),
            Value::text("memd.read_intercepted"),
            Value::text("h"),
            Value::Null,
            Value::Int(0),
            Value::Int(AVOIDED_CODEX),
            Value::Int(1_000),
            Value::Int(1),
        ],
    ))
    .unwrap();
    rt.block_on(recall.insert(
        "serve_call_event",
        &[
            "call_id",
            "channel",
            "tool_name",
            "terminal_verb",
            "tokens_returned",
            "tokens_avoided",
            "recorded_at",
            "schema_version",
        ],
        &[
            Value::text("c0"),
            Value::text("hook_inject"),
            Value::text("codex:read-gate"),
            Value::text("memd.read_intercepted"),
            Value::Int(0),
            Value::Int(AVOIDED_CODEX),
            Value::Int(1_000),
            Value::Int(1),
        ],
    ))
    .unwrap();
}

/// One family's row out of `by_family`, which is a list ordered by name.
fn family_row<'a>(report: &'a serde_json::Value, family: &str) -> &'a serde_json::Value {
    report["by_family"]
        .as_array()
        .expect("by_family is a list")
        .iter()
        .find(|row| row["family"] == family)
        .unwrap_or_else(|| panic!("no {family} row: {report}"))
}

/// The read facts this family's projection holds, as `(tool_name, tokens)`.
fn read_facts(cache: &Path, family: &impl rift_core::id::FamilyIds) -> Vec<(String, Option<i64>)> {
    let db = rift_store::projection_db_for_family_under(cache, family);
    if !db.exists() {
        return Vec::new();
    }
    let store = SqliteStore::open(&db).unwrap();
    let rows = rt()
        .block_on(store.query(
            "SELECT tc.tool_name, frt.input_tokens FROM tool_call tc \
             JOIN tool_reads_file trf ON trf.tool_call_id = tc.tool_call_id \
             LEFT JOIN file_read_tokens frt ON frt.tool_call_id = tc.tool_call_id \
             ORDER BY tc.started_at",
            &[],
        ))
        .unwrap();
    rows.iter()
        .map(|r| (r.get_text(0).unwrap_or_default(), r.get_i64(1).ok()))
        .collect()
}

/// **A Codex `cat` is a read, and the share says so.**
///
/// Before: the family has decided and never been observed reading, so its
/// denominator is zero, its share is a division by its own numerator, and the
/// honest variant drops it by name. After one real `cat` through the real
/// binary: the read fact is there, priced by the worker from the file's own
/// bytes with the real tokenizer, and the family is in the share rather than
/// beside it.
#[test]
fn codex_reads_enter_the_savings_denominator() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let (_, state) = rift_git::family_of(&repo, MintPolicy::Allow).unwrap();
    let FamilyState::Minted(family) = state else {
        panic!("the fixture repository must mint a family identity");
    };
    seed_codex_decision(&cache, &family);

    // The session runs, and reads through a command the grammar does not
    // spell — which is exactly the state the family was in before this walk
    // existed: hooks captured, and not one read fact among them.
    codex_hook(&repo, &cache, 1, "cat big.rs && touch done");
    assert!(
        read_facts(&cache, &family).is_empty(),
        "a chain with a member that is not a read is not a read at all"
    );

    // BEFORE — a numerator with no denominator.
    let before = savings(&repo, &cache);
    let row = family_row(&before, "codex");
    assert_eq!(row["tokens_avoided"], AVOIDED_CODEX);
    assert_eq!(row["read_tokens"], 0);
    assert_eq!(row["sessions_with_reads"], 0);
    assert_eq!(
        before["excluded"],
        serde_json::json!(["codex"]),
        "a family that decides and never reads is named, not averaged in"
    );

    codex_hook(&repo, &cache, 2, "cat big.rs");

    // The fact landed on the shell tool's own call, which is the existing
    // column that separates a shell read from a read tool's — no new one.
    let facts = read_facts(&cache, &family);
    assert_eq!(facts.len(), 1, "one read, one fact: {facts:?}");
    assert_eq!(facts[0].0, "Bash");
    let counted = facts[0].1.expect("the worker counted the read");
    assert_eq!(
        counted,
        i64::from(rift_tokens::count(&corpus())),
        "the file's own bytes, counted by the real tokenizer"
    );

    // AFTER — the family divides by something it was observed reading.
    let after = savings(&repo, &cache);
    let row = family_row(&after, "codex");
    assert_eq!(row["read_tokens"], counted);
    assert_eq!(row["sessions_with_reads"], 1);
    assert_eq!(
        after["excluded"],
        serde_json::json!([]),
        "with a denominator of its own, codex leaves the excluded list"
    );
    let share = after["share_excluding_undenominated_pct"]
        .as_f64()
        .expect("a share with a real denominator");
    assert!(
        share < 100.0,
        "a real denominator means a share below the division-by-itself 100 %: {share}"
    );
}

/// **A ranged read is priced at its range, a composed one at the range it
/// composes, and an unrecognised one at nothing.**
///
/// `sed -n 'A,Bp'` names its own lines, and those lines are what the
/// denominator gets — pricing the whole file against a twenty-line read would
/// put tokens in it that never reached a context window. A trailing
/// `| head -n N` and a counted `tail` name their lines just as exactly, from
/// the top and from the bottom, and are priced the same way. A command outside
/// the pinned grammar leaves no fact at all: an uncounted read makes the share
/// look larger than it is, which is a shortfall a reader can find, where an
/// invented one is a claim nobody can check.
#[test]
fn a_ranged_codex_read_is_counted_at_its_range_and_a_piped_one_not_at_all() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let (_, state) = rift_git::family_of(&repo, MintPolicy::Allow).unwrap();
    let FamilyState::Minted(family) = state else {
        panic!("the fixture repository must mint a family identity");
    };

    codex_hook(&repo, &cache, 1, "sed -n '40,60p' big.rs");
    let facts = read_facts(&cache, &family);
    assert_eq!(facts.len(), 1, "the ranged read is a read: {facts:?}");
    let ranged = facts[0].1.expect("the worker counted the range");
    assert_eq!(ranged, i64::from(rift_tokens::count(&lines(40..=60))));
    assert!(
        ranged < i64::from(rift_tokens::count(&corpus())),
        "the range costs less than the file it came from"
    );

    // A narrowing pipe names its lines as exactly as a `sed` script does, and
    // is priced at them.
    codex_hook(&repo, &cache, 2, "cat big.rs | head -n 20");
    let facts = read_facts(&cache, &family);
    assert_eq!(facts.len(), 2, "a narrowed read is a read: {facts:?}");
    assert_eq!(
        facts[1].1.expect("the worker counted the range"),
        i64::from(rift_tokens::count(&lines(1..=20))),
        "a narrowed read is priced at the lines it delivered"
    );

    // …and the commands whose lines this layer cannot name add nothing at
    // all: a member that is not a read, a pipe into something that is not one
    // of the two narrowing commands, an unpinned default window, and a
    // counted `tail`, whose line numbers are a function of a length only the
    // step that holds the bytes learns.
    for (turn, command) in [
        (3, "cat big.rs && touch done"),
        (4, "cat big.rs | grep line"),
        (5, "head big.rs"),
        (6, "tail -n 5 big.rs"),
    ] {
        codex_hook(&repo, &cache, turn, command);
        assert_eq!(
            read_facts(&cache, &family).len(),
            2,
            "`{command}` left a fact it cannot justify"
        );
    }
}
