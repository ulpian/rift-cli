//! A repository with a REAL N-position chain, built through the shipped
//! binary — the generalisation of `bundle_e2e::seeded_repo` (one commit) that
//! the bundle-review README's recipe describes (three commits, each followed
//! by the native post-commit hook under a distinct session marker, then
//! `rift doctor --fix` to drain the detached archive).
//!
//! Everything is a filesystem path inside the caller's temp dir; the binary
//! is spawned only through `common::rift_cmd`, so the fixture can never
//! resolve the repository under test as its capture repository.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::common::rift_cmd;

/// The chain ref the exporter bundles and the importer moves.
pub(crate) const CHAIN_REF: &str = "refs/rift/chain/v1";
/// The checkpoint branch the exporter bundles alongside the chain.
pub(crate) const CHECKPOINTS_REF: &str = "refs/heads/rift/checkpoints/v1";
/// The commit trailer each chain entry carries (sync-format, Chain ref entry).
const CHECKPOINT_TRAILER: &str = "Rift-Chain-Checkpoint:";

/// A seeded repository and the isolated cache its `rift` spawns use.
pub(crate) struct ChainFixture {
    pub(crate) repo: PathBuf,
    pub(crate) cache: PathBuf,
}

/// `git` in `dir`, hermetic (no user/system config, no re-targeting env),
/// asserting success.
pub(crate) fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = git_raw(dir, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `git` whose failure is an answer, not an assertion.
pub(crate) fn git_raw(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
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
        .expect("git is on PATH")
}

/// The shipped binary in `cwd` with an isolated cache: `(exit, stdout, stderr)`.
pub(crate) fn rift(cwd: &Path, cache: &Path, args: &[&str]) -> (i32, String, String) {
    let out = rift_cmd(cwd)
        .args(args)
        .env("RIFT_CACHE_DIR", cache)
        .env("RIFT_CONFIG_DIR", cache.join("conf"))
        .env("RIFT_HEADLESS", "1")
        .output()
        .expect("rift runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The oid a ref resolves to, or `None` when it does not exist.
pub(crate) fn ref_oid(dir: &Path, name: &str) -> Option<String> {
    let out = git_raw(dir, &["rev-parse", "--verify", "--quiet", name]);
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !text.is_empty()).then_some(text)
}

/// A git repository with no capture refs at all — the fresh clone a bundle
/// exists to reach — with a minimal `.rift/config.toml` so `rift` treats it
/// as enabled.
pub(crate) fn empty_target(tmp: &Path, name: &str) -> PathBuf {
    let dir = tmp.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    git_in(&dir, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(dir.join(".rift")).unwrap();
    std::fs::write(
        dir.join(".rift/config.toml"),
        "schema_version = 1\n[project]\nname = \"bundle-target\"\n",
    )
    .unwrap();
    dir
}

/// Build a repository whose chain holds exactly `commits` positions: one
/// commit per position, each archived through the native post-commit hook
/// under its own live-session marker and drained with `rift doctor --fix`
/// so the order is deterministic (no sleeping on the detached worker).
pub(crate) fn seeded_chain(tmp: &Path, name: &str, commits: usize) -> ChainFixture {
    assert!(commits >= 1, "a chain fixture needs at least one position");
    let repo = tmp.join(name);
    std::fs::create_dir_all(&repo).unwrap();
    git_in(&repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift/config.toml"),
        format!("schema_version = 1\n[project]\nname = \"{name}\"\n"),
    )
    .unwrap();
    let cache = tmp.join(format!("{name}-cache"));
    std::fs::create_dir_all(&cache).unwrap();
    let sessions = repo.join(".git").join("rift-sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let gd = repo.join(".git");

    for n in 1..=commits {
        std::fs::write(repo.join(format!("f{n}.txt")), format!("content {n}\n")).unwrap();
        git_in(&repo, &["add", "."]);
        // A distinct live session per commit, so each archive is a real,
        // separately-attributed condensation.
        let session = format!("chain-fixture-{n}");
        std::fs::write(
            sessions.join(format!("{session}.json")),
            format!(
                r#"{{"schema":1,"session_id":"{session}","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":true,"compaction_boundary":null,"updated_at_ms":{},"native_id_map":{{}}}}"#,
                1_783_000_000_000_u64 + n as u64
            ),
        )
        .unwrap();
        git_in(&repo, &["commit", "-q", "-m", &format!("seed {n}")]);
        let (code, _, err) = rift(
            &repo,
            &cache,
            &[
                "hooks",
                "git",
                "post-commit",
                "--git-dir",
                gd.to_str().unwrap(),
            ],
        );
        assert_eq!(code, 0, "post-commit {n} must never fail: {err}");
        let (code, out, err) = rift(&repo, &cache, &["doctor", "--fix", "--json"]);
        assert_eq!(code, 0, "doctor --fix after commit {n}: {out}{err}");
        // The session marker is consumed by the archive; a stale one would
        // be re-attributed to the next commit.
        let _ = std::fs::remove_file(sessions.join(format!("{session}.json")));
    }
    let fixture = ChainFixture { repo, cache };
    assert_eq!(
        fixture.chain_ids().len(),
        commits,
        "the fixture's chain must hold exactly one position per commit"
    );
    fixture
}

impl ChainFixture {
    /// The checkpoint ids in CHAIN ORDER (position 1 first), read from the
    /// chain ref's first-parent walk and its `Rift-Chain-Checkpoint` trailers
    /// — the one order the sync format permits.
    pub(crate) fn chain_ids(&self) -> Vec<String> {
        if ref_oid(&self.repo, CHAIN_REF).is_none() {
            return Vec::new();
        }
        let log = git_in(
            &self.repo,
            &[
                "log",
                "--first-parent",
                "--reverse",
                "--format=%B%x00",
                CHAIN_REF,
            ],
        );
        log.split('\0')
            .filter_map(|body| {
                body.lines()
                    .find_map(|l| l.strip_prefix(CHECKPOINT_TRAILER))
                    .map(|id| id.trim().to_owned())
            })
            .collect()
    }

    /// The stream cursor naming chain position `seq` (1-based), or the
    /// origin for 0 — spelled exactly as the sync format's cursor entry.
    pub(crate) fn cursor_at(&self, seq: usize) -> String {
        if seq == 0 {
            return "rift-cursor/1:0:".to_owned();
        }
        let ids = self.chain_ids();
        let id = ids
            .get(seq - 1)
            .unwrap_or_else(|| panic!("the chain holds no position {seq}"));
        format!("rift-cursor/1:{seq}:{id}")
    }

    /// `rift --json bundle export [--since <cursor>] --out <path>`, asserting
    /// success and returning the command's JSON document.
    pub(crate) fn export(&self, since: Option<&str>, out: &Path) -> serde_json::Value {
        let out_s = out.to_str().unwrap();
        let mut args = vec!["--json", "bundle", "export"];
        if let Some(c) = since {
            args.extend(["--since", c]);
        }
        args.extend(["--out", out_s]);
        let (code, stdout, err) = rift(&self.repo, &self.cache, &args);
        assert_eq!(code, 0, "export failed: {stdout}{err}");
        serde_json::from_str(&stdout).expect("export prints JSON")
    }
}

/// Copy a repository directory tree (a target the shipped importer will
/// mutate, cloned per case so cases never see each other's refs).
pub(crate) fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}
