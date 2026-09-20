//! End-to-end of the `clap` command surface: exit-code contract, the
//! `--json` output contract, `init` + the read-only query commands, the
//! `enable`/`disable` round-trip, and the retired-brand-never-surfaced guard.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::Path;
use std::process::{Command, Output};

mod common;

const RETIRED_BRAND: &str = concat!("ent", "ire");

/// Run `rift <args>` hermetically (config + cache redirected; offline; headless).
fn rift(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    rift_with_detected_agents(home, cwd, args, &[])
}

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

/// Run Rift with a deterministic set of compatible agent executables visible.
/// Native detection only requires a project-local executable-shaped file; the
/// fixtures are never invoked.
fn rift_with_detected_agents(home: &Path, cwd: &Path, args: &[&str], binaries: &[&str]) -> Output {
    let agent_bin = home.join("agent-bin");
    std::fs::create_dir_all(&agent_bin).expect("create agent fixture directory");
    for binary in binaries {
        std::fs::write(agent_bin.join(binary), "fixture\n").expect("write agent fixture");
    }
    let mut paths = vec![agent_bin];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(paths).expect("join fixture PATH");
    common::rift_cmd(cwd)
        .args(args)
        .env("RIFT_CONFIG_DIR", home.join("config"))
        .env("RIFT_CACHE_DIR", home.join("cache"))
        .env("RIFT_HEADLESS", "1")
        .env("PATH", path)
        .output()
        .expect("spawn rift")
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

/// Inherited `GIT_*` variables that silently re-target or re-configure a child
/// git — the same list `crates/rift-shadow-attribution/tests/support/mod.rs`
/// scrubs from its rewrite fixtures, repeated here because the two live in
/// different crates' test trees and a `dev-dependency` between them would be a
/// new edge for a constant.
///
/// **`GIT_DEFAULT_HASH` is the load-bearing one in this file**, and it was open
/// here while the attribution fixtures closed it: it changes the repository
/// *format*, so `blame_answers_a_sha256_repository_rather_than_reporting_it_empty`
/// — a test whose whole subject is oid width — would otherwise have been
/// measuring the developer's environment, and every other repository built here
/// would silently have become sha256, which `rift init` cannot open at all.
const SCRUBBED: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_COMMON_DIR",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_TEMPLATE_DIR",
    "GIT_EXTERNAL_DIFF",
    "GIT_SSH_COMMAND",
    "GIT_DEFAULT_HASH",
];

/// Variables whose protective state is a **value**, not absence.
///
/// `GIT_CONFIG_COUNT=0` is the total form of scrubbing the unbounded
/// `GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` pairs, which outrank every config
/// file; `GIT_ATTR_NOSYSTEM=1` disables `$(prefix)/etc/gitattributes`, so
/// removing it would be backwards.
const PINNED_OFF: &[(&str, &str)] = &[
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_ATTR_NOSYSTEM", "1"),
    ("GIT_CONFIG_COUNT", "0"),
    ("GIT_TERMINAL_PROMPT", "0"),
];

/// Make a `git` invocation independent of the environment it was launched from.
///
/// `home` is a throwaway root this command may treat as `$HOME`; the config
/// files under it need not exist, and git ignores the ones that do not.
fn hermetic_git(cmd: &mut Command, home: &Path) {
    let cfg = home.join("no-such-gitconfig");
    for key in SCRUBBED {
        cmd.env_remove(key);
    }
    for (k, v) in PINNED_OFF {
        cmd.env(k, v);
    }
    cmd.env("GIT_CONFIG_GLOBAL", &cfg)
        .env("GIT_CONFIG_SYSTEM", &cfg)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home)
        .env("GIT_ASKPASS", "")
        .env("GIT_EDITOR", "true");
}

/// `--object-format=sha1` is pinned on the argv as well as scrubbed from the
/// environment: scrubbing alone leaves the fixture at the mercy of git's own
/// default changing later, and every test in this file that is not *about*
/// sha256 assumes a repository `rift init` can open.
fn git_init(path: &Path) {
    let mut cmd = Command::new("git");
    cmd.args(["init", "-q", "--object-format=sha1"])
        .current_dir(path);
    hermetic_git(&mut cmd, &path.join("no-such-home"));
    let status = cmd.status().expect("spawn git init");
    assert!(status.success());
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
    assert!(stdout.contains("projects"), "projects rail: {stdout}");
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

// ---------------------------------------------------------------------------
// `rift blame` against real git repositories.
//
// The renderer's own tests (bins/rift/src/provenance.rs) are the assurance for
// what reaches a terminal; these are the assurance for the two properties of
// *git's* behaviour that the porcelain parser depends on, plus the route
// itself, which no test exercised at all until now.
// ---------------------------------------------------------------------------

/// A git invocation that cannot be steered by the developer's own config *or*
/// environment: no system file, no global file, identity and signing pinned on
/// the argv, and the re-targeting `GIT_*` variables scrubbed by [`hermetic_git`]
/// — including `GIT_TEMPLATE_DIR`, which would otherwise run a hostile hook
/// inside every `init` and `commit` below.
fn git(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new("git");
    cmd.args([
        "-c",
        "user.name=Rift Fixture",
        "-c",
        "user.email=fixture@rift.invalid",
        "-c",
        "commit.gpgsign=false",
    ])
    .args(args)
    .current_dir(cwd);
    hermetic_git(&mut cmd, home);
    cmd.output().expect("spawn git")
}

/// `rift blame` only needs `.rift/` to exist (the init guard) and creates its
/// own projection. It is made directly rather than by `rift init` because
/// `init` runs the initial recall ingest, which opens the repository through
/// `gix` — and **`gix` cannot open a sha256 repository at all** ("Failed to
/// load the git configuration"), so `rift init` exits 1 there.
///
/// **This is a workaround for a live defect, said out loud so it is not
/// mistaken for a test convenience.** Where the fault lies: `gix` owns the
/// inability (0.78.0's `try_into_object_format` accepts `sha1` only, and lenient
/// mode does not guard it), Rift owns the *fatality* — `bins/rift/src/features.rs`
/// propagates the ingest failure with `?` and kills the whole init, so an
/// optional plane failing takes the product down. Init must degrade recall to
/// off with a named warning and continue; that fix is owed and not yet made.
/// Until it lands, a developer in a sha256
/// repository can be answered by `blame` but cannot run `rift init` — which is
/// exactly why these tests build `.rift/` themselves rather than quietly
/// skipping the case.
/// Opt the fixture into the query surface: `.rift/` for its configuration,
/// and the family projection under the cache root [`rift`] hands the binary —
/// which is what "initialized on this machine" means, `.rift/` alone being
/// committed configuration that proves nothing about a machine.
fn rift_dir(home: &Path, repo: &Path) {
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    common::provision_here(repo, &home.join("cache"));
}

/// `git commit` with both dates pinned, so the commit oid — and therefore any
/// golden that contains it — is a constant of the fixture, not of the wall
/// clock. Everything else routes through [`git`]/[`hermetic_git`] unchanged.
fn git_commit_at(home: &Path, cwd: &Path, date: &str, msg: &str) -> Output {
    let mut cmd = Command::new("git");
    cmd.args([
        "-c",
        "user.name=Rift Fixture",
        "-c",
        "user.email=fixture@rift.invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-q",
        "-m",
        msg,
    ])
    .current_dir(cwd)
    .env("GIT_AUTHOR_DATE", date)
    .env("GIT_COMMITTER_DATE", date);
    hermetic_git(&mut cmd, home);
    cmd.output().expect("spawn git")
}

/// `--line` end-to-end, and **the locked `--json` contract for `rift blame`**
/// — the agent-facing contract for the verb.
///
/// The fixture commit has both git dates pinned, so its oid is a constant and
/// the whole `--json` document is byte-comparable: the golden below IS the
/// agent-facing contract, and any shape change — a renamed key, a moved field,
/// a type change — reds this test until the change is deliberate
/// (`schema_version` bumps on breaking changes per the envelope's rule).
#[test]
fn blame_line_answers_true_numbers_and_the_json_contract_is_locked() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    assert!(git(home.path(), repo.path(), &["init", "-q", "."])
        .status
        .success());
    std::fs::write(repo.path().join("f.rs"), "a\nb\nc\n").unwrap();
    assert!(git(home.path(), repo.path(), &["add", "-A"])
        .status
        .success());
    assert!(git_commit_at(
        home.path(),
        repo.path(),
        "2026-01-02T03:04:05 +0000",
        "seed"
    )
    .status
    .success());
    rift_dir(home.path(), repo.path());

    // The ranged human render speaks true file numbers, not excerpt numbers.
    let out = rift(
        home.path(),
        repo.path(),
        &["blame", "f.rs", "--line", "2-3"],
    );
    assert_eq!(code(&out), 0, "{out:?}");
    let human = String::from_utf8_lossy(&out.stdout);
    assert!(human.contains("--line 2-3"), "{human}");
    assert!(human.contains("2 lines across 1 commit;"), "{human}");
    assert!(
        !human.contains("1-3"),
        "excerpt renumbering leaked: {human}"
    );

    // The locked contract, byte-for-byte.
    let json = rift(
        home.path(),
        repo.path(),
        &["blame", "f.rs", "--line", "2", "--json"],
    );
    assert_eq!(code(&json), 0, "{json:?}");
    let got: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let expected = serde_json::json!({
        "schema_version": 1,
        "file": "f.rs",
        "lines": 1,
        "range": { "start": 2, "end": 2 },
        "spans": [{
            "start": 2,
            "end": 2,
            "lines": 1,
            "commit": "eca66ff1b988af6ed2654840c0ef0a4aa1559c50",
            "path": "f.rs",
            "tag": "unknown",
            "selection": null,
            "attribution": null,
            "sessions": [],
        }],
        "commits": {
            "eca66ff1b988af6ed2654840c0ef0a4aa1559c50": {
                "claims": [],
                "rejected": 0,
                "truncated": false,
            },
        },
        "checkpoints": {},
        "summary": {
            "commits": 1,
            "checkpoints": 0,
            "lines_with_file_evidence": 0,
        },
        "reads": { "messages": 1, "facts": 0, "counts": 0 },
    });
    assert_eq!(got, expected, "the rift blame --json contract moved");

    // A range past the end refuses with the range named — never a guess.
    let past = rift(
        home.path(),
        repo.path(),
        &["blame", "f.rs", "--line", "9-12", "--json"],
    );
    assert_eq!(code(&past), 4, "PRECONDITION: {past:?}");
    let v: serde_json::Value = serde_json::from_slice(&past.stdout).unwrap();
    assert_eq!(v["error"], "not_blameable");
    assert_eq!(v["line"], "9-12");

    // A malformed spec is a usage error before git is ever asked.
    let bad = rift(
        home.path(),
        repo.path(),
        &["blame", "f.rs", "--line", "3-1"],
    );
    assert_ne!(code(&bad), 0, "{bad:?}");
}

