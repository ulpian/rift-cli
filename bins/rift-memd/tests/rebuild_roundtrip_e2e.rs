//! A fresh cache root and both rebuilds reproduce what Rift recorded.
//!
//! One Claude Code session is replayed through the shipped `rift` binary with
//! the capture worker drained inline: a file read, then asked for again, so
//! the second read is declined; a prompt, so the session is handed its
//! pointer; a glob, whose result Rift measures as search tokens; one
//! `rift recall file-if-changed` made from inside the session,
//! whose tool result carries its `serve:` keys back through `PostToolUse` and
//! so attributes it; two turns settled on two models, one of them unpriced; and
//! a session end that condenses everything into a checkpoint. The figures
//! the owner reads are recorded off `rift savings --json`, `rift recap --json`
//! and the two projections' raw rows. Then the cache root moves to an empty
//! directory, `rift rebuild` and `rift-memd rebuild --repo` run over the same
//! repository, and the same figures are recorded again.
//!
//! Every figure comes back but one declared difference: a serve a person
//! makes in a terminal names no session, so no checkpoint freezes it and no
//! rebuild can bring it back. The fixture makes one, and it is present before
//! the rebuild and absent after it, with exactly its own verb, channel and
//! token figures gone.
//!
//! The savings share is compared too, with its denominator `read_tokens`, its
//! search channel and the session count: the shard's `tool_calls.jsonl`
//! freezes each read's instant, turn, blob oid and tokenizer, its
//! `tool_results.jsonl` each measured tool result, and its `sessions.jsonl`
//! the session's agent, so the capture rebuild writes back the rows the
//! denominator joins and the session they hang off. The measured tool results
//! are compared row for row, every column included.
//!
//! Hermetic by construction, as `rebuild_spelling_e2e` is: a fixture
//! repository in a tempdir and every root either binary could resolve on the
//! developer's machine redirected (`RIFT_CACHE_DIR`, `RIFT_CONFIG_DIR`,
//! `RIFT_FAMILY_DIR`, `HOME`, the data roots). The binaries take no clock, so
//! they stamp rows with their own; nothing asserted here is a time.
//!
//! The `rift` binary is not this package's, so cargo names no path for it:
//! it is the sibling of `rift-memd` in the same target directory, which a
//! workspace run (or `cargo test -p rift -p rift-memd`) builds before any
//! test starts.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rift_store::{CanonicalStore, SqliteStore, Value};

/// The native session id the replay delivers on every hook.
const SESSION: &str = "roundtrip-1";
/// The first turn's model, which the shipped price catalog prices.
const PRICED: &str = "claude-fable-5";
/// The second turn's model, which no catalog row prices.
const UNPRICED: &str = "claude-unpriced-9";

/// Environment that re-targets a child git or rift at another repository.
const RETARGETING_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_CEILING_DIRECTORIES",
];

/// What the owner reads off one projection pair: the figures a rebuild must
/// reproduce.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Figures {
    /// `memd_audit_event` rows by verb.
    decisions_by_verb: BTreeMap<String, i64>,
    /// `rift savings --json`, `overall.tokens_avoided`.
    tokens_avoided: i64,
    /// `rift savings --json`, `overall.tokens_returned`.
    tokens_returned: i64,
    /// `serve_call_event` rows by channel.
    serves_by_channel: BTreeMap<String, i64>,
    /// Settled tokens by model: uncached input, cached input, cache creation,
    /// output.
    tokens_by_model: BTreeMap<String, [i64; 4]>,
    /// `rift recap --json`, `cost.amount_micros`.
    cost_micros: i64,
    /// `rift savings --json`, `overall.read_tokens`: the share's denominator.
    read_tokens: i64,
    /// `rift savings --json`, `overall.share_pct`, as the document spells it.
    share_pct: String,
    /// `rift savings --json`, `overall.channels.search`: the denominator's
    /// search half.
    search_tokens: i64,
    /// Every `tool_result_tokens` row, every column, in id order.
    tool_results: Vec<Vec<String>>,
    /// `session` rows in the capture projection.
    sessions: i64,
}

/// The two keys a serve's answer names: its call row and its decision row.
#[derive(Debug, Clone)]
struct Serve {
    call_id: String,
    audit_id: String,
}

