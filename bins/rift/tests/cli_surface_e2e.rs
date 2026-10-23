//! End-to-end of the `clap` command surface: the exit-code contract, the
//! `--json` output contract, `init` and its opt-outs, the `enable`/`disable`
//! round-trip, `configure splash-mode`, and the retired-brand-never-surfaced
//! guard. The git fixtures' own calibration lives here too, because every
//! repository this suite builds goes through them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::Path;
use std::process::Command;

#[allow(dead_code)] // The CLI suites share these builders; each file uses a subset.
#[path = "common/cli_fixture.rs"]
mod cli_fixture;
mod common;
use cli_fixture::{
    code, git, git_init, hermetic_git, rift, rift_with_detected_agents, RETIRED_BRAND,
};

/// A scratch **repository**: a tempdir with a gitdir hardened discovery
/// accepts.
///
/// Every store `rift` opens is its repository family's, and a
/// family is keyed by a git common dir — so a verb that reads or writes one
/// needs a repository to belong to, exactly as capture always did (the
/// checkpoints are git refs). A bare tempdir exercises the refusal, not the
/// verb.
fn repo_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    rift_git::test_support::fake_repo(dir.path()).unwrap();
    dir
}

#[test]
fn viewer_headless_frame_is_bounded_and_creates_no_state() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    git_init(repo.path());
    let refs_before = git(home.path(), repo.path(), &["for-each-ref"]).stdout;
    let objects_before = git(home.path(), repo.path(), &["count-objects", "-v"]).stdout;
    let cache = home.path().join("cache");
    assert!(!cache.exists());

    let out = common::rift_cmd(repo.path())
        .args(["viewer", "--repo", "."])
        .env("RIFT_CONFIG_DIR", home.path().join("config"))
        .env("RIFT_CACHE_DIR", &cache)
        .env("RIFT_HEADLESS", "1")
        .env("RIFT_HEADLESS_WAIT_MS", "500")
        .output()
        .expect("spawn read-only viewer");
    assert_eq!(code(&out), 0, "viewer exits successfully: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[<]"), "closed project drawer: {stdout}");
    assert!(
        stdout.contains("[Recall viewer]"),
        "selected viewer tab: {stdout}"
    );
    assert!(
        !stdout.contains("─ projects"),
        "drawer starts closed: {stdout}"
    );
    assert!(stdout.contains("checkpoints"), "checkpoint pane: {stdout}");
    assert!(stdout.contains("timeline"), "timeline pane: {stdout}");
    assert!(stdout.lines().count() <= 40, "one bounded frame: {stdout}");
    assert!(!cache.exists(), "an absent cache root remains absent");
    assert_eq!(
        git(home.path(), repo.path(), &["for-each-ref"]).stdout,
        refs_before,
        "viewer must not write refs"
    );
    assert_eq!(
        git(home.path(), repo.path(), &["count-objects", "-v"]).stdout,
        objects_before,
        "viewer must not write objects"
    );
}

/// The scrub above, measured against the two vectors that reach *this* file:
/// one that changes the repository format and one that plants files inside
/// `.git` before any test has run a line of Rift.
///
/// Both arms are otherwise identical and both are hermetic — the only
/// difference is the order of [`hermetic_git`] and the poison, so the arm that
/// shows the vector working is not a differently-configured git. Without the
/// first arm the second would be unfalsifiable: a `sha1` repository proves
/// nothing if `GIT_DEFAULT_HASH` never worked on this git to begin with.
#[test]
fn the_git_helpers_scrub_a_format_change_and_a_planted_template() {
    let home = tempfile::tempdir().unwrap();
    let evil = tempfile::tempdir().unwrap();
    std::fs::write(evil.path().join("planted.txt"), "x\n").unwrap();

    let init = |repo: &Path, poison_first: bool| {
        let mut cmd = Command::new("git");
        cmd.args(["init", "-q"]).current_dir(repo);
        let plant = |c: &mut Command| {
            c.env("GIT_DEFAULT_HASH", "sha256")
                .env("GIT_TEMPLATE_DIR", evil.path());
        };
        if poison_first {
            plant(&mut cmd);
            hermetic_git(&mut cmd, home.path());
        } else {
            hermetic_git(&mut cmd, home.path());
            plant(&mut cmd);
        }
        assert!(cmd.status().expect("spawn git init").success());
        let fmt = git(home.path(), repo, &["rev-parse", "--show-object-format"]);
        String::from_utf8_lossy(&fmt.stdout).trim().to_owned()
    };

    // The vector, working.
    let hostile = tempfile::tempdir().unwrap();
    assert_eq!(
        init(hostile.path(), false),
        "sha256",
        "GIT_DEFAULT_HASH does not reproduce on this git, so the guard below \
         would be vacuous — do not weaken the guard, find out why"
    );
    assert!(
        hostile.path().join(".git").join("planted.txt").exists(),
        "GIT_TEMPLATE_DIR does not reproduce on this git"
    );

    // The same vector, scrubbed.
    let clean = tempfile::tempdir().unwrap();
    assert_eq!(init(clean.path(), true), "sha1", "the format is ours");
    assert!(
        !clean.path().join(".git").join("planted.txt").exists(),
        "a planted template reached .git"
    );

    // And `git_init`, which every other test in this file goes through, pins
    // the format on the argv as well.
    let pinned = tempfile::tempdir().unwrap();
    git_init(pinned.path());
    let fmt = git(
        home.path(),
        pinned.path(),
        &["rev-parse", "--show-object-format"],
    );
    assert_eq!(String::from_utf8_lossy(&fmt.stdout).trim(), "sha1");
}