/// `rift why` end-to-end, and **the locked `--json` contract**: the colon spec
/// answers one line, the flag spelling answers
/// identically, a range is refused with the single-line sentence, and the
/// whole document is byte-comparable over the date-pinned fixture — including
/// the `file:` handle, whose hash is the sha256 of the fixture's exact bytes.
#[test]
fn why_answers_one_line_and_the_json_contract_is_locked() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    assert!(git(home.path(), repo.path(), &["init", "-q", "."])
        .status
        .success());
    std::fs::write(repo.path().join("f.rs"), "a\nb\nc\n").unwrap();
    assert!(git(home.path(), repo.path(), &["add", "-A"])
        .status
        .success());
    assert!(git_commit_at(
        home.path(),
        repo.path(),
        "2026-01-02T03:04:05 +0000",
        "seed"
    )
    .status
    .success());
    rift_dir(home.path(), repo.path());

    let json = rift(home.path(), repo.path(), &["why", "f.rs:2", "--json"]);
    assert_eq!(code(&json), 0, "{json:?}");
    let got: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let expected = serde_json::json!({
        "schema_version": 1,
        "file": "f.rs",
        "line": 2,
        "span": {
            "start": 2,
            "end": 2,
            "commit": "eca66ff1b988af6ed2654840c0ef0a4aa1559c50",
            "path": "f.rs",
        },
        "tag": "unknown",
        "selection": null,
        "attribution": null,
        "sessions": [],
        "checkpoint": null,
        "prompt": null,
        "cost": null,
        "handles": {
            "file": "file:f.rs@sha256:880553fca8fcea94e325ee2cfb48e5a985cc797f39a14cc6d3cedecfeb2ae4d2",
        },
        "reads": { "messages": 1, "facts": 0, "counts": 0 },
    });
    assert_eq!(got, expected, "the rift why --json contract moved");

    // The flag spelling answers the same line the colon spelling did.
    let flagged = rift(
        home.path(),
        repo.path(),
        &["why", "f.rs", "--line", "2", "--json"],
    );
    let same: serde_json::Value = serde_json::from_slice(&flagged.stdout).unwrap();
    assert_eq!(same, expected, "colon and flag spellings must agree");

    // The human render teaches the policy-gated ladder.
    let human = rift(home.path(), repo.path(), &["why", "f.rs:2"]);
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("recall retrieve-exact"), "{text}");

    // A range is a refusal with the single-line sentence, in both spellings.
    for args in [
        &["why", "f.rs:2-3"] as &[&str],
        &["why", "f.rs", "--line", "2-3"],
    ] {
        let out = rift(home.path(), repo.path(), args);
        assert_ne!(code(&out), 0, "{out:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("single line"), "{err}");
    }
}

/// `rift checkpoint explain`'s refusal contracts, locked: no claimed
/// checkpoint at HEAD, an unknown id, and the flag exclusion — each says
/// exactly why, in the machine shape an agent scripts against. (The
/// affirmative render over a REAL shard is pinned in `hooks_archive_e2e`, where an
/// archive exists to explain.)
#[test]
fn checkpoint_explain_refuses_cleanly_without_a_checkpoint() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    assert!(git(home.path(), repo.path(), &["init", "-q", "."])
        .status
        .success());
    std::fs::write(repo.path().join("f.rs"), "a\n").unwrap();
    assert!(git(home.path(), repo.path(), &["add", "-A"])
        .status
        .success());
    assert!(git_commit_at(
        home.path(),
        repo.path(),
        "2026-01-02T03:04:05 +0000",
        "seed"
    )
    .status
    .success());
    rift_dir(home.path(), repo.path());

    let out = rift(
        home.path(),
        repo.path(),
        &["checkpoint", "explain", "--json"],
    );
    assert_eq!(code(&out), 4, "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v,
        serde_json::json!({ "error": "no_checkpoint", "target": "HEAD" })
    );

    let miss = rift(
        home.path(),
        repo.path(),
        &["checkpoint", "explain", "deadbeef1", "--json"],
    );
    assert_eq!(code(&miss), 4);
    let m: serde_json::Value = serde_json::from_slice(&miss.stdout).unwrap();
    assert_eq!(m["error"], "no_checkpoint");
    assert_eq!(m["target"], "deadbeef1");

    let both = rift(
        home.path(),
        repo.path(),
        &["checkpoint", "explain", "--full", "--short"],
    );
    assert_ne!(code(&both), 0, "{both:?}");
}

