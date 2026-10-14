//! End-to-end: a session that sends events is live, through the real binary.
//! The three ways a working session used to read as over — ended by an editor
//! restart and then resumed, never started because its start hook was
//! skipped, left `active` by an agent that never ends a session — each
//! reproduced and shown repaired; the trailer's session pick after a repair;
//! and doctor naming both idle floors.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};

use rift_store::CanonicalStore as _;

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::{
    agent_hook_raw, checkpoint_shards, doctor_fix_json, doctor_json, git_in, hook_pair_commit,
    now_ms, run_rift_stdout, seed_plain_repo, session_phase, sessions_leg, shard_blob,
    shard_tree_names, sync_env_with_data, write_agent_transcript, TWO_LINE_TRANSCRIPT,
};

/// One hermetic repository and the two private roots its hooks write under.
struct Fixture {
    tmp: tempfile::TempDir,
    repo: PathBuf,
    cache: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = seed_plain_repo(tmp.path());
        let cache = tmp.path().join("cache");
        let data = tmp.path().join("data");
        Self {
            tmp,
            repo,
            cache,
            data,
        }
    }

    /// One native hook, the bytes its own client sends, drained inline.
    fn hook(&self, agent: &str, verb: &str, body: &serde_json::Value) {
        let env = sync_env_with_data(&self.cache, &self.data);
        agent_hook_raw(&self.repo, agent, verb, &body.to_string(), &env);
    }

    /// The session's cache, as a document.
    fn cache_of(&self, session: &str) -> serde_json::Value {
        let path = self.sessions_dir().join(format!("{session}.json"));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// Rewrite one field of a session's cache on disk: how a fixture ages a
    /// session, or plants a phase an older build left behind.
    fn set_cache_field(&self, session: &str, field: &str, value: serde_json::Value) {
        let mut cache = self.cache_of(session);
        cache[field] = value;
        let path = self.sessions_dir().join(format!("{session}.json"));
        std::fs::write(path, serde_json::to_vec(&cache).unwrap()).unwrap();
    }

    /// `(from, to, trigger_kind)` for every transition of `session`, oldest
    /// first, out of the family's capture projection.
    fn transitions(&self, session: &str) -> Vec<(String, String, String)> {
        let store = rift_store::open_capture_projection(self.projection_db()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let rows = rt
            .block_on(store.query(
                "SELECT from_state, to_state, trigger_kind FROM session_transition \
                 WHERE session_id = ? ORDER BY occurred_at, rowid",
                &[rift_store::Value::text(session)],
            ))
            .unwrap();
        rows.iter()
            .map(|r| {
                let text = |i| r.get_text(i).unwrap();
                (text(0), text(1), text(2))
            })
            .collect()
    }

    /// One native hook with the binary's log on, returning what it logged.
    fn hook_logged(&self, agent: &str, verb: &str, body: &serde_json::Value) -> String {
        use std::io::Write as _;
        use std::process::Stdio;

        let env = sync_env_with_data(&self.cache, &self.data);
        let mut child = common::rift_cmd(&self.repo)
            .args(["hooks", agent, verb])
            .envs(env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())))
            .env("RIFT_LOG", "warn")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(body.to_string().as_bytes()).unwrap();
        drop(stdin);
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "agent hook never fails");
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    fn sessions_dir(&self) -> PathBuf {
        common::sessions_dir(&self.repo.join(".git"), &self.cache)
    }

    /// The file names in the sessions directory that a hook moved aside.
    fn moved_aside(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.sessions_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".json.torn-"))
            .collect();
        names.sort();
        names
    }

    /// How many of `session`'s events the projection captured.
    fn captured_events(&self, session: &str) -> i64 {
        let store = rift_store::open_capture_projection(self.projection_db()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(store.query_opt_i64(
            "SELECT count(*) FROM captured_event WHERE session_id = ?",
            &[rift_store::Value::text(session)],
        ))
        .unwrap()
        .unwrap_or(0)
    }

    fn projection_db(&self) -> PathBuf {
        let paths = rift_git::discover(&self.repo).unwrap();
        let cache_dir = self.cache.display().to_string();
        let lookup = |key: &str| (key == "RIFT_CACHE_DIR").then(|| cache_dir.clone());
        let rift_git::FamilyState::Minted(family) =
            rift_git::family::resolve_family_under(lookup, &paths, rift_git::MintPolicy::Never)
                .unwrap()
        else {
            panic!("the hooks above should have minted the family");
        };
        rift_store::projection_db_for_family_under(&self.cache, &family)
    }
}