#[test]
fn init_scaffolds_rift_and_registers() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    git_init(repo.path());
    let agents = [
        "claude",
        "codex",
        "gemini",
        "antigravity",
        "cursor",
        "copilot",
        "opencode",
        "droid",
        "ollama",
    ];
    let out = rift_with_detected_agents(home.path(), repo.path(), &["init"], &agents);
    assert_eq!(code(&out), 0, "init exits 0: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("registered"), "init stdout: {stdout}");
    assert!(repo.path().join(".rift").join("config.toml").exists());
    // All nine agents are on the fixture PATH; **three** get wired. This test
    // asserted all nine until 2026-08-20, when the supported set was cut to
    // four, and three since 2026-08-24, when Gemini CLI was demoted. The two
    // demotions have DIFFERENT reasons and the
    // distinction is kept where the list is: five agents declare verb surfaces
    // the real agent never sends, so a descriptor for them would wire a hook
    // surface nobody calls; Gemini is out of scope by decision, and its
    // gaps are ordinary defects that would be fixed if it were in scope. The
    // exit-0 assertion above is untouched: `init` succeeds, it just does less.
    for native in ["claude-code", "codex", "antigravity"] {
        assert!(
            repo.path()
                .join(".rift/agents")
                .join(format!("{native}.json"))
                .exists(),
            "missing native descriptor for {native}"
        );
    }
    for coming_soon in [
        "gemini",
        "cursor",
        "copilot-cli",
        "opencode",
        "factory-droid",
        "ollama",
    ] {
        assert!(
            !repo
                .path()
                .join(".rift/agents")
                .join(format!("{coming_soon}.json"))
                .exists(),
            "{coming_soon} is not captured by this alpha and must not be wired"
        );
    }
    // `setup` is a hidden alias of `init`.
    assert_eq!(
        code(&rift_with_detected_agents(
            home.path(),
            repo.path(),
            &["setup"],
            &agents,
        )),
        0
    );
}

/// **A coming-soon agent on the PATH does not fail `init`, and does not leave
/// the user guessing.**
///
/// This is the regression that reddened the workspace: `init` selected every
/// *detected* adapter as a default and then refused the unsupported ones one
/// at a time, so `rift init` — the alpha's primary entry command — exited 1 on
/// any machine with Ollama installed. Detection is not eligibility.
///
/// Silence would be the other wrong answer, and is the one this asserts
/// against: a user who installed Cursor and sees nothing about it cannot tell
/// "Rift decided not to" from "Rift did not notice". The exit code and the
/// sentence are pinned together for that reason.
#[test]
fn a_coming_soon_agent_is_named_rather_than_wired_or_fatal() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    git_init(repo.path());
    let out = rift_with_detected_agents(
        home.path(),
        repo.path(),
        &["init", "--no-recall"],
        &["ollama", "cursor"],
    );
    assert_eq!(code(&out), 0, "an unsupported agent must not fail: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("coming soon"),
        "init must say why it left them alone: {stdout}"
    );
    for name in ["Cursor", "Ollama"] {
        assert!(
            stdout.contains(name),
            "the line names {name} rather than summarising a count: {stdout}"
        );
    }
    for token in ["cursor", "ollama"] {
        assert!(
            !repo
                .path()
                .join(".rift/agents")
                .join(format!("{token}.json"))
                .exists(),
            "{token} was named as coming soon and then wired anyway"
        );
    }
}