/// A fixture repository and the redirected roots both binaries run under.
struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    transcript: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        seed_repository(&repo);
        let transcript = tmp.path().join(format!("{SESSION}.jsonl"));
        Self {
            _tmp: tmp,
            repo,
            home,
            transcript,
        }
    }

    /// Every root either binary resolves, under `home`; the cache root is
    /// the one this call names, so the two halves of the round trip share
    /// everything but the projections.
    fn env(&self, cache: &str) -> Vec<(&'static str, PathBuf)> {
        let home = &self.home;
        vec![
            ("RIFT_CACHE_DIR", home.join(cache)),
            ("RIFT_CONFIG_DIR", home.join("config")),
            ("RIFT_FAMILY_DIR", home.join("families")),
            ("XDG_DATA_HOME", home.join("data")),
            ("LOCALAPPDATA", home.join("data")),
            ("HOME", home.clone()),
            ("RIFT_NO_KEYCHAIN", PathBuf::from("1")),
            ("RIFT_CAPTURE_SYNC", PathBuf::from("1")),
        ]
    }

    /// `rift <args>` from the fixture repository under `cache`, `stdin` fed
    /// to it: the exit code and stdout.
    fn rift(&self, cache: &str, args: &[&str], stdin: &str) -> (i32, String) {
        let mut cmd = Command::new(rift_bin());
        cmd.current_dir(&self.repo)
            .args(args)
            .envs(self.env(cache))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for key in RETARGETING_ENV {
            cmd.env_remove(key);
        }
        let mut child = cmd.spawn().unwrap();
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

    /// A `--json` verb that answers: exit 0, parsed.
    fn rift_json(&self, cache: &str, args: &[&str]) -> serde_json::Value {
        let (code, out) = self.rift(cache, args, "");
        assert_eq!(code, 0, "{args:?}: {out}");
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{args:?}: {e}: {out}"))
    }

    /// One Claude Code hook of the session, in the shape the client sends.
    fn hook(&self, verb: &str, body: serde_json::Value) -> String {
        let mut body = body;
        body["session_id"] = SESSION.into();
        body["hook_event_name"] = verb.into();
        body["cwd"] = self.repo.display().to_string().into();
        let args = ["hooks", "claude-code", verb];
        let (code, out) = self.rift("live", &args, &body.to_string());
        assert_eq!(code, 0, "a capture hook never fails the agent: {verb}");
        out
    }

    /// `rift-memd rebuild --repo` over the fixture under `cache`.
    fn memd_rebuild(&self, cache: &str) {
        let repo = self.repo.display().to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rift-memd"));
        cmd.current_dir(&self.home)
            .args(["rebuild", "--repo", &repo])
            .envs(self.env(cache));
        for key in RETARGETING_ENV {
            cmd.env_remove(key);
        }
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "rift-memd rebuild: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Append transcript lines, as the client writes them after a turn.
    fn transcribe(&self, lines: &[serde_json::Value]) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.transcript)
            .unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }
}

/// ★ The live figures and the figures two cold rebuilds derive from the
/// checkpoint shard agree, but for the one serve no session made.
#[test]
fn a_fresh_cache_root_and_both_rebuilds_reproduce_every_recorded_figure_but_a_person_made_serve() {
    let fx = Fixture::new();
    let person = replay_session(&fx);
    let before = figures(&fx, "live");
    assert_the_session_left_every_kind_of_figure(&before);
    assert_eq!(serve_rows(&fx, "live", &person), (1, 1), "made, before");
    let expected = without(&fx, &before, &person);

    let (code, out) = fx.rift("rebuilt", &["rebuild"], "");
    assert_eq!(code, 0, "rift rebuild: {out}");
    fx.memd_rebuild("rebuilt");
    let after = figures(&fx, "rebuilt");

    assert_eq!(after, expected, "every figure but the person-made serve");
    assert_eq!(
        serve_rows(&fx, "rebuilt", &person),
        (0, 0),
        "frozen nowhere"
    );
    for cache in ["live", "rebuilt"] {
        let (code, out) = fx.rift(cache, &["verify"], "");
        assert_eq!(code, 0, "{cache}: {out}");
        assert!(out.contains("verify: OK"), "{cache}: {out}");
    }
}