/// A provenance tool that silently answers "nothing" is worse than one that
/// errors, and that is what a sha256 repository used to get: the parser
/// accepted 40-character oids only, so every porcelain header missed and a
/// three-line, one-commit file printed "0 lines across 0 commits".
#[test]
fn blame_answers_a_sha256_repository_rather_than_reporting_it_empty() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let init = git(
        home.path(),
        repo.path(),
        &["init", "-q", "--object-format=sha256", "."],
    );
    assert!(
        init.status.success(),
        "git must support --object-format=sha256 (2.29+): {init:?}"
    );
    std::fs::write(repo.path().join("f.rs"), "a\nb\nc\n").unwrap();
    assert!(git(home.path(), repo.path(), &["add", "-A"])
        .status
        .success());
    assert!(
        git(home.path(), repo.path(), &["commit", "-q", "-m", "seed"])
            .status
            .success()
    );
    rift_dir(home.path(), repo.path());

    let out = rift(home.path(), repo.path(), &["blame", "f.rs"]);
    assert_eq!(code(&out), 0, "blame exits 0: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("3 lines across 1 commit;"),
        "sha256 blame: {stdout}"
    );
    // The oid is 64 wide and the span is real, not the all-zero placeholder.
    let json = rift(home.path(), repo.path(), &["blame", "f.rs", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let commit = v["spans"][0]["commit"].as_str().unwrap().to_owned();
    assert_eq!(commit.len(), 64, "sha256 oid width");
    assert!(commit.bytes().any(|b| b != b'0'), "not the null oid");
    assert_eq!(v["lines"], 3);
}

/// What `core.quotePath=false` buys, stated as a fact about the output.
///
/// git's default c-quotes every byte ≥ 0x80, so `café.rs` arrives from the
/// porcelain as `"caf\303\251.rs"` — which matches nothing the projection
/// recorded, so every non-ASCII path silently loses its file evidence and
/// degrades to a guess. The path this verb reports is the one the repository
/// itself spells, and `git ls-files` is asked what that is rather than assumed,
/// because macOS may store it in a different normal form than the argv used.
#[test]
fn blame_reports_a_non_ascii_path_as_the_repository_spells_it() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    assert!(git(home.path(), repo.path(), &["init", "-q", "."])
        .status
        .success());
    std::fs::write(repo.path().join("café.rs"), "x\n").unwrap();
    assert!(git(home.path(), repo.path(), &["add", "-A"])
        .status
        .success());
    assert!(
        git(home.path(), repo.path(), &["commit", "-q", "-m", "seed"])
            .status
            .success()
    );
    rift_dir(home.path(), repo.path());

    let listed = git(
        home.path(),
        repo.path(),
        &["-c", "core.quotePath=false", "ls-files"],
    );
    let recorded = String::from_utf8_lossy(&listed.stdout).trim().to_owned();
    let out = rift(home.path(), repo.path(), &["blame", &recorded, "--json"]);
    assert_eq!(code(&out), 0, "blame exits 0: {out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let path = v["spans"][0]["path"].as_str().unwrap();
    assert_eq!(path, recorded, "the repository's own spelling");
    assert!(!path.starts_with('"'), "not c-quoted: {path}");
    assert!(!path.contains("\\3"), "no octal escapes: {path}");
}

/// The code points `paths_contain` cannot tell from nothing — exactly
/// `char::is_whitespace`, and exactly the trailing-collision class swept in
/// `bins/rift/src/provenance.rs`.
#[cfg(unix)]
const COLLIDING_WHITESPACE: [char; 25] = [
    '\u{9}', '\u{a}', '\u{b}', '\u{c}', '\u{d}', '\u{20}', '\u{85}', '\u{a0}', '\u{1680}',
    '\u{2000}', '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}',
    '\u{2008}', '\u{2009}', '\u{200a}', '\u{2028}', '\u{2029}', '\u{202f}', '\u{205f}', '\u{3000}',
];

/// **The cost of `core.quotePath=false`, made executable.** `blame` passes that
/// flag so a non-ASCII path stops losing its file evidence; the flag is also
/// what lets 19 of the 25 matcher-collision code points reach the porcelain
/// parser raw. The first version of this note said the opposite — that those 19
/// predated the verb — so the number is a test now rather than a sentence.
///
/// Measured here on whatever git runs the suite, both settings, one repository:
///
/// | | raw at the parser |
/// |---|---|
/// | `core.quotePath=true` (git's default) | 1 — `U+0020` |
/// | `core.quotePath=false` (what `blame` passes) | 20 |
/// | reachable *only* because of the flag | **19** |
///
/// The five that never arrive raw are the C0 whitespace, which git c-quotes
/// whatever the setting. Unix only: `TAB`, `LF` and `CR` cannot be filenames on
/// Windows, so the sweep could only be partial there.
#[cfg(unix)]
#[test]
fn the_quote_path_flag_this_verb_adds_is_what_makes_nineteen_collisions_reachable() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    assert!(git(home.path(), repo.path(), &["init", "-q", "."])
        .status
        .success());

    // One directory per name. On a normalization-insensitive filesystem (APFS)
    // `U+2000` folds onto `U+2002` and `U+2001` onto `U+2003` inside a single
    // directory, so two of the 25 would silently become one file and the sweep
    // would be measuring the filesystem instead of git.
    let mut names = Vec::new();
    for (i, c) in COLLIDING_WHITESPACE.iter().enumerate() {
        let dir = format!("d{i:02}");
        std::fs::create_dir(repo.path().join(&dir)).unwrap();
        let name = format!("{dir}/a.rs{c}");
        std::fs::write(repo.path().join(&name), "one\n").unwrap();
        names.push((*c, name));
    }
    assert!(git(home.path(), repo.path(), &["add", "-A"])
        .status
        .success());
    assert!(
        git(home.path(), repo.path(), &["commit", "-q", "-m", "seed"])
            .status
            .success()
    );

    // The value the parser would see, taken from git rather than predicted.
    let header = |setting: &str, name: &str| -> String {
        let flag = format!("core.quotePath={setting}");
        let out = git(
            home.path(),
            repo.path(),
            &["-c", &flag, "blame", "--line-porcelain", "--", name],
        );
        assert!(out.status.success(), "blame {name:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("filename ").map(ToOwned::to_owned))
            .expect("a filename header")
    };
    let raw_under = |setting: &str| -> Vec<char> {
        names
            .iter()
            .filter(|(_, n)| header(setting, n) == *n)
            .map(|(c, _)| *c)
            .collect()
    };

    let by_default = raw_under("true");
    let with_the_flag = raw_under("false");
    assert_eq!(
        by_default,
        vec!['\u{20}'],
        "git's default lets exactly one of the 25 through raw"
    );
    assert_eq!(
        with_the_flag.len(),
        20,
        "the flag lets 20 through: {with_the_flag:?}"
    );
    let created_here: Vec<char> = with_the_flag
        .iter()
        .copied()
        .filter(|c| !by_default.contains(c))
        .collect();
    assert_eq!(
        created_here.len(),
        19,
        "19 of the 20 are reachable only because this verb passes the flag: \
         {created_here:?}"
    );

    // The five that are never raw are the C0 whitespace, and they are *quoted*
    // rather than merely different — asserted positively, because "not equal to
    // the name" would also be true if git had mangled them some other way.
    let never_raw: Vec<char> = COLLIDING_WHITESPACE
        .iter()
        .copied()
        .filter(|c| !with_the_flag.contains(c))
        .collect();
    assert_eq!(never_raw, vec!['\u{9}', '\u{a}', '\u{b}', '\u{c}', '\u{d}']);
    for (c, name) in &names {
        for setting in ["true", "false"] {
            let got = header(setting, name);
            if never_raw.contains(c) {
                assert!(
                    got.starts_with('"') && got.ends_with('"'),
                    "U+{:04X} under {setting}: {got:?}",
                    *c as u32
                );
            }
        }
    }
}

/// The dependency the backslash rule rests on, asserted against git rather
/// than read out of its documentation.
///
/// The parser refuses a `"` or a `\` in an **unquoted** `filename` value on the
/// grounds that git escapes both unconditionally — the same standing it gives
/// the C0 class, and the reason it is not an asymmetry any more. If that ever
/// stopped being true, the refusal would start costing real evidence silently,
/// so it is measured under both settings of the flag `blame` passes.
///
/// The backslash is not only a framing question: the matcher folds `\` to `/`,
/// so an unquoted `src/b\c.rs` would read as the different, real `src/b/c.rs`.
#[cfg(unix)]
#[test]
fn git_escapes_a_quote_and_a_backslash_whatever_core_quote_path_says() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    assert!(git(home.path(), repo.path(), &["init", "-q", "."])
        .status
        .success());
    std::fs::create_dir(repo.path().join("src")).unwrap();
    for name in ["src/b\\c.rs", "src/d\"e.rs", "src/plain.rs"] {
        std::fs::write(repo.path().join(name), "one\n").unwrap();
    }
    assert!(git(home.path(), repo.path(), &["add", "-A"])
        .status
        .success());
    assert!(
        git(home.path(), repo.path(), &["commit", "-q", "-m", "seed"])
            .status
            .success()
    );

    for setting in ["true", "false"] {
        let flag = format!("core.quotePath={setting}");
        let header = |name: &str| -> String {
            let out = git(
                home.path(),
                repo.path(),
                &["-c", &flag, "blame", "--line-porcelain", "--", name],
            );
            assert!(out.status.success(), "blame {name:?}: {out:?}");
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .find_map(|l| l.strip_prefix("filename ").map(ToOwned::to_owned))
                .expect("a filename header")
        };
        assert_eq!(
            header("src/b\\c.rs"),
            "\"src/b\\\\c.rs\"",
            "a literal backslash under core.quotePath={setting}"
        );
        assert_eq!(
            header("src/d\"e.rs"),
            "\"src/d\\\"e.rs\"",
            "a literal quote under core.quotePath={setting}"
        );
        // The control: an ordinary name is not quoted, so "everything is
        // quoted" is not what the two assertions above are observing.
        assert_eq!(header("src/plain.rs"), "src/plain.rs");
    }
}