/// A Claude Code payload for `session`, naming its transcript.
fn claude(session: &str, transcript: &Path, extra: serde_json::Value) -> serde_json::Value {
    let mut body = serde_json::json!({
        "session_id": session,
        "transcript_path": transcript.display().to_string(),
        "tool_name": "Bash",
        "tool_input": {"command": "true"},
    });
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    body
}

fn no_refusals(transitions: &[(String, String, String)]) -> bool {
    transitions
        .iter()
        .all(|(_, _, kind)| !kind.starts_with("refused-"))
}

/// **(a) An editor restart ends a session, and the agent resumes it.** A real
/// `SessionEnd` took the session to `ended`; the resume under the same id
/// (`SessionStart` with `source: resume`) used to be refused, and every hook
/// after it was captured while the session read `ended`. Now the resume
/// returns it to `active`, a tool call keeps it there, and its next end
/// condenses the bytes written since the first checkpoint.
#[test]
fn a_session_resumed_after_its_end_is_working_again_and_condenses_again() {
    let fx = Fixture::new();
    let s = "liveness-resume";
    let transcript = write_agent_transcript(fx.tmp.path(), s, TWO_LINE_TRANSCRIPT);
    let body = |extra| claude(s, &transcript, extra);
    fx.hook(
        "claude-code",
        "SessionStart",
        &body(serde_json::json!({"source": "startup"})),
    );
    fx.hook(
        "claude-code",
        "UserPromptSubmit",
        &body(serde_json::json!({"prompt": "go"})),
    );
    fx.hook(
        "claude-code",
        "SessionEnd",
        &body(serde_json::json!({"reason": "other"})),
    );
    assert_eq!(session_phase(&fx.repo, s), "ended");
    assert_eq!(
        checkpoint_shards(&fx.repo).len(),
        1,
        "the first end condensed"
    );

    fx.hook(
        "claude-code",
        "SessionStart",
        &body(serde_json::json!({"source": "resume"})),
    );
    assert_eq!(
        session_phase(&fx.repo, s),
        "active",
        "the resume is accepted"
    );
    fx.hook("claude-code", "PreToolUse", &body(serde_json::json!({})));
    assert_eq!(session_phase(&fx.repo, s), "active");

    let more = concat!(
        r#"{"type":"user","message":{"role":"user","content":"and one more"}}"#,
        "\n"
    );
    let mut grown = std::fs::read_to_string(&transcript).unwrap();
    grown.push_str(more);
    std::fs::write(&transcript, grown).unwrap();
    fx.hook(
        "claude-code",
        "SessionEnd",
        &body(serde_json::json!({"reason": "exit"})),
    );
    assert_eq!(session_phase(&fx.repo, s), "ended");
    assert_eq!(
        checkpoint_shards(&fx.repo).len(),
        2,
        "the second end condensed the new bytes into a checkpoint of their own"
    );
    let transitions = fx.transitions(s);
    assert!(
        transitions.contains(&("ended".into(), "active".into(), "start".into())),
        "{transitions:?}"
    );
    assert!(no_refusals(&transitions), "{transitions:?}");
}

/// **(b) A session whose start hook never arrived.** Codex skips a hook whose
/// trust lapsed and runs the rest, so its sessions sent tool events only and
/// sat `idle` at turn 0 for ever. Now the first event opens the session,
/// filed as a synthesized start, and opens turn 1; a new native turn name
/// opens turn 2 with no prompt hook in sight.
#[test]
fn tool_events_open_a_session_that_never_started_and_its_turns() {
    let fx = Fixture::new();
    let s = "liveness-codex-no-start";
    let tool = |turn: &str| {
        serde_json::json!({
            "session_id": s,
            "turn_id": turn,
            "tool_name": "Bash",
            "tool_input": {"command": "true"},
        })
    };
    fx.hook("codex", "PreToolUse", &tool("turn-a"));
    assert_eq!(session_phase(&fx.repo, s), "active");
    let cache = fx.cache_of(s);
    assert_eq!(cache["current_turn_ordinal"], 1, "{cache}");
    assert_eq!(cache["turn_open"], true, "{cache}");
    fx.hook("codex", "PostToolUse", &tool("turn-a"));
    fx.hook("codex", "Stop", &tool("turn-a"));
    fx.hook("codex", "PreToolUse", &tool("turn-b"));
    let cache = fx.cache_of(s);
    assert_eq!(cache["current_turn_ordinal"], 2, "{cache}");
    assert_eq!(session_phase(&fx.repo, s), "active");
    let transitions = fx.transitions(s);
    assert_eq!(
        transitions.first(),
        Some(&("idle".into(), "active".into(), "synthesized-start".into())),
        "{transitions:?}"
    );
    assert!(no_refusals(&transitions), "{transitions:?}");
}