/// The session, replayed hook by hook. Hands back the serve a person made
/// in a terminal while it ran.
fn replay_session(fx: &Fixture) -> Serve {
    let transcript = fx.transcript.display().to_string();
    fx.hook(
        "SessionStart",
        serde_json::json!({"source": "startup", "transcript_path": transcript}),
    );
    fx.hook("UserPromptSubmit", serde_json::json!({"prompt": "add it"}));
    // Before the read writes `booked.txt`: the glob is measured against the
    // path index only while the tree is the tree the index was built over.
    fx.hook(
        "PostToolUse",
        serde_json::json!({
            "tool_name": "Glob",
            "tool_input": {"pattern": "*.txt"},
            "tool_response": {"filenames": ["a.txt"]},
            "tool_use_id": "toolu_roundtrip_glob",
        }),
    );
    decline_a_repeat_read(fx);
    let (answer, _) = cli_serve(fx, "a.txt");
    fx.hook(
        "PostToolUse",
        serde_json::json!({
            "tool_name": "Bash",
            "tool_input": {"command": "rift recall file-if-changed a.txt"},
            "tool_response": {"stdout": answer, "stderr": "", "interrupted": false},
            "tool_use_id": "toolu_roundtrip_bash",
        }),
    );
    let (_, person) = cli_serve(fx, "booked.txt");
    settle_two_turns(fx);
    fx.hook("SessionEnd", serde_json::json!({"reason": "exit"}));
    person
}