#[test]
fn no_recall_is_the_explicit_init_opt_out() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    git_init(repo.path());
    let out = rift(home.path(), repo.path(), &["init", "--no-recall"]);
    assert_eq!(code(&out), 0, "init --no-recall exits 0: {out:?}");
    assert!(repo.path().join(".rift/config.toml").exists());
    assert!(!repo.path().join(".mcp.json").exists());
    assert!(!repo.path().join(".codex/config.toml").exists());
    assert!(!repo.path().join(".gemini/settings.json").exists());
    assert!(!repo.path().join(".agents/mcp_config.json").exists());
}

/// `--no-recall` skips Recall and nothing else: Project Settings passes it and
/// provisions Recall itself, so the capture hooks of a detected agent are
/// still wired, and the help says so.
#[test]
fn no_recall_still_wires_the_agents_init_detects() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    git_init(repo.path());
    let out = rift_with_detected_agents(
        home.path(),
        repo.path(),
        &["init", "--no-recall", "--json"],
        &["codex"],
    );
    assert_eq!(code(&out), 0, "init --no-recall exits 0: {out:?}");
    assert!(
        repo.path().join(".rift/agents/codex.json").exists(),
        "the detected agent is wired under --no-recall: {out:?}"
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["recall"]["skipped"], true, "and Recall is skipped");
}

#[test]
fn recall_status_json_has_all_native_adapter_rows() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    git_init(repo.path());
    let init = rift(home.path(), repo.path(), &["init"]);
    assert_eq!(code(&init), 0, "init exits 0: {init:?}");
    let out = rift(home.path(), repo.path(), &["recall", "status", "--json"]);
    assert_eq!(code(&out), 0, "recall status exits 0: {out:?}");
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let integrations = value["integrations"].as_array().unwrap();
    assert_eq!(integrations.len(), 9);
    for integration in integrations {
        for key in ["agent", "display_name", "detection", "wiring"] {
            assert!(
                integration.get(key).is_some(),
                "missing {key}: {integration}"
            );
        }
        assert!(integration.get("mcp").is_none());
        assert!(integration.get("server_probe").is_none());
    }
}

#[test]
fn init_from_a_subdirectory_wires_the_repository_root() {
    let home = tempfile::tempdir().unwrap();
    let outer = tempfile::tempdir().unwrap();
    let repo = outer.path().join("repo");
    let sub = repo.join("a/b");
    std::fs::create_dir_all(&repo).unwrap();
    git_init(&repo);
    std::fs::create_dir_all(&sub).unwrap();
    let out = rift_with_detected_agents(home.path(), &sub, &["init"], &["codex"]);
    assert_eq!(code(&out), 0, "subdirectory init exits 0: {out:?}");
    assert!(repo.join(".rift/agents/codex.json").exists());
    assert!(!sub.join(".rift/agents/codex.json").exists());
}

#[test]
fn query_in_non_rift_repo_exits_not_init() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap(); // no `rift init` here
    let out = rift(home.path(), repo.path(), &["status"]);
    assert_eq!(code(&out), 3, "status in a non-rift repo exits 3");
}

#[test]
fn unknown_subcommand_is_usage_error() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let out = rift(home.path(), repo.path(), &["definitely-not-a-command"]);
    assert_eq!(code(&out), 2, "clap usage error is exit 2");
}

#[test]
fn status_json_is_a_valid_contract() {
    let home = tempfile::tempdir().unwrap();
    let repo = repo_dir();
    rift(home.path(), repo.path(), &["init", "--no-recall"]);
    let out = rift(home.path(), repo.path(), &["status", "--json"]);
    assert_eq!(code(&out), 0);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("status --json is JSON");
    for key in [
        "checkpoints",
        "sessions",
        "captured_events",
        "hook_receipts",
        "transitions",
    ] {
        assert!(
            v.get(key).is_some_and(serde_json::Value::is_i64),
            "missing {key}: {v}"
        );
    }
}

#[test]
fn enable_then_disable_round_trips() {
    let home = tempfile::tempdir().unwrap();
    let repo = repo_dir();
    rift(home.path(), repo.path(), &["init", "--no-recall"]);

    let desc = repo
        .path()
        .join(".rift")
        .join("agents")
        .join("claude-code.json");
    let out = rift(
        home.path(),
        repo.path(),
        &["enable", "--agent", "claude-code"],
    );
    assert_eq!(code(&out), 0, "enable exits 0: {out:?}");
    assert!(desc.exists(), "enable writes the wiring descriptor");
    let descriptor = std::fs::read_to_string(&desc).unwrap();
    assert!(
        !descriptor.to_ascii_lowercase().contains(RETIRED_BRAND),
        "no brand leak in wiring"
    );
    assert!(descriptor.contains("hooks claude-code"));

    // enable is idempotent.
    assert_eq!(
        code(&rift(
            home.path(),
            repo.path(),
            &["enable", "--agent", "claude-code"]
        )),
        0
    );

    let out = rift(
        home.path(),
        repo.path(),
        &["disable", "--agent", "claude-code"],
    );
    assert_eq!(code(&out), 0, "disable exits 0");
    assert!(!desc.exists(), "disable removes the wiring descriptor");
}