/// A Codex session opened and closed without a prompt sends only its
/// `SessionEnd` (Codex runs its start hook when a turn starts). That one hook
/// opens the session and ends it in one pass, with an empty condense, rather
/// than leaving an idle cache nothing can ever reach.
#[test]
fn a_lone_session_end_opens_and_ends_the_session_in_one_pass() {
    let fx = Fixture::new();
    let s = "liveness-codex-promptless";
    fx.hook("codex", "SessionEnd", &serde_json::json!({"session_id": s}));
    assert_eq!(session_phase(&fx.repo, s), "ended");
    let kinds: Vec<String> = fx.transitions(s).into_iter().map(|(_, _, k)| k).collect();
    assert_eq!(kinds, ["synthesized-start", "end", "condense-done"]);
}

/// **(c) An agent that never ends a session.** Antigravity publishes no
/// session end, so its sessions read `active` for days. Six minutes quiet is
/// past the family's five-minute floor, and the next `SessionStart` of any
/// session sweeps it: condensed to a checkpoint and `ended`, while the
/// session that started is `active`.
#[test]
fn a_quiet_session_is_ended_with_a_checkpoint_by_the_next_session_start() {
    let fx = Fixture::new();
    let quiet = "liveness-antigravity-quiet";
    let invocation = serde_json::json!({"conversationId": quiet, "invocationNum": 0});
    fx.hook("antigravity", "PreInvocation", &invocation);
    assert_eq!(session_phase(&fx.repo, quiet), "active");
    fx.set_cache_field(
        quiet,
        "updated_at_ms",
        serde_json::json!(now_ms() - 6 * 60_000),
    );
    assert!(checkpoint_shards(&fx.repo).is_empty());

    let fresh = "liveness-claude-fresh";
    let transcript = write_agent_transcript(fx.tmp.path(), fresh, TWO_LINE_TRANSCRIPT);
    let start = claude(fresh, &transcript, serde_json::json!({"source": "startup"}));
    fx.hook("claude-code", "SessionStart", &start);

    assert_eq!(
        session_phase(&fx.repo, quiet),
        "ended",
        "the sweep ended it"
    );
    assert_eq!(session_phase(&fx.repo, fresh), "active");
    assert_eq!(checkpoint_shards(&fx.repo).len(), 1, "with a checkpoint");
    let kinds: Vec<String> = fx
        .transitions(quiet)
        .into_iter()
        .map(|(_, _, k)| k)
        .collect();
    assert_eq!(kinds, ["start", "idle-timeout", "condense-done"]);
}

/// **The trailer names the session that is working.** With a real session
/// ended and a foreign one still `active`, the commit trailer used to name the
/// foreign one. Once the real session's next hook repairs it, it is live and
/// most recently touched, so the predicted checkpoint is its own, and the
/// stale foreign session is not on it.
#[test]
fn the_trailer_names_the_repaired_session_and_not_a_stale_foreign_one() {
    let fx = Fixture::new();
    let foreign = "liveness-foreign-stale";
    let invocation = serde_json::json!({"conversationId": foreign, "invocationNum": 0});
    fx.hook("antigravity", "PreInvocation", &invocation);
    fx.set_cache_field(
        foreign,
        "updated_at_ms",
        serde_json::json!(now_ms() - 60_000),
    );

    let mine = "liveness-mine";
    // No transcript: the trailer's prediction is then exact, so the shard it
    // names can be found by its id.
    let body = serde_json::json!({"session_id": mine, "tool_name": "Bash"});
    fx.hook("claude-code", "SessionStart", &body);
    fx.hook("claude-code", "SessionEnd", &body);
    assert_eq!(session_phase(&fx.repo, mine), "ended");
    fx.hook("claude-code", "PreToolUse", &body);
    assert_eq!(
        session_phase(&fx.repo, mine),
        "active",
        "repaired by its hook"
    );

    std::fs::write(fx.repo.join("w.txt"), "work\n").unwrap();
    git_in(&fx.repo, &["add", "w.txt"]);
    let env = sync_env_with_data(&fx.cache, &fx.data);
    let predicted = hook_pair_commit(&fx.repo, fx.tmp.path(), "work", &env);
    let names = shard_tree_names(&fx.repo);
    let meta = names
        .lines()
        .find(|l| l.ends_with("metadata.json") && l.replace('/', "").contains(&predicted))
        .unwrap_or_else(|| panic!("no shard for {predicted} in {names}"));
    let meta = shard_blob(&fx.repo, meta);
    assert!(
        meta.contains(mine),
        "the trailer's checkpoint is mine: {meta}"
    );
    assert!(!meta.contains(foreign), "and not the foreign one's: {meta}");
}

