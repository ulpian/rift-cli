//! Cross-process end-to-end capture for **Codex and Antigravity**.
//!
//! A 2026-08-24 review found that only `claude-code` ever crossed a
//! process boundary in a merge-blocking test: `vogon_e2e.rs` drives one agent,
//! and everything known about the other two supported agents was asserted
//! in-process, where a hook that never reaches `rift hooks <token> <verb>`
//! cannot fail. Two of the three agents this alpha claims to capture had never
//! been captured by the shipped binary in CI.
//!
//! **The scripts are not invented here.** Each verb list is what was pinned
//! as the agent's own published surface, and each payload uses the field
//! names those pins record:
//!
//! * Codex — ten verbs, from `crates/rift-agent-codex/fixtures/`
//!   (`session_id`, `turn_id`, `agent_id`, `agent_type`, `model`,
//!   `permission_mode`, `tool_name`, `tool_input`, `tool_response`,
//!   `tool_use_id`, `reason`). `SessionEnd` is among them *because* the pin
//!   showed Codex publishes it; before that the adapter declared no
//!   `SessionEnd` and a Codex session had an end nothing recorded.
//! * Antigravity — five verbs and the nested `toolCall`/`conversationId`
//!   camelCase shape its golden stream in `adapter_sweep.rs` records.
//!
//! **Hermetic by construction**, the same way `vogon_e2e.rs` is: the repository
//! is a tempdir, `RIFT_CACHE_DIR`/`XDG_DATA_HOME` redirect the projection and
//! data root into it, and the keychain is off. This suite must never be able to
//! write into the repository it runs in — that class already put one fixture
//! session into this repository's immutable checkpoint history.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The `rift` binary this suite drives, **rebuilt from the current source
/// before it is driven**, into its own target directory.
///
/// Both halves are load-bearing and both were paid for by `vogon_e2e.rs`, whose
/// `rift_bin` docstring records the incidents: `CARGO_BIN_EXE_*` only names
/// bins of *this* package, so the binary under test can only be found as a
/// sibling in the same profile directory — and finding it is not enough,
/// because `cargo test -p rift-shadow-hooks` has no reason to relink a binary
/// belonging to `bins/rift`, so a suite that merely locates it can drive a
/// stale one and stay green over a planted defect. Building into the shared
/// `target/` instead fights the outer `--all-features` build over the same
/// fingerprints. So: unconditional build, separate directory.
fn rift_bin() -> PathBuf {
    static BIN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BIN.get_or_init(|| {
        let vogon = PathBuf::from(env!("CARGO_BIN_EXE_vogon"));
        let profile_dir = vogon.parent().expect("vogon has a parent dir");
        let profile = profile_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("debug")
            .to_owned();
        let target = profile_dir
            .parent()
            .expect("a target dir above the profile dir")
            .join("vogon-e2e");
        let mut build = Command::new(env!("CARGO"));
        build.args(["build", "-p", "rift", "--bin", "rift"]);
        if profile == "release" {
            build.arg("--release");
        }
        let status = build
            .env("CARGO_TARGET_DIR", &target)
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_PROFILE_DEV_DEBUG", "0")
            .env("CARGO_PROFILE_DEV_STRIP", "debuginfo")
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .status()
            .expect("cargo build -p rift");
        assert!(status.success(), "building the rift binary failed");
        // `EXE_SUFFIX` rather than a bare "rift": CI runs this leg on Windows
        // too, and the sibling suite names it the same way for the same reason.
        let exe = target
            .join(&profile)
            .join(format!("rift{}", std::env::consts::EXE_SUFFIX));
        assert!(exe.is_file(), "no rift binary at {}", exe.display());
        exe
    })
    .clone()
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(repo)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A hermetic repository with one commit and the file the scripts edit.
fn seed_repo(tmp: &Path) -> PathBuf {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("src").join("lib.rs"), "fn seed() {}\n").unwrap();
    git(&repo, &["add", "--", "src"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=seed",
            "-c",
            "user.email=seed@rift.invalid",
            "commit",
            "-q",
            "-m",
            "seed",
        ],
    );
    repo
}