/// `booked.txt` read whole, then read again: the repeat is declined.
fn decline_a_repeat_read(fx: &Fixture) {
    let file = fx.repo.join("booked.txt");
    let body = (1..=40).fold(String::new(), |mut body, n| {
        let _ = writeln!(body, "line {n}");
        body
    });
    std::fs::write(&file, body).unwrap();
    let read = serde_json::json!({
        "tool_name": "Read",
        "tool_input": {"file_path": file.display().to_string(), "offset": 1, "limit": 40},
        "tool_use_id": "toolu_roundtrip_read",
    });
    fx.hook("PreToolUse", read.clone());
    let mut done = read.clone();
    done["tool_response"] = serde_json::json!({
        "file": {"filePath": file.display().to_string(), "content": std::fs::read_to_string(&file).unwrap()},
    });
    fx.hook("PostToolUse", done);
    let repeat = fx.hook("PreToolUse", read);
    assert!(
        repeat.contains(r#""permissionDecision":"deny""#),
        "{repeat}"
    );
}

/// `rift recall file-if-changed <path>` through the shipped CLI: its answer
/// and the two keys the answer names.
fn cli_serve(fx: &Fixture, path: &str) -> (String, Serve) {
    let (code, out) = fx.rift("live", &["recall", "file-if-changed", path], "");
    assert_eq!(code, 0, "{out}");
    let (call_id, audit_id) = out
        .lines()
        .find_map(|line| line.strip_prefix("serve: "))
        .and_then(|keys| keys.split_once(' '))
        .unwrap_or_else(|| panic!("the answer names its keys: {out}"));
    let serve = Serve {
        call_id: call_id.to_owned(),
        audit_id: audit_id.to_owned(),
    };
    (out.clone(), serve)
}

/// Two turns, each settled the way the hooks settle one: the first on
/// [`PRICED`] by the prompt that opens the second, the second on
/// [`UNPRICED`] by the session's end. A transcript bills one model per turn,
/// its last, so two models take two turns.
fn settle_two_turns(fx: &Fixture) {
    let usage = |output: i64| {
        serde_json::json!({"input_tokens": 20, "output_tokens": output,
            "cache_read_input_tokens": 300, "cache_creation_input_tokens": 40})
    };
    let turn = |prompt: &str, model: &str, output: i64| {
        [
            serde_json::json!({"type": "user", "message": {"content": prompt}}),
            serde_json::json!({"type": "assistant",
                "message": {"model": model, "usage": usage(output)}}),
        ]
    };
    fx.hook("Stop", serde_json::json!({"stop_hook_active": false}));
    fx.transcribe(&turn("add it", PRICED, 7));
    fx.hook("UserPromptSubmit", serde_json::json!({"prompt": "test it"}));
    fx.hook("Stop", serde_json::json!({"stop_hook_active": false}));
    fx.transcribe(&turn("test it", UNPRICED, 11));
}

/// The figures under `cache`: the two documents, then the raw rows of the
/// two projections `rift savings` names.
fn figures(fx: &Fixture, cache: &str) -> Figures {
    let savings = fx.rift_json(cache, &["savings", "--json"]);
    let recap = fx.rift_json(cache, &["recap", "--json"]);
    let recall = open_named(&savings, "recall_db");
    let capture = open_named(&savings, "projection_db");
    Figures {
        decisions_by_verb: counted(
            &recall,
            "SELECT verb, count(*) FROM memd_audit_event GROUP BY verb",
        ),
        tokens_avoided: savings["overall"]["tokens_avoided"].as_i64().unwrap(),
        tokens_returned: savings["overall"]["tokens_returned"].as_i64().unwrap(),
        serves_by_channel: counted(
            &recall,
            "SELECT channel, count(*) FROM serve_call_event GROUP BY channel",
        ),
        tokens_by_model: tokens_by_model(&capture),
        cost_micros: recap["cost"]["amount_micros"].as_i64().unwrap(),
        read_tokens: savings["overall"]["read_tokens"].as_i64().unwrap(),
        share_pct: savings["overall"]["share_pct"].to_string(),
        search_tokens: savings["overall"]["channels"]["search"]
            .as_i64()
            .unwrap_or(0),
        tool_results: tool_result_rows(&capture),
        sessions: counted(&capture, "SELECT 'session', count(*) FROM session")["session"],
    }
}

/// The figures the rebuild must reproduce: `before` with the person-made
/// serve's own verb, channel and tokens taken out, read off its own rows.
fn without(fx: &Fixture, before: &Figures, person: &Serve) -> Figures {
    let savings = fx.rift_json("live", &["savings", "--json"]);
    let recall = open_named(&savings, "recall_db");
    let decision = only_row(
        &recall,
        "SELECT verb FROM memd_audit_event WHERE audit_id = ?",
        &person.audit_id,
    );
    let call = only_row(
        &recall,
        "SELECT channel, tokens_avoided, tokens_returned FROM serve_call_event \
         WHERE call_id = ?",
        &person.call_id,
    );
    let verb = decision.get_text(0).unwrap();
    let channel = call.get_text(0).unwrap();
    let mut expected = before.clone();
    *expected.decisions_by_verb.get_mut(&verb).unwrap() -= 1;
    *expected.serves_by_channel.get_mut(&channel).unwrap() -= 1;
    expected.decisions_by_verb.retain(|_, n| *n > 0);
    expected.serves_by_channel.retain(|_, n| *n > 0);
    expected.tokens_avoided -= call.get_i64(1).unwrap();
    expected.tokens_returned -= call.get_i64(2).unwrap();
    // The share moves with its numerator, by the one formula the report uses.
    let count = |n: i64| u64::try_from(n).unwrap();
    let share = rift_recall_ledger::savings::share(
        count(expected.tokens_avoided),
        count(expected.read_tokens),
    );
    expected.share_pct = serde_json::json!(share).to_string();
    expected
}

/// The replay left every kind of figure the round trip compares, so an
/// equality below cannot pass over two empty projections.
fn assert_the_session_left_every_kind_of_figure(before: &Figures) {
    let verbs: Vec<&str> = before
        .decisions_by_verb
        .keys()
        .map(String::as_str)
        .collect();
    for verb in [
        "memd.manifest_injected",
        "memd.manifest_served",
        "memd.read_intercepted",
    ] {
        assert!(verbs.contains(&verb), "{verb} in {verbs:?}");
    }
    let channels: Vec<&str> = before
        .serves_by_channel
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(channels, ["hook_inject", "skill_cli"]);
    assert!(before.tokens_avoided > 0, "{before:?}");
    assert!(before.tokens_returned > 0, "{before:?}");
    let models: Vec<&str> = before.tokens_by_model.keys().map(String::as_str).collect();
    assert_eq!(models, [PRICED, UNPRICED]);
    assert!(before.cost_micros > 0, "the priced turn costs: {before:?}");
    assert!(
        before.read_tokens > 0,
        "the read is in the denominator: {before:?}"
    );
    assert!(
        before.search_tokens > 0,
        "the glob is in the denominator: {before:?}"
    );
    assert_eq!(before.tool_results.len(), 1, "{before:?}");
    assert_eq!(before.sessions, 1, "{before:?}");
}

/// Every `tool_result_tokens` row of the capture projection, each column
/// spelled as text, so a rebuilt row is compared field for field.
fn tool_result_rows(capture: &SqliteStore) -> Vec<Vec<String>> {
    let rows = select(
        capture,
        "SELECT tool_result_id, session_id, tool_name, channel, \
         CAST(output_tokens AS TEXT), tokenizer, basis, CAST(recorded_at AS TEXT), \
         CAST(schema_version AS TEXT) FROM tool_result_tokens ORDER BY tool_result_id",
        &[],
    );
    rows.iter()
        .map(|row| (0..9).map(|i| row.get_text(i).unwrap()).collect())
        .collect()
}

/// How many `memd_audit_event` and `serve_call_event` rows carry the serve's
/// two keys under `cache`.
fn serve_rows(fx: &Fixture, cache: &str, serve: &Serve) -> (i64, i64) {
    let savings = fx.rift_json(cache, &["savings", "--json"]);
    let recall = open_named(&savings, "recall_db");
    let rows = |sql: &str, key: &str| {
        let found = select(&recall, sql, &[Value::text(key)]);
        found[0].get_i64(0).unwrap()
    };
    (
        rows(
            "SELECT count(*) FROM memd_audit_event WHERE audit_id = ?",
            &serve.audit_id,
        ),
        rows(
            "SELECT count(*) FROM serve_call_event WHERE call_id = ?",
            &serve.call_id,
        ),
    )
}

/// The settled tokens of the capture projection, summed by model.
fn tokens_by_model(capture: &SqliteStore) -> BTreeMap<String, [i64; 4]> {
    let rows = select(
        capture,
        "SELECT mc.model, sum(tu.uncached_input), sum(tu.cached_input), \
         sum(tu.cache_creation), sum(tu.output) \
         FROM model_call mc JOIN token_usage tu ON tu.call_id = mc.call_id \
         GROUP BY mc.model",
        &[],
    );
    rows.iter()
        .map(|row| {
            let class = |i| row.get_i64(i).unwrap();
            (
                row.get_text(0).unwrap(),
                [class(1), class(2), class(3), class(4)],
            )
        })
        .collect()
}

/// `(text, count)` rows as a map.
fn counted(store: &SqliteStore, sql: &str) -> BTreeMap<String, i64> {
    let rows = select(store, sql, &[]);
    rows.iter()
        .map(|row| (row.get_text(0).unwrap(), row.get_i64(1).unwrap()))
        .collect()
}

/// The one row `sql` selects for `key`.
fn only_row(store: &SqliteStore, sql: &str, key: &str) -> rift_store::Row {
    let rows = select(store, sql, &[Value::text(key)]);
    let [row] = rows.as_slice() else {
        panic!("one row for {key}: {sql}");
    };
    row.clone()
}

/// The store a `rift savings --json` document names under `member`, open.
fn open_named(savings: &serde_json::Value, member: &str) -> SqliteStore {
    let path = savings[member]
        .as_str()
        .unwrap_or_else(|| panic!("savings names its {member}: {savings}"));
    SqliteStore::open_read_only(path).unwrap()
}

/// Every row `sql` selects from `store`.
fn select(store: &SqliteStore, sql: &str, params: &[Value]) -> Vec<rift_store::Row> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(store.query(sql, params)).unwrap()
}