/// Doctor names both floors: a session past the sweep's five minutes is
/// sweepable, one past a day is STUCK, and `--fix` sweeps at the sweep's own
/// floor. A session recorded `ended` while its own events kept arriving is
/// reported as Rift's defect, with nothing asked of the developer.
#[test]
fn doctor_reports_sweepable_and_stuck_sessions_and_a_phase_behind_its_events() {
    let fx = Fixture::new();
    let env = sync_env_with_data(&fx.cache, &fx.data);
    let behind = "age-behind";
    fx.hook(
        "claude-code",
        "SessionStart",
        &serde_json::json!({"session_id": behind}),
    );
    fx.hook(
        "claude-code",
        "PreToolUse",
        &serde_json::json!({"session_id": behind}),
    );
    fx.set_cache_field(behind, "state", serde_json::json!("ended"));
    // Every session starts fresh, and only then is aged: a hook drains inline
    // here, and its sweep would otherwise end an aged one before doctor looks.
    let ages = [
        ("age-fresh", 0),
        ("age-six-min", 6 * 60_000),
        ("age-two-days", 2 * 86_400_000),
    ];
    for (session, _) in ages {
        fx.hook(
            "claude-code",
            "SessionStart",
            &serde_json::json!({"session_id": session}),
        );
    }
    for (session, age) in ages {
        fx.set_cache_field(session, "updated_at_ms", serde_json::json!(now_ms() - age));
    }

    let report = doctor_json(&fx.repo, &env);
    let leg = sessions_leg(&report);
    assert_eq!(
        leg["sweepable"],
        serde_json::json!(["age-six-min"]),
        "{leg}"
    );
    assert_eq!(leg["stuck"], serde_json::json!(["age-two-days"]), "{leg}");
    assert_eq!(leg["idle_sweep_ms"], 300_000, "{leg}");
    assert_eq!(leg["behind_events"], serde_json::json!([behind]), "{leg}");
    let (_, human) = run_rift_stdout(&fx.repo, &["doctor"], &env);
    assert!(human.contains("sweepable (idle past 5 min"), "{human}");
    assert!(human.contains("STUCK past 24h"), "{human}");
    assert!(human.contains("Rift's own state error"), "{human}");

    let fixed = doctor_fix_json(&fx.repo, &env);
    let mut finalized: Vec<String> = sessions_leg(&fixed)["finalized"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    finalized.sort();
    assert_eq!(finalized, ["age-six-min", "age-two-days"], "{fixed}");
    assert_eq!(session_phase(&fx.repo, "age-fresh"), "active");
}

/// The cache a race published on a real machine: one whole object, then the
/// 37-byte tail of a longer one a concurrent writer of the same session had
/// written into the same temp file. Byte for byte but for the transcript's
/// file name, which spelled the session id beside the path and is shortened
/// here so the fixture adds no high-entropy token to the repository's text.
const TORN_CACHE: &str = r#"{
  "schema": 1,
  "session_id": "6455cff9-8e83-44ea-96d9-ddfc9922c065",
  "state": "active",
  "agent": "claude-code",
  "current_turn_ordinal": 2,
  "turn_open": false,
  "compaction_boundary": null,
  "updated_at_ms": 1790405788184,
  "native_id_map": {},
  "transcript_path": "/Users/ulpian/.claude/projects/-Users-ulpian-Documents-codebase-rift-rift-cli/transcript.jsonl",
  "work_tree": "/Users/ulpian/Documents/codebase/rift/rift-cli",
  "last_worktree": {
    "token": "94b3809e5b05",
    "name": "main",
    "linked": false,
    "branch": "main",
    "head": "6e8ac49e8247056a2efcbc4d89d0c5f0c010a796"
  }
}"subagent": "affb8b381701b8ee6"
  }
}"#;