/// Redirect a child's projection, data root and keychain into `sandbox`.
fn hermetic<'a>(cmd: &'a mut Command, sandbox: &Path) -> &'a mut Command {
    cmd.env("RIFT_CACHE_DIR", sandbox.join("cache"))
        .env("XDG_DATA_HOME", sandbox.join("data"))
        .env("LOCALAPPDATA", sandbox.join("data"))
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("RIFT_CAPTURE_SYNC", "1")
        .current_dir(sandbox)
}

/// Deliver one hook to the REAL binary over a real process boundary.
///
/// The agent token is a parameter, which is the whole difference from
/// `vogon_e2e.rs`'s `deliver` — that one hard-codes `claude-code`, which is
/// exactly why the other two agents had never crossed a process.
fn deliver(repo: &Path, sandbox: &Path, agent: &str, verb: &str, payload: &serde_json::Value) {
    let mut cmd = Command::new(rift_bin());
    cmd.args(["hooks", agent, verb])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hermetic(&mut cmd, sandbox).current_dir(repo);
    let mut child = cmd.spawn().unwrap();
    use std::io::Write as _;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    assert!(
        child.wait().unwrap().success(),
        "a hook never fails the agent: {agent} {verb}"
    );
}

/// `(column 0, column 1)` for a query against the sandbox's projection.
///
/// Both columns read as TEXT, so a caller counting rows must `CAST(… AS TEXT)`:
/// a bare `count(*)` reads back as the empty string and two empty strings
/// compare equal to each other, which is how a sibling suite's first draft
/// passed vacuously.
fn rows(cache: &Path, repo: &Path, sql: &str) -> Vec<(String, String)> {
    let id = rift_git::repo_id_for(repo).unwrap();
    let db = rift_store::projection_db_under(cache, &id);
    assert!(db.exists(), "no projection at {}", db.display());
    let store = rift_store::open_capture_projection(db).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let rows = rt
        .block_on(rift_store::CanonicalStore::query(&store, sql, &[]))
        .unwrap();
    rows.iter()
        .map(|r| {
            (
                r.get_text(0).unwrap_or_default(),
                r.get_text(1).unwrap_or_default(),
            )
        })
        .collect()
}

/// Every hook receipt, asserted non-empty so an equality over it cannot pass on
/// an empty projection.
fn receipts(cache: &Path, repo: &Path) -> Vec<(String, String)> {
    let receipts = rows(
        cache,
        repo,
        "SELECT verb, outcome FROM agent_hook_event ORDER BY verb",
    );
    assert!(!receipts.is_empty(), "no hook receipts at all");
    receipts
}

/// `kind → count` over every canonical event captured. Never empty when
/// anything was captured, which is what makes an equality assertion on it
/// non-vacuous.
fn census(cache: &Path, repo: &Path) -> BTreeMap<String, usize> {
    let rows = rows(
        cache,
        repo,
        "SELECT kind, CAST(count(*) AS TEXT) FROM captured_event GROUP BY kind ORDER BY kind",
    );
    assert!(!rows.is_empty(), "nothing at all was captured");
    rows.into_iter()
        .map(|(kind, count)| (kind, count.parse().unwrap()))
        .collect()
}