/// The write is where a changed hook is said. Codex skips a hook whose
/// recorded trust no longer matches it, silently, so `rift enable` compares
/// what it just wrote against Codex's own record and prints the remedy —
/// naming VS Code because the recorded session ran there — and returns it in
/// `--json`, without touching Codex's configuration.
#[test]
fn enabling_codex_over_a_changed_trusted_hook_says_so_at_the_write() {
    let home = tempfile::tempdir().unwrap();
    let repo = repo_dir();
    record_vscode_codex_session(home.path(), repo.path());
    let codex_home = home.path().join("codex-home");
    let config = trust_a_different_session_start(&codex_home, repo.path());

    let out = rift(home.path(), repo.path(), &["enable", "--agent", "codex"]);
    assert_eq!(code(&out), 0, "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("codex trust: error — Codex is skipping 10 of 10 Rift hooks"),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "Codex is skipping 10 of Rift's hooks because they are new or changed after you \
             approved them"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains("Then reload the VS Code window and start a new Codex conversation."),
        "{stdout}"
    );

    let out = rift(
        home.path(),
        repo.path(),
        &["enable", "--agent", "codex", "--json"],
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(json["codex_trust_remedy"]
        .as_str()
        .unwrap()
        .starts_with("Codex is skipping 10 of Rift's hooks"));
    let verdicts: Vec<(&str, &str)> = json["codex_trust"]["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hook| {
            (
                hook["event"].as_str().unwrap(),
                hook["verdict"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(
        verdicts.contains(&("SessionStart", "modified")),
        "{verdicts:?}"
    );
    assert!(verdicts.contains(&("Stop", "untrusted")), "{verdicts:?}");
    assert_eq!(
        std::fs::read_to_string(codex_home.join("config.toml")).unwrap(),
        config,
        "Rift never writes Codex's configuration"
    );
}

/// A Codex session recorded in `repo`, run by the VS Code extension at a
/// version the trust check is proven against.
fn record_vscode_codex_session(home: &Path, repo: &Path) {
    let rollout = home.join("rollout.jsonl");
    std::fs::write(
        &rollout,
        "{\"type\":\"session_meta\",\"payload\":{\"originator\":\"codex_vscode\",\
         \"source\":\"vscode\",\"cli_version\":\"0.155.0-alpha.16.3\"}}\n",
    )
    .unwrap();
    let sessions = common::seed_sessions_dir(&repo.join(".git"));
    std::fs::create_dir_all(&sessions).unwrap();
    let cache = serde_json::json!({
        "schema": 1, "session_id": "s1", "state": "ended", "agent": "codex",
        "current_turn_ordinal": 0, "compaction_boundary": null, "updated_at_ms": 1,
        "transcript_path": rollout.display().to_string(),
    });
    std::fs::write(sessions.join("s1.json"), cache.to_string()).unwrap();
}

/// Codex's record of a `SessionStart` hook trusted as something other than
/// what Rift writes, keyed by the path Codex resolves (the canonical one).
/// Returns the configuration's text.
fn trust_a_different_session_start(codex_home: &Path, repo: &Path) -> String {
    std::fs::create_dir_all(codex_home).unwrap();
    let hooks_json = std::fs::canonicalize(repo)
        .unwrap()
        .join(".codex/hooks.json");
    let config = format!(
        "[hooks.state.\"{}:session_start:0:0\"]\ntrusted_hash = \"sha256:{}\"\n",
        hooks_json.display().to_string().replace('\\', "\\\\"),
        "0".repeat(64)
    );
    std::fs::write(codex_home.join("config.toml"), &config).unwrap();
    config
}

#[test]
fn enable_unknown_agent_fails_cleanly() {
    let home = tempfile::tempdir().unwrap();
    let repo = repo_dir();
    rift(home.path(), repo.path(), &["init", "--no-recall"]);
    let out = rift(
        home.path(),
        repo.path(),
        &["enable", "--agent", "not-an-agent"],
    );
    assert_eq!(
        code(&out),
        4,
        "no adapter for the agent is a precondition failure (exit 4)"
    );
}

#[test]
fn help_never_mentions_the_retired_brand() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let out = rift(home.path(), repo.path(), &["--help"]);
    let text = String::from_utf8_lossy(&out.stdout).to_ascii_lowercase();
    assert!(
        !text.contains(RETIRED_BRAND),
        "help leaks the backend brand: {text}"
    );
    // The hidden `hooks` subcommand must not be listed as its own command line
    // (it may still be referenced inside another command's description).
    assert!(
        !text
            .lines()
            .any(|l| l.trim_start().starts_with("hooks ") || l.trim_start() == "hooks"),
        "hidden hooks surfaced as a subcommand: {text}"
    );
}

#[test]
fn hooks_schema_emits_event_vocabulary() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let out = rift(home.path(), repo.path(), &["hooks", "--schema"]);
    assert_eq!(code(&out), 0);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("schema is JSON");
    assert!(v["event_kinds"]
        .as_array()
        .is_some_and(|a| a.iter().any(|k| k == "tool_call")));
    assert!(v.get("schema_version").is_some());
}

#[test]
fn gc_reports_only_transient_and_never_the_chain() {
    let home = tempfile::tempdir().unwrap();
    let repo = repo_dir();
    rift(home.path(), repo.path(), &["init", "--no-recall"]);
    let out = rift(home.path(), repo.path(), &["gc", "--json"]);
    assert_eq!(code(&out), 0);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("gc --json");
    assert!(v
        .get("prunable_transient_refs")
        .is_some_and(serde_json::Value::is_array));
}

// --- `rift configure splash-mode` --------------------------------------------

/// The global canopy-prefs file under the hermetic `RIFT_CONFIG_DIR`.
fn canopy_prefs_path(home: &Path) -> std::path::PathBuf {
    home.join("config").join("rift").join("canopy.toml")
}

#[test]
fn configure_splash_mode_round_trips_and_survives_relaunch() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();

    // Set: the preference lands in the global canopy prefs (never `.rift/`).
    let out = rift(
        home.path(),
        repo.path(),
        &["configure", "splash-mode", "never"],
    );
    assert_eq!(code(&out), 0, "configure splash-mode exits 0: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("never"), "confirms the mode: {stdout}");
    let prefs = std::fs::read_to_string(canopy_prefs_path(home.path())).unwrap();
    assert!(prefs.contains("splash_mode = \"never\""), "prefs: {prefs}");
    assert!(
        !repo.path().join(".rift").exists(),
        "a global preference never scaffolds the repo"
    );

    // The choice survives across launches: a second process starts from the
    // persisted file and supersedes only the splash key.
    let out = rift(
        home.path(),
        repo.path(),
        &["configure", "splash-mode", "always", "--json"],
    );
    assert_eq!(code(&out), 0);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("--json is JSON");
    assert_eq!(v["splash_mode"], "always");
    let prefs = std::fs::read_to_string(canopy_prefs_path(home.path())).unwrap();
    assert!(prefs.contains("splash_mode = \"always\""), "prefs: {prefs}");

    // And the default spelling round-trips too.
    let out = rift(
        home.path(),
        repo.path(),
        &["configure", "splash-mode", "first-run"],
    );
    assert_eq!(code(&out), 0);
    let prefs = std::fs::read_to_string(canopy_prefs_path(home.path())).unwrap();
    assert!(
        prefs.contains("splash_mode = \"first-run\""),
        "prefs: {prefs}"
    );
}

#[test]
fn configure_splash_mode_writes_only_the_splash_key() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();

    // Pre-seed every other preference away from its default.
    let path = canopy_prefs_path(home.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "seen_splash = true\nsplash_mode = \"first-run\"\nreduced_motion = true\nmouse = false\n",
    )
    .unwrap();

    let out = rift(
        home.path(),
        repo.path(),
        &["configure", "splash-mode", "never"],
    );
    assert_eq!(code(&out), 0);

    let prefs = std::fs::read_to_string(&path).unwrap();
    assert!(prefs.contains("splash_mode = \"never\""), "prefs: {prefs}");
    // Every other key rides through the superseding write untouched.
    assert!(prefs.contains("seen_splash = true"), "prefs: {prefs}");
    assert!(prefs.contains("reduced_motion = true"), "prefs: {prefs}");
    assert!(prefs.contains("mouse = false"), "prefs: {prefs}");
}

#[test]
fn configure_splash_mode_rejects_unknown_values() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let out = rift(
        home.path(),
        repo.path(),
        &["configure", "splash-mode", "sometimes"],
    );
    assert_eq!(code(&out), 2, "an unknown mode is a clap usage error");
    assert!(
        !canopy_prefs_path(home.path()).exists(),
        "nothing is written on a usage error"
    );
}