/// The parse boundary's one dependency on git, asserted against git.
///
/// A filename is attacker-controlled in any repository you clone. This one is
/// *named* a newline followed by a plausible blame row, so if the porcelain
/// carried it raw the name would spell a second `filename` header of its own —
/// choosing the path that file evidence is matched against — and if the header
/// carried it raw the row would print above the real ones. git c-quotes a
/// pathname holding a control character unconditionally (`core.quotePath`
/// governs only bytes ≥ 0x80), which is what makes the first half safe, and
/// `safe_path` makes the second half safe. Unix only: a name like this cannot
/// exist on Windows, so there the test could only be unfalsifiable.
///
/// This one pins the **outcome**; the test after it pins the **dependency**.
/// The distinction is not pedantry: this name ends in an `ESC`, so the parser's
/// own fail-safe fires and every assertion below would still hold under a git
/// that had stopped quoting.
#[cfg(unix)]
#[test]
fn a_filename_that_spells_a_blame_row_forges_neither_a_row_nor_a_header() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    assert!(git(home.path(), repo.path(), &["init", "-q", "."])
        .status
        .success());
    let evil = "a.rs\n          1-9  0123456789ab  ckpt cafecafecafe  ai  \
                trusted-model  conf 1.000  file-touched\nfilename hijacked.rs\u{1b}[31m";
    std::fs::write(repo.path().join(evil), "one\n").unwrap();
    assert!(git(home.path(), repo.path(), &["add", "-A"])
        .status
        .success());
    assert!(
        git(home.path(), repo.path(), &["commit", "-q", "-m", "seed"])
            .status
            .success()
    );
    rift_dir(home.path(), repo.path());

    let out = rift(home.path(), repo.path(), &["blame", "--", evil]);
    assert_eq!(code(&out), 0, "blame exits 0: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // One header, one span (the file has one line), one summary — and the
    // trailing newline `emit` adds. A forged row can only exist as an extra.
    assert_eq!(stdout.lines().count(), 3, "forged row: {stdout:?}");
    assert!(!stdout.contains('\u{1b}'), "raw ESC: {stdout:?}");
    assert!(
        stdout.contains("1 line across 1 commit;"),
        "one real line: {stdout}"
    );
    // The header quotes the name; the hijacked path never became one.
    assert!(stdout.starts_with("rift blame \"a.rs\\n"), "{stdout:?}");
    // `--json` before the `--`: after it, clap reads it as a second path.
    let json = rift(home.path(), repo.path(), &["blame", "--json", "--", evil]);
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let spans = v["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 1, "one span, not one per forged header");
    assert_ne!(spans[0]["path"], "hijacked.rs");
}

/// The same threat with the fail-safe taken away, so only git's quoting is left.
///
/// Every segment of this name after the newline is ordinary text, so the
/// parser's control-character check cannot fire on it at all: if git handed the
/// name over raw, the porcelain would carry a second, fully ordinary-looking
/// `filename hijacked.rs` header, and the span would be matched against
/// `hijacked.rs` — a path the attacker chose, read as recorded evidence. What
/// prevents that is that git c-quotes the **whole** pathname onto one line, and
/// that is what this asserts: the span carries the quoted spelling, exactly.
///
/// The three outcomes are distinguishable, which is the point: `"b.rs\nfilename
/// hijacked.rs"` (quoted, today), `b.rs` (unquoted, split at the newline) and
/// `hijacked.rs` (unquoted, second header wins). Only the first passes.
#[cfg(unix)]
#[test]
fn the_span_path_depends_on_git_quoting_the_whole_name_not_on_the_fail_safe() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    assert!(git(home.path(), repo.path(), &["init", "-q", "."])
        .status
        .success());
    let evil = "b.rs\nfilename hijacked.rs";
    std::fs::write(repo.path().join(evil), "one\n").unwrap();
    assert!(git(home.path(), repo.path(), &["add", "-A"])
        .status
        .success());
    assert!(
        git(home.path(), repo.path(), &["commit", "-q", "-m", "seed"])
            .status
            .success()
    );
    rift_dir(home.path(), repo.path());

    let out = rift(home.path(), repo.path(), &["blame", "--json", "--", evil]);
    assert_eq!(code(&out), 0, "blame exits 0: {out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let spans = v["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 1, "one span, not one per forged header");
    assert_eq!(
        spans[0]["path"], "\"b.rs\\nfilename hijacked.rs\"",
        "git must c-quote the whole name onto one porcelain line"
    );
    assert_ne!(spans[0]["path"], "hijacked.rs", "the attacker's path");
    assert_ne!(spans[0]["path"], "b.rs", "the truncated path");
    // And the human render is still three lines with nothing raw in them.
    let human = rift(home.path(), repo.path(), &["blame", "--", evil]);
    let stdout = String::from_utf8_lossy(&human.stdout);
    assert_eq!(stdout.lines().count(), 3, "forged row: {stdout:?}");
}

/// A C1 control is not a broken frame, and treating it as one threw evidence
/// away.
///
/// `core.quotePath=false` asks git to hand back bytes ≥ 0x80 raw — that is the
/// whole reason `café.rs` resolves — and `U+0080..=U+009F` are ≥ 0x80. The
/// parser used to test `char::is_control`, which is true for them, so a path git
/// had spelled correctly and in full lost its name and every span of that file
/// degraded to `path-excluded (guess)`. Fail-safe in direction, wrong about the
/// class, and silent. The fail-safe now fires on ASCII controls only — the class
/// git quotes whatever `core.quotePath` says.
#[cfg(unix)]
#[test]
fn a_c1_control_in_a_filename_keeps_the_path_git_spelled() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    assert!(git(home.path(), repo.path(), &["init", "-q", "."])
        .status
        .success());
    std::fs::write(repo.path().join("g\u{85}.rs"), "one\ntwo\n").unwrap();
    assert!(git(home.path(), repo.path(), &["add", "-A"])
        .status
        .success());
    assert!(
        git(home.path(), repo.path(), &["commit", "-q", "-m", "seed"])
            .status
            .success()
    );
    rift_dir(home.path(), repo.path());

    // The repository's own spelling, asked for rather than assumed.
    let listed = git(
        home.path(),
        repo.path(),
        &["-c", "core.quotePath=false", "ls-files"],
    );
    let recorded = String::from_utf8_lossy(&listed.stdout)
        .trim_end_matches('\n')
        .to_owned();
    assert!(recorded.contains('\u{85}'), "fixture name: {recorded:?}");

    let out = rift(
        home.path(),
        repo.path(),
        &["blame", "--json", "--", &recorded],
    );
    assert_eq!(code(&out), 0, "blame exits 0: {out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["spans"][0]["path"], recorded,
        "a C1 path keeps the name git spelled it with"
    );
    assert_eq!(v["lines"], 2);
}

/// The route itself, and the one argument shape the `--` terminator decides.
///
/// **What this pins.** The contract: every flag-shaped and revision-shaped
/// argument is answered as a *path* — exit 4, `not_blameable`, the file reported
/// exactly as asked rather than as parsed — so a future change that resolves a
/// revision, that lets clap swallow `-h` before the verb sees it, or that echoes
/// a parsed spelling, turns this red.
///
/// **And the terminator, which the first cut of this test wrongly said no
/// assertion could distinguish.** That measurement swept eleven argv shapes, but
/// every one of them named a path *that does not exist*, so both arms failed by
/// construction and the comparison was vacuous. `--` can only matter for a path
/// that is really tracked and is really spelled like a flag, so this repository
/// tracks two of them: `-w` (a real `git blame` option) and `--evil.rs` (not an
/// option at all). Measured on git 2.50.1 — with the terminator both exit 0;
/// without it `-w` is consumed as the whitespace option and git exits **129**
/// for want of a path, and `--evil.rs` exits 129 as `error: unknown option`.
/// Two different mechanisms, one outcome, and the earlier claim that git fails
/// "for want of a second argument" was only ever half the story. Delete
/// `"--".to_owned()` from `blame`'s argv and the two assertions below fail;
/// before this test existed, deleting it failed nothing in the whole workspace.
#[test]
fn a_flag_shaped_argument_is_a_path_and_the_terminator_is_what_makes_it_one() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    assert!(git(home.path(), repo.path(), &["init", "-q", "."])
        .status
        .success());
    std::fs::write(repo.path().join("real.rs"), "a\nb\n").unwrap();
    // Two tracked files whose names are flags. Written through the filesystem,
    // never through an argv, so the fixture cannot be an artefact of git's own
    // argument parsing.
    std::fs::write(repo.path().join("-w"), "a\nb\n").unwrap();
    std::fs::write(repo.path().join("--evil.rs"), "p\nq\nr\n").unwrap();
    assert!(git(home.path(), repo.path(), &["add", "-A"])
        .status
        .success());
    assert!(
        git(home.path(), repo.path(), &["commit", "-q", "-m", "seed"])
            .status
            .success()
    );
    rift_dir(home.path(), repo.path());

    // THE TERMINATOR. A tracked file named exactly like an option is blamed as
    // the file it is, for both mechanisms git has for refusing one.
    let opt = rift(home.path(), repo.path(), &["blame", "--", "-w"]);
    assert_eq!(code(&opt), 0, "`-w` is a tracked file here: {opt:?}");
    assert!(
        String::from_utf8_lossy(&opt.stdout).contains("2 lines across 1 commit;"),
        "{opt:?}"
    );
    let unknown = rift(home.path(), repo.path(), &["blame", "--", "--evil.rs"]);
    assert_eq!(
        code(&unknown),
        0,
        "`--evil.rs` is tracked here: {unknown:?}"
    );
    assert!(
        String::from_utf8_lossy(&unknown.stdout).contains("3 lines across 1 commit;"),
        "{unknown:?}"
    );

    // A flag-shaped path that is *not* tracked is still answered as a path.
    let missing = rift(home.path(), repo.path(), &["blame", "--", "--missing.rs"]);
    assert_eq!(code(&missing), 4, "`--missing.rs` is a path: {missing:?}");
    let json = rift(
        home.path(),
        repo.path(),
        &["blame", "--json", "--", "--missing.rs"],
    );
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(v["error"], "not_blameable");
    assert_eq!(
        v["file"], "--missing.rs",
        "reported as asked, not as parsed"
    );

    // Revisions are paths here too: `HEAD` names no file in this worktree, so
    // the answer is exit 4 rather than a blame of the commit it resolves to.
    for arg in [
        "HEAD",
        "main",
        "@",
        "HEAD~1",
        "-h",
        "--help",
        "--version",
        "-",
        "--",
        "--reverse",
    ] {
        let out = rift(home.path(), repo.path(), &["blame", "--", arg]);
        assert_eq!(code(&out), 4, "`{arg}` must be a path, got {out:?}");
    }
    // A real path still answers.
    let ok = rift(home.path(), repo.path(), &["blame", "--", "real.rs"]);
    assert_eq!(code(&ok), 0, "a tracked path answers: {ok:?}");
    assert!(String::from_utf8_lossy(&ok.stdout).contains("2 lines across 1 commit;"));
    // …and without the `--` too, for the ordinary spelling.
    assert_eq!(
        code(&rift(home.path(), repo.path(), &["blame", "real.rs"])),
        0
    );
    // No path at all is a usage error, not a panic or an empty answer.
    assert_eq!(code(&rift(home.path(), repo.path(), &["blame"])), 2);
    // An empty path is refused before git is reached.
    assert_eq!(
        code(&rift(home.path(), repo.path(), &["blame", "--", ""])),
        1
    );
}