/// Codex's scripted session: the ten verbs its own pinned schema publishes and
/// Rift declares, in the order a real session fires them.
fn codex_script(repo: &Path) -> Vec<(&'static str, serde_json::Value)> {
    let file = repo.join("src").join("lib.rs").display().to_string();
    let base = |extra: serde_json::Value| {
        let mut v = serde_json::json!({
            "session_id": "codex-e2e-1",
            "cwd": repo.display().to_string(),
            "model": "gpt-5.4-codex",
            "permission_mode": "auto",
        });
        for (k, val) in extra.as_object().unwrap() {
            v[k.clone()] = val.clone();
        }
        v
    };
    vec![
        (
            "SessionStart",
            base(serde_json::json!({"source": "startup"})),
        ),
        (
            "UserPromptSubmit",
            base(serde_json::json!({"turn_id": "t1"})),
        ),
        (
            "PreToolUse",
            base(serde_json::json!({
                "turn_id": "t1", "tool_name": "apply_patch",
                "tool_input": {"file_path": file}
            })),
        ),
        (
            "PostToolUse",
            base(serde_json::json!({
                "turn_id": "t1", "tool_name": "apply_patch",
                "tool_use_id": "tu-1", "tool_response": {"ok": true}
            })),
        ),
        (
            "SubagentStart",
            base(serde_json::json!({
                "turn_id": "t1", "agent_id": "child-1", "agent_type": "review"
            })),
        ),
        (
            "SubagentStop",
            base(serde_json::json!({"turn_id": "t1", "agent_id": "child-1"})),
        ),
        ("PreCompact", base(serde_json::json!({"turn_id": "t1"}))),
        ("PostCompact", base(serde_json::json!({"turn_id": "t1"}))),
        ("Stop", base(serde_json::json!({"turn_id": "t1"}))),
        ("SessionEnd", base(serde_json::json!({"reason": "other"}))),
    ]
}

/// Antigravity's scripted session: five verbs in its documented camelCase
/// shape, matching the golden stream `adapter_sweep.rs` pins.
fn antigravity_script(repo: &Path) -> Vec<(&'static str, serde_json::Value)> {
    let file = repo.join("src").join("lib.rs").display().to_string();
    vec![
        (
            "PreInvocation",
            serde_json::json!({"conversationId": "agy-e2e-1", "invocationNum": 0}),
        ),
        (
            "PreToolUse",
            serde_json::json!({
                "conversationId": "agy-e2e-1",
                "toolCall": {"name": "write_to_file", "args": {"AbsolutePath": file}}
            }),
        ),
        (
            "PostToolUse",
            serde_json::json!({"conversationId": "agy-e2e-1", "stepIdx": 1}),
        ),
        (
            "PostInvocation",
            serde_json::json!({"conversationId": "agy-e2e-1", "invocationNum": 0}),
        ),
        (
            "Stop",
            serde_json::json!({"conversationId": "agy-e2e-1", "fullyIdle": true}),
        ),
    ]
}

/// Drive one agent's whole script through the real binary, then commit.
///
/// The script is built from the repo path rather than passed in, because the
/// payloads carry `cwd` and absolute tool paths — a sibling suite learned the
/// expensive way that omitting the repo changes the idempotency key and makes
/// identical-looking hooks land as different rows.
fn play(
    agent: &str,
    build: fn(&Path) -> Vec<(&'static str, serde_json::Value)>,
) -> (tempfile::TempDir, PathBuf, usize) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let script = build(&repo);
    for (verb, payload) in &script {
        deliver(&repo, tmp.path(), agent, verb, payload);
    }
    std::fs::write(repo.join("src").join("lib.rs"), "fn edited() {}\n").unwrap();
    git(&repo, &["add", "--", "src"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=e2e",
            "-c",
            "user.email=e2e@rift.invalid",
            "commit",
            "-q",
            "-m",
            "the turn's work",
        ],
    );
    let driven = script.len();
    (tmp, repo, driven)
}

/// Assert every driven verb left a **recorded** receipt — never `skipped-
/// unknown-verb`, never `deferred-no-session`.
fn every_verb_recorded(cache: &Path, repo: &Path, script_verbs: &[&str]) {
    let receipts = receipts(cache, repo);
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for (verb, outcome) in &receipts {
        assert_eq!(
            outcome, "recorded",
            "{verb} landed as {outcome:?}, not a recorded capture"
        );
        let known = script_verbs
            .iter()
            .find(|v| **v == verb.as_str())
            .unwrap_or_else(|| panic!("{verb} was recorded but never driven"));
        *seen.entry(known).or_default() += 1;
    }
    for verb in script_verbs {
        assert!(
            seen.contains_key(verb),
            "{verb} was driven across a process and left no receipt; seen: {seen:?}"
        );
    }
}