/// The session the torn cache belongs to.
const TORN_SESSION: &str = "6455cff9-8e83-44ea-96d9-ddfc9922c065";

/// **A torn cache no longer stops capture.** Every hook after the tear used
/// to fail its ingest on the parse, so the session captured nothing until a
/// person edited the file. Now the next hook moves the file aside whole,
/// re-opens the session as a synthesized start, captures its event, writes
/// the cache whole and says so in its log; doctor reports the moved-aside
/// file as Rift's own defect.
#[test]
fn a_torn_session_cache_is_moved_aside_and_the_next_hook_captures() {
    let fx = Fixture::new();
    std::fs::create_dir_all(fx.sessions_dir()).unwrap();
    let cache_file = fx.sessions_dir().join(format!("{TORN_SESSION}.json"));
    std::fs::write(&cache_file, TORN_CACHE).unwrap();
    assert!(serde_json::from_str::<serde_json::Value>(TORN_CACHE).is_err());

    let body = serde_json::json!({
        "session_id": TORN_SESSION,
        "tool_name": "Bash",
        "tool_input": {"command": "true"},
        "tool_response": {"stdout": ""},
    });
    let log = fx.hook_logged("claude-code", "PostToolUse", &body);

    assert!(
        log.contains("session cache unreadable, moved aside, session re-opened"),
        "{log}"
    );
    assert_eq!(session_phase(&fx.repo, TORN_SESSION), "active");
    assert_eq!(fx.cache_of(TORN_SESSION)["session_id"], TORN_SESSION);
    assert!(
        fx.captured_events(TORN_SESSION) > 0,
        "the event is captured"
    );
    let aside = fx.moved_aside();
    assert_eq!(aside.len(), 1, "{aside:?}");
    let kept = std::fs::read_to_string(fx.sessions_dir().join(&aside[0])).unwrap();
    assert_eq!(kept, TORN_CACHE, "moved aside whole, nothing deleted");
    assert_eq!(
        fx.transitions(TORN_SESSION),
        [("idle".into(), "active".into(), "synthesized-start".into())]
    );

    let env = sync_env_with_data(&fx.cache, &fx.data);
    let leg = sessions_leg(&doctor_json(&fx.repo, &env)).clone();
    assert_eq!(leg["torn_caches"], serde_json::json!(aside), "{leg}");
    assert_eq!(leg["unreadable_caches"], serde_json::json!([]), "{leg}");
    let (_, human) = run_rift_stdout(&fx.repo, &["doctor"], &env);
    assert!(
        human.contains("1 session cache(s) found unreadable and moved aside"),
        "{human}"
    );
    assert!(
        human.contains("Rift's own write error, not yours"),
        "{human}"
    );
}

/// Before any hook repairs it, a cache that does not parse is named for what
/// it is: `rift session list` prints `cache unreadable` where it printed
/// `archived`, and doctor counts it as Rift's defect with nothing to do.
#[test]
fn an_unreadable_cache_is_listed_as_unreadable_and_reported_by_doctor() {
    let fx = Fixture::new();
    let body = serde_json::json!({"session_id": TORN_SESSION, "tool_name": "Bash"});
    fx.hook("claude-code", "SessionStart", &body);
    fx.hook("claude-code", "PreToolUse", &body);
    let cache_file = fx.sessions_dir().join(format!("{TORN_SESSION}.json"));
    std::fs::write(&cache_file, TORN_CACHE).unwrap();

    let env = sync_env_with_data(&fx.cache, &fx.data);
    let (_, listed) = run_rift_stdout(&fx.repo, &["session", "list"], &env);
    let line = listed
        .lines()
        .find(|l| l.contains(TORN_SESSION))
        .unwrap_or_else(|| panic!("the session is listed: {listed}"));
    assert!(line.contains("cache unreadable"), "{line}");
    assert!(!line.contains("archived"), "{line}");

    let leg = sessions_leg(&doctor_json(&fx.repo, &env)).clone();
    assert_eq!(
        leg["unreadable_caches"],
        serde_json::json!([TORN_SESSION]),
        "{leg}"
    );
    let (_, human) = run_rift_stdout(&fx.repo, &["doctor"], &env);
    assert!(
        human.contains("1 session cache(s) unreadable — Rift's own write error"),
        "{human}"
    );
    assert_eq!(
        std::fs::read_to_string(&cache_file).unwrap(),
        TORN_CACHE,
        "reading moves nothing"
    );
}