/// `blame` is a projection query, so it answers the init guard like the rest:
/// a repository Rift has never seen is exit 3, not an empty blame.
#[test]
fn blame_in_a_non_rift_repo_exits_not_init() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    assert!(git(home.path(), repo.path(), &["init", "-q", "."])
        .status
        .success());
    std::fs::write(repo.path().join("real.rs"), "a\n").unwrap();
    let out = rift(home.path(), repo.path(), &["blame", "real.rs"]);
    assert_eq!(code(&out), 3, "no `.rift/` is exit 3: {out:?}");
}

/// Build a repo whose ONLY legacy artifact is a per-checkpoint ref — an
/// adversarial review's `onlyrefs` plant, as a fixture: the shape that made the first version of
/// this verb report "no pre-Rift checkpoints found" with exit 0.
fn seed_only_refs_legacy_repo(home: &Path, repo: &Path) -> String {
    const ULID: &str = "01JQZ8T9WQKX3M7VN2P4RB6CDE";
    assert!(git(home, repo, &["init", "-q", "-b", "main"])
        .status
        .success());
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    assert!(git(home, repo, &["add", "-A"]).status.success());
    assert!(git(home, repo, &["commit", "-qm", "seed"]).status.success());

    // The shard tree, on an orphan commit reachable ONLY through the ref.
    assert!(git(home, repo, &["checkout", "-q", "--orphan", "staging"])
        .status
        .success());
    assert!(git(home, repo, &["rm", "-rfq", "."]).status.success());
    let (hi, rest) = ULID.split_at(2);
    let shard = repo.join(hi).join(rest);
    std::fs::create_dir_all(&shard).unwrap();
    std::fs::write(
        shard.join("metadata.json"),
        r#"{"strategy":"manual-commit","session_ids":["01JLEGACYSESSIONX"]}"#,
    )
    .unwrap();
    std::fs::write(
        shard.join("full.jsonl"),
        "{\"session_id\":\"01JLEGACYSESSIONX\",\"turn\":0,\"event\":0,\"kind\":\"session.start\",\"verb\":\"SessionStart\",\"occurred_at\":1}\n",
    )
    .unwrap();
    std::fs::write(
        shard.join("redaction_report.json"),
        format!(
            r#"{{"rule_pack_versions":["legacy@0.7.8"],"input_sha256":"{}"}}"#,
            "d".repeat(64)
        ),
    )
    .unwrap();
    assert!(git(home, repo, &["add", "-A"]).status.success());
    assert!(git(home, repo, &["commit", "-qm", "legacy"])
        .status
        .success());
    let oid = String::from_utf8(git(home, repo, &["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_owned();
    assert!(git(
        home,
        repo,
        &[
            "update-ref",
            &format!("refs/{RETIRED_BRAND}/checkpoints/{hi}/{ULID}"),
            &oid,
        ],
    )
    .status
    .success());
    assert!(git(home, repo, &["checkout", "-q", "main"])
        .status
        .success());
    assert!(git(home, repo, &["branch", "-qD", "staging"])
        .status
        .success());
    ULID.to_owned()
}

/// The rejection, as a test. A repository carrying ONLY the per-checkpoint-refs
/// layout must be imported and reported — not answered with a confident zero.
#[test]
fn import_from_legacy_reads_a_repo_that_has_only_the_per_checkpoint_refs_layout() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let ulid = seed_only_refs_legacy_repo(home.path(), repo.path());

    let out = rift(home.path(), repo.path(), &["import", "--from-legacy"]);
    assert_eq!(code(&out), 0, "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        !text.contains("no pre-Rift checkpoints"),
        "the false negative this test exists for: {text}"
    );
    // Reported under the RIGHT layout — the previous version hardcoded the other.
    assert!(
        text.contains("layout per-checkpoint-refs: 1"),
        "layout must be derived from the carrying ref: {text}"
    );
    assert!(text.contains("importable 1"), "{text}");
    // Still no brand on the surface.
    assert!(
        !text.to_ascii_lowercase().contains(RETIRED_BRAND),
        "brand leaked: {text}"
    );

    // …and the JSON agrees, including the count that replaced the always-zero
    // `sources` field.
    let out = rift(
        home.path(),
        repo.path(),
        &["import", "--from-legacy", "--json"],
    );
    assert_eq!(code(&out), 0, "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("import emits json");
    assert_eq!(v["refs_layout_shards"], serde_json::json!(1), "{v}");
    assert_eq!(v["importable"], serde_json::json!(1), "{v}");
    assert_eq!(
        v["by_layout"]["per-checkpoint-refs"],
        serde_json::json!(1),
        "{v}"
    );
    assert_eq!(v["trust"], serde_json::json!("claim"), "{v}");
    assert!(v["root_checks"].is_object(), "{v}");

    // Read-only: the ref the shard lives on is untouched, byte for byte.
    let refs = git(home.path(), repo.path(), &["for-each-ref"]).stdout;
    assert!(
        String::from_utf8_lossy(&refs)
            .contains(&format!("refs/{RETIRED_BRAND}/checkpoints/01/{ulid}")),
        "the source ref survives the import unchanged"
    );
}
/// Write one legacy shard tree (`<id[:2]>/<id[2:]>/…`) into the working tree.
fn write_legacy_shard(repo: &Path, id: &str) {
    let (hi, rest) = id.split_at(2);
    let shard = repo.join(hi).join(rest);
    std::fs::create_dir_all(&shard).unwrap();
    std::fs::write(
        shard.join("metadata.json"),
        r#"{"strategy":"manual-commit","session_ids":["01JLEGACYSESSIONX"]}"#,
    )
    .unwrap();
    std::fs::write(
        shard.join("full.jsonl"),
        "{\"session_id\":\"01JLEGACYSESSIONX\",\"turn\":0,\"event\":0,\"kind\":\"session.start\",\"verb\":\"SessionStart\",\"occurred_at\":1}\n",
    )
    .unwrap();
    std::fs::write(
        shard.join("redaction_report.json"),
        format!(
            r#"{{"rule_pack_versions":["legacy@0.7.8"],"input_sha256":"{}"}}"#,
            "d".repeat(64)
        ),
    )
    .unwrap();
}

/// Commit the current working tree as an orphan and park it at `full_ref`,
/// reachable ONLY through that ref. Returns nothing; `main` is checked out.
fn park_orphan_tree_at(home: &Path, repo: &Path, full_ref: &str, seed: impl Fn(&Path)) {
    assert!(git(home, repo, &["checkout", "-q", "--orphan", "staging"])
        .status
        .success());
    assert!(git(home, repo, &["rm", "-rfq", "."]).status.success());
    seed(repo);
    assert!(git(home, repo, &["add", "-A"]).status.success());
    assert!(git(home, repo, &["commit", "-qm", "legacy"])
        .status
        .success());
    let oid = String::from_utf8(git(home, repo, &["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_owned();
    assert!(git(home, repo, &["update-ref", full_ref, &oid])
        .status
        .success());
    assert!(git(home, repo, &["checkout", "-q", "main"])
        .status
        .success());
    assert!(git(home, repo, &["branch", "-qD", "staging"])
        .status
        .success());
}

fn seed_main_with_one_commit(home: &Path, repo: &Path) {
    assert!(git(home, repo, &["init", "-q", "-b", "main"])
        .status
        .success());
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    assert!(git(home, repo, &["add", "-A"]).status.success());
    assert!(git(home, repo, &["commit", "-qm", "seed"]).status.success());
}

/// An adversarial review's junk-ref shape, as a fixture. Three shards live under a ref that is
/// inside the legacy checkpoint namespace but is **not** spelled
/// `<shard>/<id>`, so `layout_of_shard_ref` refuses to classify its name — and
/// the repository has **no** legacy branch at all.
fn seed_junk_ref_legacy_repo(home: &Path, repo: &Path) -> Vec<&'static str> {
    const IDS: [&str; 3] = [
        "01JQZ8T9WQKX3M7VN2P4RB6CD1",
        "01JQZ8T9WQKX3M7VN2P4RB6CD2",
        "01JQZ8T9WQKX3M7VN2P4RB6CD3",
    ];
    seed_main_with_one_commit(home, repo);
    park_orphan_tree_at(
        home,
        repo,
        concat!("refs/", "ent", "ire", "/checkpoints/scratch"),
        |r| {
            for id in IDS {
                write_legacy_shard(r, id);
            }
        },
    );
    IDS.to_vec()
}

/// The layout label must come from the walk that FOUND the shard, not from a
/// hardcoded fallback. This repository has no legacy branch, so any occurrence
/// of `sharded-branch` in the output is a fabricated label — which is exactly
/// what the previous `.unwrap_or(Layout::ShardedBranch)` printed here.
#[test]
fn import_from_legacy_never_labels_a_branchless_repo_sharded_branch() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let ids = seed_junk_ref_legacy_repo(home.path(), repo.path());

    let out = rift(home.path(), repo.path(), &["import", "--from-legacy"]);
    assert_eq!(code(&out), 0, "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    // THE ASSERTION THIS TEST EXISTS FOR: there is no branch in this repository.
    assert!(
        !text.contains("sharded-branch"),
        "a layout that is not present must never be named: {text}"
    );
    assert!(
        text.contains(&format!("layout per-checkpoint-refs: {}", ids.len())),
        "the ref walk found them, so the ref walk names them: {text}"
    );
    // …and nothing was dropped on the way to being labelled correctly.
    assert!(
        text.contains(&format!("importable {}", ids.len())),
        "{text}"
    );

    let out = rift(
        home.path(),
        repo.path(),
        &["import", "--from-legacy", "--json"],
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("import emits json");
    assert_eq!(v["by_layout"]["per-checkpoint-refs"], 3, "{v}");
    assert_eq!(v["by_layout"]["sharded-branch"], serde_json::Value::Null);
    assert_eq!(v["refs_layout_shards"], 3, "{v}");
    // Always emitted, so a reader can tell "nothing unattributed" from "nobody
    // looked" — and so the residual can never be silently relabelled again.
    assert_eq!(v["unattributed"], 0, "{v}");
}

/// The other direction of the same fix: a repository whose only legacy artifact
/// IS the shared orphan branch must still be labelled `sharded-branch` — which
/// the removed fallback used to do by accident. Nothing covered this before, so
/// dropping the fallback could have silently unlabelled every branch shard.
#[test]
fn import_from_legacy_labels_the_branch_layout_from_the_branch_walk() {
    const HEX_ID: &str = "a1b2c3d4e5f60718";
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    seed_main_with_one_commit(home.path(), repo.path());
    park_orphan_tree_at(
        home.path(),
        repo.path(),
        concat!("refs/heads/", "ent", "ire", "/checkpoints/v1"),
        |r| write_legacy_shard(r, HEX_ID),
    );

    let out = rift(home.path(), repo.path(), &["import", "--from-legacy"]);
    assert_eq!(code(&out), 0, "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("layout sharded-branch: 1"), "{text}");
    assert!(!text.contains("per-checkpoint-refs"), "{text}");
    assert!(text.contains("importable 1"), "{text}");

    let out = rift(
        home.path(),
        repo.path(),
        &["import", "--from-legacy", "--json"],
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("import emits json");
    assert_eq!(v["by_layout"]["sharded-branch"], 1, "{v}");
    assert_eq!(v["refs_layout_shards"], 0, "{v}");
    assert_eq!(v["unattributed"], 0, "{v}");
}

/// `rift import --from-legacy` on a repository with no pre-Rift checkpoints: a
/// clean, quiet zero. The verb is READ-ONLY, so this also pins that it writes
/// nothing — the ref listing before and after is byte-identical.
#[test]
fn import_from_legacy_is_read_only_and_quiet_when_there_is_nothing_to_read() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    assert!(git(home.path(), repo.path(), &["init", "-q", "."])
        .status
        .success());
    std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
    assert!(git(home.path(), repo.path(), &["add", "a.txt"])
        .status
        .success());
    assert!(git(home.path(), repo.path(), &["commit", "-qm", "seed"])
        .status
        .success());

    let refs_before = git(home.path(), repo.path(), &["for-each-ref"]).stdout;
    let out = rift(home.path(), repo.path(), &["import", "--from-legacy"]);
    assert_eq!(code(&out), 0, "an empty import is success: {out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        text.contains("no pre-Rift checkpoints"),
        "says so plainly: {text}"
    );
    // The legacy tool's name appears nowhere a user can see it.
    let lower = text.to_ascii_lowercase();
    assert!(!lower.contains(RETIRED_BRAND), "brand leaked: {text}");

    let refs_after = git(home.path(), repo.path(), &["for-each-ref"]).stdout;
    assert_eq!(refs_before, refs_after, "import is read-only");
}

/// `rift import` with no source selected is a precondition error, not a silent
/// no-op a script would read as "there was nothing to import".
#[test]
fn import_without_a_source_is_a_precondition_error() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    assert!(git(home.path(), repo.path(), &["init", "-q", "."])
        .status
        .success());
    let out = rift(home.path(), repo.path(), &["import"]);
    // 4 = PRECONDITION, not 1: a script can tell "you did not ask for anything"
    // apart from "the import failed".
    assert_eq!(
        code(&out),
        4,
        "no source selected is a precondition: {out:?}"
    );
}

// --- `rift chain` — the checkpoint order -------------------------------------

/// Content-hash order is `A < MID < B`; **write order is A, B, MID**. Any verb
/// that sorts ids and calls the result an order puts B at the head; the
/// branch's own first-parent history puts MID there. Every assertion below
/// turns on that difference, which is the fork this ref exists to end.
const CK_A: &str = "4c5c0a053e73b924663009e1602882b6073d3fdd10ad33aa92c988a643e040f3";
const CK_B: &str = "b970a014c1a3dd6508c5382679051a37f7cc1cd238b66543ecb1472a746d23c7";
const CK_MID: &str = "93ae336492a162b429cf27a5952d8d8f0a7a878eca243585d5bc33893d7d0d9a";

/// Write one checkpoint shard onto the branch currently checked out, as one
/// commit — the shape `NativeBackend::archive` writes.
fn commit_shard(home: &Path, repo: &Path, id: &str) {
    let (hi, rest) = id.split_at(2);
    let shard = repo.join(hi).join(rest);
    std::fs::create_dir_all(&shard).unwrap();
    std::fs::write(
        shard.join("metadata.json"),
        r#"{"strategy":"manual-commit"}"#,
    )
    .unwrap();
    assert!(git(home, repo, &["add", "-A"]).status.success());
    assert!(git(home, repo, &["commit", "-qm", id]).status.success());
}

/// A repository whose orphan `rift/checkpoints/v1` branch carries `ids`, one
/// commit each, in the order given.
fn seed_checkpoint_branch(home: &Path, repo: &Path, ids: &[&str]) {
    assert!(git(home, repo, &["init", "-q", "-b", "main"])
        .status
        .success());
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    assert!(git(home, repo, &["add", "-A"]).status.success());
    assert!(git(home, repo, &["commit", "-qm", "seed"]).status.success());
    assert!(git(
        home,
        repo,
        &["checkout", "-q", "--orphan", "rift/checkpoints/v1"]
    )
    .status
    .success());
    assert!(git(home, repo, &["rm", "-rfq", "."]).status.success());
    for id in ids {
        commit_shard(home, repo, id);
    }
    assert!(git(home, repo, &["checkout", "-qf", "main"])
        .status
        .success());
}

fn chain_status_json(home: &Path, repo: &Path) -> serde_json::Value {
    let out = rift(home, repo, &["chain", "status", "--json"]);
    assert_eq!(code(&out), 0, "{out:?}");
    serde_json::from_slice(&out.stdout).expect("chain status --json")
}

/// `rift chain init` records the branch's **write** order and names its head.
#[test]
fn chain_init_records_the_branch_write_order_and_names_the_head() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    assert!(
        CK_A < CK_MID && CK_MID < CK_B,
        "the fixture must carry the sort hazard"
    );
    seed_checkpoint_branch(home.path(), repo.path(), &[CK_A, CK_B, CK_MID]);

    let out = rift(home.path(), repo.path(), &["chain", "init"]);
    assert_eq!(code(&out), 0, "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("3 entries"), "entries written: {text}");
    assert!(text.contains("head entry:"), "the head is named: {text}");
    assert!(
        text.contains(&format!("head checkpoint: {CK_MID}")),
        "the head checkpoint is the LAST WRITTEN one: {text}"
    );

    let st = chain_status_json(home.path(), repo.path());
    assert_eq!(st["positions"], 3);
    assert_eq!(st["exists"], true);
    assert_eq!(st["head_checkpoint"], CK_MID);
    assert_ne!(
        st["head_checkpoint"], CK_B,
        "sorted ids would have put B at the head — that sort is the fork"
    );
}

/// The refusal, rendered as an explanation, with nothing moved behind it.
#[test]
fn a_second_chain_init_is_refused_as_a_rewrite_and_moves_nothing() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    seed_checkpoint_branch(home.path(), repo.path(), &[CK_A, CK_B]);
    assert_eq!(code(&rift(home.path(), repo.path(), &["chain", "init"])), 0);
    let before = chain_status_json(home.path(), repo.path());

    let out = rift(home.path(), repo.path(), &["chain", "init"]);
    // 65 = INTEGRITY: refused on an append-only ground, and not retryable.
    assert_eq!(code(&out), 65, "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("already exists"), "{err}");
    assert!(
        err.contains("rewrite wearing an initializer's name"),
        "the refusal explains itself: {err}"
    );
    assert!(
        err.contains("Nothing was written"),
        "and says what it did not do: {err}"
    );
    assert!(
        out.stdout.is_empty(),
        "a refusal writes nothing to stdout: {out:?}"
    );

    let after = chain_status_json(home.path(), repo.path());
    assert_eq!(before["head_entry"], after["head_entry"], "the ref moved");
    assert_eq!(before["positions"], after["positions"]);
}

/// An absent ref is reported as absent — never invented by the reading verb.
#[test]
fn chain_status_reports_an_absent_ref_without_creating_one() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    seed_checkpoint_branch(home.path(), repo.path(), &[CK_A]);

    let out = rift(home.path(), repo.path(), &["chain", "status"]);
    assert_eq!(code(&out), 0, "reading an absent ref is not a failure");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("does not exist yet"), "{text}");
    assert!(
        text.contains("rift chain init"),
        "says what records it: {text}"
    );

    let st = chain_status_json(home.path(), repo.path());
    assert_eq!(st["exists"], false);
    assert_eq!(st["positions"], 0);
    // …and status is read-only: the ref is still absent afterwards.
    let rp = git(
        home.path(),
        repo.path(),
        &["rev-parse", "--verify", "--quiet", "refs/rift/chain/v1"],
    );
    assert!(!rp.status.success(), "status created the ref: {rp:?}");
}

/// A branch that forked and merged has no single write order. Report it;
/// linearizing it would be the same invention the sorted-hash chain was.
#[test]
fn a_merged_checkpoint_branch_is_refused_rather_than_linearized() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    seed_checkpoint_branch(home.path(), repo.path(), &[CK_A]);
    // Fork the checkpoint branch, write a checkpoint on each side, merge.
    assert!(git(
        home.path(),
        repo.path(),
        &["checkout", "-qf", "rift/checkpoints/v1"]
    )
    .status
    .success());
    assert!(
        git(home.path(), repo.path(), &["checkout", "-q", "-b", "side"])
            .status
            .success()
    );
    commit_shard(home.path(), repo.path(), CK_MID);
    assert!(git(
        home.path(),
        repo.path(),
        &["checkout", "-qf", "rift/checkpoints/v1"]
    )
    .status
    .success());
    commit_shard(home.path(), repo.path(), CK_B);
    assert!(git(
        home.path(),
        repo.path(),
        &["merge", "-q", "--no-edit", "side"]
    )
    .status
    .success());
    assert!(git(home.path(), repo.path(), &["checkout", "-qf", "main"])
        .status
        .success());

    let out = rift(home.path(), repo.path(), &["chain", "init"]);
    assert_eq!(code(&out), 65, "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("not linear"), "names the shape: {err}");
    assert!(err.contains("Nothing was written"), "{err}");
    let rp = git(
        home.path(),
        repo.path(),
        &["rev-parse", "--verify", "--quiet", "refs/rift/chain/v1"],
    );
    assert!(
        !rp.status.success(),
        "a refused init leaves no partial chain: {rp:?}"
    );
}

// ---------------------------------------------------------------------------
// The POPULATED `--json` contracts, byte-locked, and the
// squash tie-break proven against trailer order — over one deterministic
// fixture whose every input is pinned (dates, identity, clock, content), so
// the whole stdout of each verb is a constant of the tree.
// ---------------------------------------------------------------------------

/// The fixture's two capture instants (epoch ms): X is OLDER, Y is NEWER —
/// and X is listed FIRST in the trailer, so trailer order and creation order
/// disagree on purpose.
const LOCK_T_OLD: i64 = 1_756_000_100_000;
const LOCK_T_NEW: i64 = 1_756_000_200_000;

/// [`git_commit_at`] with the WHOLE identity pinned, not just the dates.
///
/// The sibling helper leaves author/committer to git's auto-detection, which
/// embeds the developer's name and hostname in the commit object — and
/// therefore in its oid — so a golden frozen over such an oid holds only on
/// the machine that froze it. Measured, not argued: the `f.rs a\nb\nc\n` seed
/// tree at the same pinned dates and message commits as `76687c1b…` under
/// this machine's auto-ident and `eca66ff1…` under the identity pinned here.
/// The goldens below must hold on every machine, so nothing about the
/// identity is left for git to guess.
fn git_commit_pinned(home: &Path, cwd: &Path, date: &str, msg: &str) -> Output {
    let mut cmd = Command::new("git");
    cmd.args(["commit", "-q", "-m", msg])
        .current_dir(cwd)
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .env("GIT_AUTHOR_NAME", "Rift Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@rift.invalid")
        .env("GIT_COMMITTER_NAME", "Rift Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@rift.invalid");
    hermetic_git(&mut cmd, home);
    cmd.output().expect("spawn git")
}

/// Build the populated provenance world in `repo`: one date-pinned commit of
/// `f.rs` whose message claims TWO checkpoints (trailer order X, Y), two real
/// shards archived through the production writer with recorded capture times
/// (X older, Y newer), the projection derived by the REAL bridge, and
/// deterministic session/attribution/cost rows. Returns `(ck_x, ck_y)`.
///
/// The ids, the commit oid, the prompt bytes and every timestamp in the three
/// `--json` documents are functions of pinned inputs — no wall clock, no RNG —
/// which is what makes the byte-locks below possible at all.
// One linear seeding cascade, same shape and same reason as
// `workflows::data_tests::capture`: the fixture's value is that it reads top
// to bottom against the doc above.
#[allow(clippy::too_many_lines)]
fn populated_provenance_fixture(home: &Path, repo: &Path) -> (String, String) {
    use rift_shadow::backend::{CheckpointArchive, RedactionReportView};
    use rift_shadow::ShadowBackend as _;
    use rift_store::CanonicalStore as _;

    assert!(git(home, repo, &["init", "-q", "."]).status.success());
    std::fs::write(
        repo.join("f.rs"),
        "alpha line one\nalpha line two\nalpha line three\n",
    )
    .unwrap();
    assert!(git(home, repo, &["add", "-A"]).status.success());

    // The two archives, through the same functions the condense path calls.
    let packs = rift_shadow::redact::RulePackSet::builtin();
    let seal = |text: &str| {
        let out = rift_shadow::redact::redact_with(
            &rift_core::SecretBytes::new(text.as_bytes().to_vec()),
            &packs,
        )
        .unwrap();
        (out.redacted, RedactionReportView::from_redact(&out.report))
    };
    let (content_x, report_x) = seal("capture x: the older checkpoint\n");
    let (content_y, report_y) = seal("capture y: the newer checkpoint\n");
    // Y carries the transcript, so the SELECTED checkpoint is the one whose
    // prompt `why`/`explain` must serve.
    let raw = concat!(
        r#"{"type":"user","timestamp":"2026-08-01T00:00:00Z","message":{"role":"user","content":[{"type":"text","text":"wire the alpha parser"}]}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-08-01T00:00:01Z","message":{"id":"msg_01G3LOCK","role":"assistant","usage":{"input_tokens":64,"output_tokens":8},"content":[{"type":"text","text":"Wiring it."}]}}"#,
        "\n",
    );
    let (transcript, _lines) = rift_shadow::archived_transcript(
        &rift_core::SecretBytes::new(raw.as_bytes().to_vec()),
        0,
        &packs,
    )
    .unwrap();
    let archive = |session: &str,
                   content: rift_core::RedactedBytes,
                   report: RedactionReportView,
                   transcript: Option<rift_shadow::backend::ArchivedTranscript>,
                   at: i64| {
        let mut a = CheckpointArchive {
            repo: repo.to_path_buf(),
            commit: String::new(),
            checkpoint_id: String::new(),
            sessions: vec![session.to_owned()],
            strategy: "manual-commit".to_owned(),
            prev_hash: None,
            events: Vec::new(),
            read_facts: Vec::new(),
            content,
            report,
            transcript,
            tasks: Vec::new(),
            created_at_ms: Some(at),
            // A hand-built archive is in no repository: the member is ABSENT, which
            // is what keeps every frozen anchor and golden byte-identical.
            worktree: None,
            session_worktrees: std::collections::BTreeMap::new(),
        };
        a.checkpoint_id = a.derived_checkpoint_id().unwrap();
        a
    };
    let mut ck_x = archive("01JG3LOCKA", content_x, report_x, None, LOCK_T_OLD);
    let mut ck_y = archive(
        "01JG3LOCKB",
        content_y,
        report_y,
        Some(transcript),
        LOCK_T_NEW,
    );

    // The commit CLAIMS both — X first, so the legacy `candidates[0]` rule and
    // the created-at rule name different checkpoints.
    let msg = format!(
        "seed\n\nRift-Checkpoint: {}\nRift-Checkpoint: {}",
        ck_x.checkpoint_id, ck_y.checkpoint_id
    );
    assert!(
        git_commit_pinned(home, repo, "2026-01-02T03:04:05 +0000", &msg)
            .status
            .success()
    );
    let head = git(home, repo, &["rev-parse", "HEAD"]);
    let anchor = String::from_utf8_lossy(&head.stdout).trim().to_owned();

    // Anchor recorded, ids untouched by it (the anchoring-commit rule), then
    // both shards through the production writer.
    ck_x.commit.clone_from(&anchor);
    ck_y.commit.clone_from(&anchor);
    let git_repo = rift_git::discover(repo).unwrap();
    let backend = rift_shadow::NativeBackend::new(rift_git::PlumbingGitWrite::new(git_repo));
    backend.archive_checkpoint(&ck_x).unwrap();
    backend.archive_checkpoint(&ck_y).unwrap();

    // The REAL bridge derives the projection — `checkpoint.created_at` comes
    // from each shard's own `metadata.json`, never from this call's clock.
    let repo_id = rift_git::repo_id_for(repo).unwrap();
    let db = rift_store::projection_db_under(&home.join("cache"), &repo_id);
    let store = rift_store::open_capture_projection(&db).unwrap();
    let reader = rift_recall_substrate::GixSubstrate::open(repo).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(rift_shadow::ingest_from_substrate(
        &store,
        &reader,
        &repo_id.to_string(),
        "claude-code",
        1_756_000_300_000,
    ))
    .unwrap();

    // Deterministic session metadata, per-file attribution for BOTH claims
    // (so rank criterion 1 ties and criterion 3 must decide), and one costed
    // model call for Y's session — the rows the live capture path writes.
    rt.block_on(async {
        let t = |v: &str| rift_store::Value::text(v.to_owned());
        let sid = |native: &str| rift_core::capture_session_row_id("claude-code", native);
        for (native, started) in [("01JG3LOCKA", LOCK_T_OLD), ("01JG3LOCKB", LOCK_T_NEW)] {
            store
                .insert(
                    "session",
                    &[
                        "session_id",
                        "agent",
                        "model_primary",
                        "repo_id",
                        "started_at",
                        "schema_version",
                    ],
                    &[
                        t(&sid(native)),
                        t("claude-code"),
                        t("claude-sonnet-4-5"),
                        t(&repo_id.to_string()),
                        rift_store::Value::Int(started - 5_000),
                        rift_store::Value::Int(1),
                    ],
                )
                .await
                .unwrap();
        }
        rift_store::record_file_revision(
            &store,
            &rift_store::FileRevisionRow {
                file_revision_id: "fr-g3".into(),
                path: "f.rs".into(),
                blob_oid: "b0".into(),
                content_sha256: "c0".into(),
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        for (ck, agent_added, human_added, conf) in [
            (&ck_x.checkpoint_id, 3, 0, 900),
            (&ck_y.checkpoint_id, 2, 1, 850),
        ] {
            rift_store::record_attribution(
                &store,
                &rift_store::AttributionRow {
                    checkpoint_id: ck.clone(),
                    file_revision_id: "fr-g3".into(),
                    agent_added,
                    human_added,
                    human_modified_agent: 0,
                    agent_removed_by_human: 0,
                    confidence_milli: conf,
                    method_version: "m1".into(),
                    supersedes_pk: None,
                    schema_version: 1,
                },
            )
            .await
            .unwrap();
        }
        store
            .insert(
                "turn",
                &[
                    "turn_id",
                    "session_id",
                    "ordinal",
                    "started_at",
                    "schema_version",
                ],
                &[
                    t("turn-g3"),
                    t(&sid("01JG3LOCKB")),
                    rift_store::Value::Int(1),
                    rift_store::Value::Int(LOCK_T_NEW - 4_000),
                    rift_store::Value::Int(1),
                ],
            )
            .await
            .unwrap();
        store
            .insert(
                "model_call",
                &[
                    "call_id",
                    "turn_id",
                    "model",
                    "started_at",
                    "schema_version",
                ],
                &[
                    t("mc-g3"),
                    t("turn-g3"),
                    t("claude-sonnet-4-5"),
                    rift_store::Value::Int(LOCK_T_NEW - 3_000),
                    rift_store::Value::Int(1),
                ],
            )
            .await
            .unwrap();
        store
            .insert(
                "token_usage",
                &[
                    "call_id",
                    "uncached_input",
                    "cached_input",
                    "cache_creation",
                    "output",
                    "counter_method",
                    "schema_version",
                ],
                &[
                    t("mc-g3"),
                    rift_store::Value::Int(64),
                    rift_store::Value::Int(0),
                    rift_store::Value::Int(0),
                    rift_store::Value::Int(8),
                    t("exact"),
                    rift_store::Value::Int(1),
                ],
            )
            .await
            .unwrap();
    });

    rift_dir(home, repo);
    (ck_x.checkpoint_id, ck_y.checkpoint_id)
}

/// Read one committed golden, comparing BYTES — the shard-goldens discipline:
/// no bless switch, no normalization; a deliberate contract change re-freezes
/// the file in its own commit, with the diff in review.
fn assert_matches_golden(name: &str, got: &[u8]) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("goldens-provenance")
        .join(name);
    let want = std::fs::read(&path).unwrap_or_else(|_| {
        panic!(
            "{name}: no frozen golden. There is no bless switch — place \
             this file by hand after reading it:\n----8<----\n{}\n----8<----",
            String::from_utf8_lossy(got)
        )
    });
    assert!(
        want == got,
        "the locked {name} document moved.\n--- frozen ---\n{}\n--- got ---\n{}",
        String::from_utf8_lossy(&want),
        String::from_utf8_lossy(got),
    );
}

/// **The populated `--json` documents are byte-locked.** The
/// earlier locks froze the EMPTY documents — `"checkpoints": {}`, `"prompt":
/// null` — so the populated shape could drift without a byte moving. These
/// three goldens freeze the answering shape: resolved spans, attribution
/// counts, sessions with labels, a non-null redacted prompt served from the
/// shard, cost, and a real `created_at_ms` — the injected capture instant,
/// which is also the proof the writer fix reaches the documents.
#[test]
fn the_populated_blame_why_explain_documents_are_byte_locked() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let (_ck_x, ck_y) = populated_provenance_fixture(home.path(), repo.path());

    let blame = rift(home.path(), repo.path(), &["blame", "f.rs", "--json"]);
    assert_eq!(code(&blame), 0, "{blame:?}");
    assert_matches_golden("blame.golden", &blame.stdout);

    let why = rift(home.path(), repo.path(), &["why", "f.rs:2", "--json"]);
    assert_eq!(code(&why), 0, "{why:?}");
    assert_matches_golden("why.golden", &why.stdout);

    let explain = rift(
        home.path(),
        repo.path(),
        &["checkpoint", "explain", &ck_y, "--json"],
    );
    assert_eq!(code(&explain), 0, "{explain:?}");
    assert_matches_golden("explain.golden", &explain.stdout);

    // Beside the frozen bytes, the load-bearing populated facts in the clear —
    // so a legitimate future re-freeze cannot silently drop them.
    let w: serde_json::Value = serde_json::from_slice(&why.stdout).unwrap();
    assert_eq!(w["prompt"]["text"], "wire the alpha parser", "{w}");
    assert_eq!(w["tag"], "mixed", "{w}");
    assert!(w["cost"].is_object(), "{w}");
    // The checkpoint's strategy token rides both documents:
    // a live archive says `manual-commit`; an imported transcript would say
    // `import`.
    assert_eq!(w["checkpoint"]["strategy"], "manual-commit", "{w}");
    let b: serde_json::Value = serde_json::from_slice(&blame.stdout).unwrap();
    assert_eq!(b["checkpoints"][&ck_y]["strategy"], "manual-commit", "{b}");
    // The unknown rule in the clear, beside the frozen bytes: this fixture's
    // archives are hand-built and carry NO `worktree` member, so all three
    // documents answer null — the shape every shard written before worktree
    // provenance existed is in.
    // Null, not the string `main`: a re-freeze that turned absence into the
    // main work tree would be a claim, and it would pass a bytes-only lock.
    assert_eq!(w["checkpoint"]["worktree"], serde_json::Value::Null, "{w}");
    assert_eq!(
        b["checkpoints"][&ck_y]["worktree"],
        serde_json::Value::Null,
        "{b}"
    );
    let e: serde_json::Value = serde_json::from_slice(&explain.stdout).unwrap();
    assert_eq!(e["created_at_ms"], LOCK_T_NEW, "{e}");
    assert_eq!(e["prompt"]["text"], "wire the alpha parser", "{e}");
    assert_eq!(
        e["metadata"]["created_at"], "2025-08-24T01:50:00.000Z",
        "{e}"
    );
    assert_eq!(e["worktree"], serde_json::Value::Null, "{e}");
}

/// **The squash tie-break prefers creation time over trailer order**
/// (proven at the verb): one commit claims X then Y, X is
/// older, both recorded touching the file — the legacy `candidates[0]` rule
/// answers X, the created-at rule answers Y, and the shipped verb must answer
/// Y. Before the writer fix every native checkpoint carried `created_at =
/// -1`, so this exact rule silently degraded to trailer order and no test
/// could tell the two apart.
#[test]
fn a_multi_trailer_commit_resolves_by_created_at_not_trailer_order() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let (ck_x, ck_y) = populated_provenance_fixture(home.path(), repo.path());

    let out = rift(home.path(), repo.path(), &["blame", "f.rs", "--json"]);
    assert_eq!(code(&out), 0, "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    // The claims really are in trailer order, X before Y…
    let commit = v["spans"][0]["commit"].as_str().unwrap();
    let claims = v["commits"][commit]["claims"].as_array().unwrap();
    assert_eq!(claims.len(), 2, "{v}");
    assert_eq!(claims[0]["checkpoint_id"], ck_x.as_str(), "{v}");
    assert_eq!(claims[0]["ordinal"], 0, "{v}");
    assert_eq!(claims[1]["checkpoint_id"], ck_y.as_str(), "{v}");

    // …and the selection is Y anyway: the newer capture wins the file.
    let sel = &v["spans"][0]["selection"];
    assert_eq!(sel["checkpoint_id"], ck_y.as_str(), "{v}");
    assert_eq!(sel["considered"], 2, "{v}");
    assert_eq!(sel["basis"], "file_touched", "{v}");
    assert_eq!(sel["resolved"], true, "{v}");
    assert_eq!(
        v["checkpoints"][ck_y.as_str()]["created_at_ms"],
        LOCK_T_NEW,
        "the winner's recorded instant, not a sentinel: {v}"
    );
}