/// **Codex crosses a process boundary, for all ten declared verbs.**
///
/// Before this suite, `vogon_e2e.rs` drove `claude-code` and nothing drove
/// Codex: everything known about it was asserted in-process, where a hook that
/// never reaches `rift hooks codex <verb>` cannot fail. `SessionEnd` is on the
/// list only because Codex's own schema was pinned and found to publish it
/// — so this test would have been impossible to write correctly a commit ago.
///
/// The expected canonical census is written out here rather than derived from
/// the adapter, deliberately: two independent statements of the same fact catch
/// a change to either side, where a shared constant agrees with itself.
#[test]
fn a_codex_session_records_every_declared_verb_through_the_real_binary() {
    let (tmp, repo, driven) = play("codex", codex_script);
    assert_eq!(driven, 10, "the pinned Codex surface declares ten verbs");
    let cache = tmp.path().join("cache");

    every_verb_recorded(
        &cache,
        &repo,
        &[
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "SubagentStart",
            "SubagentStop",
            "PreCompact",
            "PostCompact",
            "Stop",
            "SessionEnd",
        ],
    );

    let census = census(&cache, &repo);
    assert_eq!(census.get("session_start"), Some(&1));
    assert_eq!(census.get("prompt_submit"), Some(&1));
    assert_eq!(census.get("tool_call"), Some(&1));
    assert_eq!(
        census.get("file_edit"),
        Some(&1),
        "apply_patch is Codex's canonical mutation and must fan out to an edit"
    );
    assert_eq!(census.get("tool_result"), Some(&1));
    assert_eq!(census.get("subagent_start"), Some(&1));
    assert_eq!(census.get("subagent_end"), Some(&1));
    assert_eq!(
        census.get("session_compaction"),
        Some(&2),
        "PreCompact and PostCompact both mark a compaction boundary"
    );
    assert_eq!(census.get("turn_end"), Some(&1));
    assert_eq!(
        census.get("session_end"),
        Some(&1),
        "THE PIN'S PAYLOAD: Codex once declared no SessionEnd, so a \
         Codex session had an end nothing recorded"
    );
}

/// **Antigravity crosses a process boundary, for all five declared verbs.**
///
/// Antigravity is Rift-only — no third-party implementation exists to check it
/// against — so its documented surface and its golden stream ARE its recorded
/// surface, and driving them through the shipped binary is the strongest claim
/// available. Its `no_upstream_reference` sweep row stays honestly
/// `Unverifiable`; this test does not change that and is not offered as if it
/// did.
#[test]
fn an_antigravity_session_records_every_declared_verb_through_the_real_binary() {
    let (tmp, repo, driven) = play("antigravity", antigravity_script);
    assert_eq!(driven, 5, "Antigravity declares five verbs");
    let cache = tmp.path().join("cache");

    every_verb_recorded(
        &cache,
        &repo,
        &[
            "PreInvocation",
            "PreToolUse",
            "PostToolUse",
            "PostInvocation",
            "Stop",
        ],
    );

    let census = census(&cache, &repo);
    assert_eq!(
        census.get("session_start"),
        Some(&1),
        "the FIRST invocation is the synthetic session start"
    );
    assert_eq!(census.get("tool_call"), Some(&1));
    assert_eq!(
        census.get("file_edit"),
        Some(&1),
        "write_to_file must fan out to an edit beside the tool call"
    );
    assert_eq!(census.get("tool_result"), Some(&1));
    assert_eq!(census.get("prompt_response"), Some(&1));
    assert_eq!(census.get("turn_end"), Some(&1));

    // The BOUNDARY census (2026-08-29): Antigravity is End-only (`Stop`
    // closes a turn nothing opened), so before the bridge synthesized the
    // missing Start its ordinal never advanced and only one `end` snapshot
    // could ever land (measured: `start` absent). Now the session's first
    // activity opens turn 1 with a synthesized Start and the Stop closes it.
    let boundaries = rows(
        &cache,
        &repo,
        "SELECT boundary, CAST(count(*) AS TEXT) FROM turn_boundary_snapshot \
         GROUP BY boundary ORDER BY boundary",
    );
    assert_eq!(
        boundaries,
        vec![
            ("end".to_owned(), "1".to_owned()),
            ("start".to_owned(), "1".to_owned())
        ],
        "boundary census: one synthesized start, one real end"
    );
}