/// The `rift` binary: the sibling of the `rift-memd` cargo built for this
/// test, since cargo names a binary's path only to its own package's tests.
fn rift_bin() -> PathBuf {
    let memd = Path::new(env!("CARGO_BIN_EXE_rift-memd"));
    let rift = memd.with_file_name(format!("rift{}", std::env::consts::EXE_SUFFIX));
    assert!(
        rift.is_file(),
        "no rift binary beside rift-memd at {}: build both (cargo test -p rift -p rift-memd)",
        rift.display()
    );
    rift
}

/// A repository with one commit, Recall declared on, and declines asked for
/// Claude Code in its own policy file.
fn seed_repository(repo: &Path) {
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(repo, &["add", "a.txt"]);
    git(repo, &["commit", "-q", "-m", "seed"]);
    let policies = repo.join(".rift").join("policies");
    std::fs::create_dir_all(&policies).unwrap();
    std::fs::write(
        repo.join(".rift/config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    std::fs::write(
        policies.join("serve.toml"),
        "[interception]\nmode = \"enforce\"\nmin_slice_lines = 1\n\n\
         [interception.agents]\nclaude-code = \"enforce\"\n",
    )
    .unwrap();
}

/// Run `git` in `dir` with a fixed identity and no ambient re-targeting.
fn git(dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join("absent-gitconfig"))
        .env("GIT_AUTHOR_NAME", "rift-test")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "rift-test")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z");
    for key in RETARGETING_ENV {
        cmd.env_remove(key);
    }
    let out = cmd.output().expect("git is on PATH");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
