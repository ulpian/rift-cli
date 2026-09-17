//! End-to-end: **`rift init` inside a linked worktree.**
//!
//! A member is a second path to one repository, not a second repository. Run
//! from inside one, `init` therefore registers the family's **root** — and
//! adopts the member it was run in, which is the wiring that checkout does not
//! have and cannot get any other way. Registering the member instead was the
//! duplicate project the one-row rule removes: two rows, two cards, one repository.
//!
//! Everything here runs the shipped binary against a real `git init` and a real
//! `git worktree add`, with the registry, cache, family and Codex roots all
//! redirected into the fixture — this machine's own `~/.config/rift/
//! projects.toml` is neither read nor written.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

/// Hermetic `git` in `dir`: no system/global config, a fixed identity, and
/// every re-targeting variable scrubbed.
fn git_in(dir: &Path, args: &[&str]) {
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
        .output()
        .expect("git is on PATH");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository family with one linked member, plus the roots every child of
/// this fixture is redirected onto.
struct Family {
    _tmp: tempfile::TempDir,
    main: PathBuf,
    member: PathBuf,
    home: PathBuf,
}

impl Family {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let main = root.join("main");
        let member = root.join("member");
        let home = root.join("home");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        git_in(&main, &["init", "-q", "-b", "main"]);
        std::fs::write(main.join("a.txt"), "a\n").unwrap();
        git_in(&main, &["add", "a.txt"]);
        git_in(&main, &["commit", "-q", "-m", "seed"]);
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

        Self {
            _tmp: tmp,
            main,
            member,
            home,
        }
    }

    /// Run `rift <args>` from `cwd` with the fixture's roots; `(code, stdout)`.
    fn rift(&self, cwd: &Path, args: &[&str]) -> (i32, String) {
        self.rift_with(cwd, args, &[]).0
    }

    /// As [`Family::rift`], with `extra` environment layered on; returns
    /// `((code, stdout), stderr)` — a non-zero outcome prints its report to
    /// stderr (`emit`).
    fn rift_with(
        &self,
        cwd: &Path,
        args: &[&str],
        extra: &[(&str, String)],
    ) -> ((i32, String), String) {
        let mut cmd = common::rift_cmd(cwd);
        cmd.args(args);
        for (key, value) in extra {
            cmd.env(key, value);
        }
        let out = cmd
            .env("RIFT_CACHE_DIR", self.home.join("cache"))
            .env("RIFT_CONFIG_DIR", self.home.join("config"))
            .env("RIFT_HEADLESS", "1")
            .env("RIFT_NO_KEYCHAIN", "1")
            .env("HOME", &self.home)
            .env("CODEX_HOME", self.home.join(".codex"))
            .output()
            .expect("rift runs");
        (
            (
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stdout).into_owned(),
            ),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// The machine-global TOML registry this fixture writes.
    fn registry_text(&self) -> String {
        std::fs::read_to_string(self.home.join("config/rift/projects.toml"))
            .expect("`rift init` writes the registry")
    }
}

/// ★ `init` run inside a linked worktree registers the family
/// **root**, and adopts the worktree it was run in.
///
/// Both halves matter and they are different facts. Registering the root is
/// what keeps one row per repository; adopting the member is what
/// gives that checkout the wiring an agent launched there needs, which no
/// amount of registering the root would provide.
///
/// The falsification is the second half of the test: `init` from the MAIN work
/// tree afterwards must find the row already there rather than add a second
/// one — if the first run had registered the member, this run would add the
/// root beside it and the count would be two.
#[test]
fn init_from_a_member_registers_the_family_root_not_the_member() {
    let f = Family::new();
    let main_canonical = std::fs::canonicalize(&f.main).unwrap();
    let member = f.member.clone();

    let (code, out) = f.rift(&member, &["init", "--no-recall"]);
    assert_eq!(code, 0, "init from a member: {out}");
    assert!(
        out.contains("adopted worktree member of family"),
        "the member is adopted, and the line names its family: {out}"
    );
    assert!(
        out.contains(&main_canonical.display().to_string()),
        "the family named is the MAIN work tree: {out}"
    );

    let registry = f.registry_text();
    assert_eq!(
        registry.matches("[[projects]]").count(),
        1,
        "exactly one row for the family: {registry}"
    );
    // Parsed, then compared as a path: `toml_edit` writes a backslash path
    // as a literal `'…'` string, so a text search for `"…"` is platform-naive.
    let rows: toml::Value = toml::from_str(&registry).expect("the registry is TOML");
    let row_path = rows["projects"][0]["path"]
        .as_str()
        .map(std::path::PathBuf::from);
    assert_eq!(
        row_path,
        Some(main_canonical.clone()),
        "and the row is the family ROOT: {registry}"
    );
    assert!(
        !registry.contains(&format!("{}\"", member.display())),
        "the member is not a row of its own: {registry}"
    );

    // The dashboard's own registry agrees, and lists the member as a derived
    // child rather than a second project.
    let (code, listed) = f.rift(&member, &["projects", "list", "--json"]);
    assert_eq!(code, 0, "{listed}");
    let doc: serde_json::Value = serde_json::from_str(&listed).unwrap();
    let rows = doc["projects"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "one registered project: {doc}");
    assert_eq!(rows[0]["path"], main_canonical.display().to_string());

    // Falsification: the main work tree's own `init` finds the row already
    // there. A member row would have let this one add a second.
    let (code, again) = f.rift(&f.main, &["init", "--no-recall"]);
    assert_eq!(code, 0, "init from main: {again}");
    assert!(
        again.contains("already registered"),
        "the family's row is the one the first run wrote: {again}"
    );
    let registry = f.registry_text();
    assert_eq!(
        registry.matches("[[projects]]").count(),
        1,
        "still exactly one row: {registry}"
    );
}

/// ★ The half-initialised `init` (VERIFY B): a native file Rift does not own
/// is a printed refusal and a non-zero exit at the END, never an abort that
/// leaves a repository provisioned halfway.
///
/// The wiring loop used to `return Err` on the first agent that refused — and
/// it runs AFTER the registry row, the `.rift/` scaffold, the projection and
/// the git hooks. A developer whose `.rift/agents/claude-code.json` was a file
/// of their own therefore got a repository with hooks installed, no Recall
/// step ever run, and one error line to explain it; `rift doctor` then read the
/// result as a real installation.
///
/// The assertion that states the rule is the last one: the step AFTER the loop
/// ran. Exit 4 and the printed refusal say the failure was not swallowed.
#[test]
fn a_refused_agent_is_reported_and_init_still_finishes() {
    let f = Family::new();
    // Somebody else's file, exactly where Rift's descriptor goes.
    std::fs::create_dir_all(f.main.join(".rift/agents")).unwrap();
    std::fs::write(
        f.main.join(".rift/agents/claude-code.json"),
        b"{\"mine\": true}\n",
    )
    .unwrap();

    // Detection is the executable on PATH; without one there is nothing to
    // refuse and this test would pass for the wrong reason.
    let bin = f.home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("claude"), b"#!/bin/sh\nexit 0\n").unwrap();
    // Joined with the platform's separator: Windows splits `PATH` on `;`,
    // so a `:`-joined value would never be searched.
    let mut paths = vec![bin.clone()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(paths)
        .expect("join fixture PATH")
        .into_string()
        .expect("a UTF-8 PATH");

    let main = f.main.clone();
    let ((code, stdout), stderr) = f.rift_with(&main, &["init", "--no-recall"], &[("PATH", path)]);
    let report = format!("{stdout}{stderr}");
    assert_eq!(code, 4, "a refusal is a precondition exit: {report}");
    assert!(
        report.contains("not wired — claude-code"),
        "the refusal names the agent and the file: {report}"
    );
    assert!(
        main.join(".git/hooks/post-commit").exists(),
        "the git hooks the loop runs after were installed"
    );
    assert!(
        report.contains("recall: skipped"),
        "and the step AFTER the loop still ran — the abort is gone: {report}"
    );
    assert_eq!(
        std::fs::read(main.join(".rift/agents/claude-code.json")).unwrap(),
        b"{\"mine\": true}\n",
        "the file Rift does not own is byte-identical"
    );
}

/// ★ `rift projects list --worktrees` lists the family's members
/// under their repository, and the bare command's output does not move.
///
/// Driven through the production argv path — clap parses the flag, the same
/// renderer draws both — because that is the half a renderer test cannot hold:
/// a flag wired to nothing renders nothing.
///
/// Both outputs are **byte-locked**. The default one is what the requirement
/// spells out ("default output unchanged"), and a `contains` assertion would
/// have let a member row join it unnoticed; the `--worktrees` one pins the
/// indentation, which is the whole of "under their root" on a flat list.
#[test]
fn the_worktrees_flag_lists_members_under_their_root_and_leaves_the_bare_list_alone() {
    let f = Family::new();
    let main = std::fs::canonicalize(&f.main).unwrap();
    let member = std::fs::canonicalize(&f.member).unwrap();

    let (code, out) = f.rift(&f.main, &["init", "--no-recall"]);
    assert_eq!(code, 0, "init from main: {out}");

    let (code, bare) = f.rift(&f.main, &["projects", "list"]);
    assert_eq!(code, 0, "{bare}");
    assert_eq!(
        bare,
        format!("  {}  (main)\n", main.display()),
        "the default output is the registered rows and nothing else"
    );

    let (code, listed) = f.rift(&f.main, &["projects", "list", "--worktrees"]);
    assert_eq!(code, 0, "{listed}");
    assert_eq!(
        listed,
        format!(
            "  {}  (main)\n    {}  (member)\n",
            main.display(),
            member.display()
        ),
        "the member follows its repository, indented under it"
    );

    let (code, json) = f.rift(&f.main, &["projects", "list", "--worktrees", "--json"]);
    assert_eq!(code, 0, "{json}");
    let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
    let rows = doc["projects"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{doc}");
    assert_eq!(rows[0]["kind"], "repo");
    assert_eq!(rows[1]["kind"], "worktree");
    assert_eq!(rows[1]["path"], member.display().to_string());
}