/// A redelivered hook is a no-op for BOTH agents, across a real process.
///
/// Replay safety is asserted per-adapter in-process by
/// `rift_agent::conformance::check_replay_safe`, and registry-wide through
/// dispatch — but never through the binary for these two, so the stamping that
/// makes a redelivery idempotent had never been exercised end to end for them.
///
/// **ONLY THE TOOL PAIR IS REDELIVERED, and the reason is a measurement the
/// sibling suite already paid for.** The idempotency key carries the turn and
/// event ordinals, so re-sending a turn-OPENING verb is not a redelivery at
/// all — it legitimately opens the next turn and the new rows are correct.
/// `vogon_e2e.rs` records the same correction beside its own replay test
/// (`redelivering_the_last_turns_hooks_writes_no_second_row`, and the
/// `resuming a session id is not a redelivery` test under it). The first draft
/// of THIS test replayed whole scripts and read the doubled census as a dedup
/// failure; measured, the product was right and the test was not. `PreToolUse`
/// and `PostToolUse` neither open nor close a turn, so they are the same hook
/// twice for both agents — and they are the fan-out that matters, because a
/// doubled `file_edit` is a doubled working-tree change.
///
/// **For an End-only adapter the pair is redelivered while its turn is still
/// open** (2026-08-29). Antigravity declares `TurnSignals { start: false, end:
/// true }`: nothing it sends opens a turn, so the bridge opens the next ordinal
/// on the first activity after each `Stop`. A tool hook arriving after the
/// Stop is therefore the next turn's first activity BY DECLARATION — the same
/// "not a redelivery at all" correction the vogon suite recorded for
/// turn-opening verbs — and the only redelivery that is one is the pair sent
/// again inside the open turn. So Antigravity's leg drives its script up to
/// the Stop, redelivers, asserts, and only then closes the turn.
#[test]
fn a_redelivered_tool_hook_writes_no_second_row_for_either_agent() {
    for (agent, build, expected) in [
        ("codex", codex_script as fn(&Path) -> _, 10usize),
        ("antigravity", antigravity_script as fn(&Path) -> _, 5),
    ] {
        let pending: Option<(&'static str, serde_json::Value)>;
        let (tmp, repo, driven) = if agent == "antigravity" {
            let tmp = tempfile::tempdir().unwrap();
            let repo = seed_repo(tmp.path());
            let script: Vec<(&'static str, serde_json::Value)> = build(&repo);
            let (last, head) = script.split_last().unwrap();
            assert_eq!(last.0, "Stop", "the End-only script ends with its Stop");
            for (verb, payload) in head {
                deliver(&repo, tmp.path(), agent, verb, payload);
            }
            pending = Some(last.clone());
            (tmp, repo, script.len())
        } else {
            pending = None;
            play(agent, build)
        };
        assert_eq!(driven, expected);
        let cache = tmp.path().join("cache");
        let before = census(&cache, &repo);
        assert_eq!(
            before.get("file_edit"),
            Some(&1),
            "{agent}: the fan-out this test guards must exist before it is replayed"
        );

        let script = build(&repo);
        let tool_pair: Vec<_> = script
            .iter()
            .filter(|(verb, _)| matches!(*verb, "PreToolUse" | "PostToolUse"))
            .collect();
        assert_eq!(tool_pair.len(), 2, "{agent}: no tool pair to redeliver");
        for (verb, payload) in tool_pair {
            deliver(&repo, tmp.path(), agent, verb, payload);
        }

        assert_eq!(
            census(&cache, &repo),
            before,
            "{agent}: a redelivered tool hook doubled the capture"
        );
        if let Some((verb, payload)) = pending {
            deliver(&repo, tmp.path(), agent, verb, &payload);
            assert_eq!(
                census(&cache, &repo).get("turn_end"),
                Some(&1),
                "{agent}: the deferred Stop closes the one turn"
            );
        }
    }
}
