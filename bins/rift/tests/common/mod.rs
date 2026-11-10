//! Hermetic-spawn chokepoint for every test that runs the real `rift` binary.
//!
//! **Why this exists.** The agent-hook surface
//! (`rift hooks <agent> <verb>`) resolves its repository from the process
//! working directory (`capture_repo` walks up from cwd), and the session
//! journal used to live under the DISCOVERED git dir — `.git/rift-sessions/…`,
//! or the per-worktree admin dir for a linked worktree — where
//! `RIFT_CACHE_DIR` never covered it. It now lives in the family's run state
//! under the cache root ([`run_state_dir`]), but the class below is still
//! the cwd's to close: the family is the cwd's repository's.
//! A `SessionStart` spawned by this suite without a pinned cwd inherited the
//! test runner's cwd inside the real repository, wrote `01JE2E.json` into the
//! real per-worktree journal, and the next real `git commit`'s post-commit
//! hook swept that live fixture session into the repository's immutable
//! checkpoint history (shard `51/4865…`, anchored on commit `e2f1dbb`,
//! 2026-08-16T14:39:29Z). The shard stays — canonical history is append-only.
//! What this module removes is the CLASS:
//!
//! * every spawn of the binary under test goes through [`rift_cmd`], which
//!   takes a **mandatory** working directory and panics on one inside the
//!   repository under test — repo discovery walks *up*, so a cwd outside the
//!   real repo can never resolve into it;
//! * [`rift_cmd`] also scrubs the `GIT_*` re-targeting environment, closing
//!   the env fall-through half of the same class (an inherited `GIT_DIR`
//!   re-aims a child regardless of cwd); a test that needs a specific git
//!   dir passes `--git-dir` explicitly;
//! * [`only_the_chokepoint_names_the_binary`] keeps this the only door: a
//!   future test naming `CARGO_BIN_EXE_rift` anywhere else goes red;
//! * [`rift_cmd`] names a config root under the system temp dir
//!   ([`test_config_root`]), which a test may override with its own. The
//!   config root holds the per-family column keys (`rift/keys/`) and the
//!   project registry, and a spawn that inherited the developer's environment
//!   minted a key file in their real `~/.config/rift/keys/` for every fixture
//!   family. A spawn that runs the binary by another door (an installed git
//!   hook entry) names it itself, and [`no_spawn_sets_a_cache_root_alone`]
//!   holds that.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The one place the binary under test may be named.
const RIFT: &str = env!("CARGO_BIN_EXE_rift");

/// Environment that re-targets a child git/rift to a repository other than
/// the one its cwd names. Scrubbed on every spawn.
const RETARGETING_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_CEILING_DIRECTORIES",
];

/// The repository this test suite itself lives in — the tree a spawned child
/// must NEVER resolve as its capture repository. Walks up from the manifest
/// dir to the first ancestor carrying a `.git` entry (a linked worktree's
/// `.git` is a *file*, so this checks existence, not directory-ness).
pub(crate) fn repo_under_test() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if dir.join(".git").exists() {
            return dir;
        }
        assert!(
            dir.pop(),
            "no enclosing repository above CARGO_MANIFEST_DIR"
        );
    }
}

/// Panic unless `cwd` is hermetic: an existing directory strictly outside
/// [`repo_under_test`]. Discovery walks up, so "outside" is sufficient — a
/// child started there can never land its journal in the real repository.
pub(crate) fn assert_hermetic_cwd(cwd: &Path) {
    let repo = repo_under_test()
        .canonicalize()
        .expect("repository under test resolves");
    assert!(cwd.exists(), "spawn cwd {} must exist", cwd.display());
    let cwd = cwd.canonicalize().expect("spawn cwd canonicalizes");
    assert!(
        !cwd.starts_with(&repo),
        "test would spawn `rift` with cwd INSIDE the repository under test\n  \
         cwd:  {}\n  repo: {}\n\
         Exactly this shape wrote fixture session 01JE2E into the\n\
         real session journal, and the real post-commit hook archived it into\n\
         immutable checkpoint history (shard 51/4865…). Spawn from a tempdir.",
        cwd.display(),
        repo.display()
    );
}

/// The binary's path, for assertions on shim CONTENT (never for spawning —
/// spawns go through [`rift_cmd`], which the lexical guard enforces).
#[allow(dead_code)] // used by doc_helper_seam, hooks_e2e and post_checkout_e2e only; every suite that includes this module is its own crate
pub(crate) fn rift_bin() -> &'static str {
    RIFT
}

/// A path as a POSIX command line spells it: the verbatim prefix dropped and
/// `/` throughout. The shell-read grammar refuses `\` (a shell metacharacter)
/// and `?` (a glob) by design, so a Windows `display()` would never be a
/// read; `C:/Users/…` is still absolute to `Path` and canonicalises to the
/// same file. Fixture-side only: the grammar is not widened for it.
#[allow(dead_code)] // not every suite spells a path for a shell or a transcript dir; this module is shared
pub(crate) fn shell_spelling(path: &Path) -> String {
    let spelled = path.display().to_string();
    let plain = spelled.strip_prefix(r"\\?\").unwrap_or(&spelled);
    plain.replace('\\', "/")
}

/// Claude Code's per-project transcript directory for `work_tree` under the
/// redirected `home`, folded by the binary's own rule: every UTF-16 code unit
/// outside `[A-Za-z0-9]` becomes `-`, and a name over 200 units keeps its
/// first 200, then `-` and the base-36 absolute value of the path's Java
/// string hash.
///
/// One copy for every suite, because two fixtures that each folded only the
/// characters their author's temporary directory happened to contain went
/// red on a runner whose temporary directory carried a `_`. The binary
/// discovers the work tree canonicalized (macOS `/var` → `/private/var`), so
/// the fold runs over the canonical path, spelled plainly first so the name
/// stays relative on Windows too.
#[allow(dead_code)] // only the suites that plant an agent transcript call it; this module is shared
pub(crate) fn claude_code_project_dir(home: &Path, work_tree: &Path) -> PathBuf {
    const MAX_UNITS: usize = 200;
    let path = shell_spelling(&work_tree.canonicalize().unwrap());
    let folded: String = path
        .encode_utf16()
        .map(|unit| match u8::try_from(unit) {
            Ok(byte) if byte.is_ascii_alphanumeric() => char::from(byte),
            _ => '-',
        })
        .collect();
    let name = match folded.get(..MAX_UNITS) {
        Some(prefix) if folded.len() > MAX_UNITS => {
            let hash = path.encode_utf16().fold(0_i32, |hash, unit| {
                hash.wrapping_mul(31).wrapping_add(i32::from(unit))
            });
            format!("{prefix}-{}", base36(hash.unsigned_abs()))
        }
        _ => folded,
    };
    home.join(".claude").join("projects").join(name)
}

/// `Number.prototype.toString(36)` for a non-negative integer: lowercase.
fn base36(mut value: u32) -> String {
    let mut digits = Vec::new();
    loop {
        digits.extend(char::from_digit(value % 36, 36));
        value /= 36;
        if value == 0 {
            break;
        }
    }
    digits.iter().rev().collect()
}
/// THE spawn chokepoint: a `Command` for the binary under test, pinned to a
/// hermetic working directory, with the re-targeting environment scrubbed
/// and the config root pinned to [`test_config_root`].
/// Layer stdio/env/args on the returned builder as the test needs; a later
/// `.env("RIFT_CONFIG_DIR", ..)` replaces the default.
pub(crate) fn rift_cmd(cwd: &Path) -> Command {
    assert_hermetic_cwd(cwd);
    let mut cmd = Command::new(RIFT);
    cmd.current_dir(cwd);
    for k in RETARGETING_ENV {
        cmd.env_remove(k);
    }
    cmd.env("RIFT_CONFIG_DIR", test_config_root());
    cmd
}

/// The config root every spawn of this test process names unless the test
/// names its own: one directory under the system temp dir, created once.
///
/// One per process, not per test, because [`rift_cmd`] is handed only a cwd.
/// That is enough to keep the developer's config out of reach: each test's
/// family is minted in its own temp repository, so the key files under it
/// never collide, and the tests that read the registry back name their own.
pub(crate) fn test_config_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        tempfile::Builder::new()
            .prefix("rift-e2e-config-")
            .tempdir()
            .expect("the test process's config root is created under the system temp dir")
            .keep()
    })
}

/// Set the repository up **on this machine** the way `rift init` leaves it —
/// the family minted, its capture projection created under `cache_root` —
/// and nothing else: no registry row, no git hooks, no agent wiring, each of
/// which would reach outside the fixture (a hook installed here would run the
/// binary at the fixture's next `git commit` with git's own environment, and
/// land a projection in the developer's real cache root).
///
/// "Initialized" is this file existing, never `.rift/` existing
/// (docs/guide/02, "Initialized is a fact about this machine"), so a fixture
/// that wants the query surface — `blame`, `why`, `session attach`, `resume`,
/// `import` — opts in here, with the same cache root it hands the binary.
#[allow(dead_code)] // the query-surface fixtures only; this module is shared
pub(crate) fn provision_here(repo: &Path, cache_root: &Path) {
    let paths = rift_git::discover(repo).expect("a repository to provision");
    // The family dir override is the one environment fact minting reads; the
    // fixtures leave it unset for the binary, so it is unset here too.
    let state =
        rift_git::family::resolve_family_under(|_| None, &paths, rift_git::MintPolicy::Allow)
            .expect("the family resolves");
    let family = match state {
        rift_git::FamilyState::Minted(family) => family,
        other => unreachable!(
            "the family at {} cannot be minted: {other:?}",
            repo.display()
        ),
    };
    let db = rift_store::projection_db_for_family_under(cache_root, &family);
    std::fs::create_dir_all(db.parent().expect("the projection has a directory"))
        .expect("create the projection directory");
    drop(rift_store::open_capture_projection(&db).expect("the projection opens"));
}

/// Where the binary under test keeps `root` of the member whose git dir is
/// `git_dir`, given the cache root it was handed: the family's run state,
/// `<cache_root>/rift/<family_id>/run/<member>/`.
///
/// Mints the family when the fixture has not yet, exactly as the binary's
/// first resolution would, so a fixture may seed run state before the first
/// spawn.
#[allow(dead_code)] // the run-state suites only; this module is shared
pub(crate) fn run_state_dir(
    git_dir: &Path,
    cache_root: &Path,
    root: rift_shadow::hooks::run_state::RunRoot,
) -> PathBuf {
    let git_dir = git_dir.canonicalize().expect("the git dir resolves");
    let work_tree = rift_shadow::hooks::session_cache::work_tree_of(&git_dir)
        .expect("the git dir names a work tree");
    // From the git dir when the checkout is gone, as the binary resolves it.
    let paths = rift_git::discover(&work_tree)
        .or_else(|_| rift_git::discover(&git_dir))
        .expect("a repository");
    let state =
        rift_git::family::resolve_family_under(|_| None, &paths, rift_git::MintPolicy::Allow)
            .expect("the family resolves");
    let rift_git::FamilyState::Minted(family) = state else {
        unreachable!("the family at {} cannot be minted", work_tree.display());
    };
    let family_dir = rift_store::projection_dir_under(cache_root, &family.family_id);
    root.under(&rift_shadow::hooks::run_state::member_dir(
        &family_dir,
        &work_tree,
    ))
}

/// Where a fixture seeds session caches before the binary under test has
/// run: the git dir's old tree, `<git_dir>/rift-sessions`, which is where an
/// older binary would have left them. The binary's first resolution of the
/// member moves them into [`sessions_dir`], so every suite that seeds here
/// also drives the move; a read after a run goes to [`sessions_dir`].
#[allow(dead_code)] // the seeding suites only; this module is shared
pub(crate) fn seed_sessions_dir(git_dir: &Path) -> PathBuf {
    rift_shadow::hooks::run_state::RunRoot::Sessions.legacy_under(git_dir)
}

/// The file a reader of `dir` (a run-state directory) reads for `name`, as
/// the binary reads it: the new tree's, or the old tree's while the member is
/// unmoved and only it has the file.
#[allow(dead_code)] // the run-state suites only; this module is shared
pub(crate) fn read_path(dir: &Path, name: &str) -> PathBuf {
    rift_shadow::hooks::run_state::read_path(dir, name)
}

/// [`run_state_dir`] for the session caches.
#[allow(dead_code)] // the run-state suites only; this module is shared
pub(crate) fn sessions_dir(git_dir: &Path, cache_root: &Path) -> PathBuf {
    run_state_dir(
        git_dir,
        cache_root,
        rift_shadow::hooks::run_state::RunRoot::Sessions,
    )
}

/// [`run_state_dir`] for the agent-hook job journal.
#[allow(dead_code)] // the run-state suites only; this module is shared
pub(crate) fn agent_queue_dir(git_dir: &Path, cache_root: &Path) -> PathBuf {
    run_state_dir(
        git_dir,
        cache_root,
        rift_shadow::hooks::run_state::RunRoot::AgentQueue,
    )
}

/// [`run_state_dir`] for the post-commit capture queue.
#[allow(dead_code)] // the run-state suites only; this module is shared
pub(crate) fn capture_queue_dir(git_dir: &Path, cache_root: &Path) -> PathBuf {
    run_state_dir(
        git_dir,
        cache_root,
        rift_shadow::hooks::run_state::RunRoot::CaptureQueue,
    )
}

/// Recursively collect `.rs` files under `dir` that contain `needle`.
fn files_containing(dir: &Path, needle: &str, hits: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read tests dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_containing(&path, needle, hits);
        } else if path.extension().is_some_and(|e| e == "rs")
            && std::fs::read_to_string(&path).is_ok_and(|s| s.contains(needle))
        {
            hits.push(path);
        }
    }
}

/// **The guard on the guard**: the binary under test is nameable only here.
/// Every other naming of `CARGO_BIN_EXE_rift` under `tests/` is a spawn that
/// could dodge [`rift_cmd`]'s hermeticity check — the exact hole that leaked
/// session 01JE2E — so its appearance anywhere else is a failure.
#[test]
fn only_the_chokepoint_names_the_binary() {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut hits = Vec::new();
    // Split literal so this file's own scan string is not itself a hit shape
    // in other files' error output.
    let needle = concat!("CARGO_BIN_", "EXE_rift");
    files_containing(&tests, needle, &mut hits);
    let offenders: Vec<_> = hits
        .iter()
        .filter(|p| !p.ends_with("common/mod.rs"))
        .collect();
    assert!(
        offenders.is_empty(),
        "the rift binary may only be spawned via common::rift_cmd (hermetic \
         chokepoint); raw namings found in: {offenders:?}"
    );
    assert!(
        hits.iter().any(|p| p.ends_with("common/mod.rs")),
        "scan self-check: the chokepoint itself must be found"
    );
}

/// The builders that pin the config root themselves: the chokepoint, and the
/// installed-hook runner `hooks_e2e` keeps for the one door it cannot use.
const CONFIG_PINNING_BUILDERS: [&str; 2] = ["rift_cmd(", "hook_command("];

/// How far above an `.env(..)` line its builder may start, and how near a
/// `RIFT_CONFIG_DIR` must sit to count as set on the same builder.
const BUILDER_REACH: usize = 30;
const CONFIG_NEARBY: usize = 4;

/// The `.env("RIFT_CACHE_DIR", ..)` lines of `text` (1-based) whose builder
/// does not pin the config root and that set none of their own nearby.
fn cache_without_config(text: &str) -> Vec<usize> {
    let lines: Vec<&str> = text.lines().collect();
    let needle = concat!(".env(\"RIFT_", "CACHE_DIR\"");
    let config = concat!("\"RIFT_", "CONFIG_DIR\"");
    let mut offenders = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        if !line.contains(needle) {
            continue;
        }
        let builder = lines[at.saturating_sub(BUILDER_REACH)..=at]
            .iter()
            .rev()
            .find_map(|l| {
                ["Command::new("]
                    .iter()
                    .chain(CONFIG_PINNING_BUILDERS.iter())
                    .find(|b| l.contains(*b))
            });
        if builder.is_some_and(|b| CONFIG_PINNING_BUILDERS.contains(b)) {
            continue;
        }
        let near =
            &lines[at.saturating_sub(CONFIG_NEARBY)..(at + CONFIG_NEARBY + 1).min(lines.len())];
        if !near.iter().any(|l| l.contains(config)) {
            offenders.push(at + 1);
        }
    }
    offenders
}

/// **No spawn names a cache root and leaves the config root to the
/// developer's environment.** [`rift_cmd`] pins the config root, so a
/// builder it started is covered; any other builder that sets
/// `RIFT_CACHE_DIR` (a `git` whose hooks run the binary, an installed hook
/// entry) must set `RIFT_CONFIG_DIR` beside it, or the binary it reaches
/// mints its family's column key in the real `~/.config/rift/keys/`.
#[test]
fn no_spawn_sets_a_cache_root_alone() {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut hits = Vec::new();
    files_containing(&tests, concat!("RIFT_", "CACHE_DIR"), &mut hits);
    let offenders: Vec<String> = hits
        .iter()
        .flat_map(|path| {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            cache_without_config(&text)
                .into_iter()
                .map(move |line| format!("{}:{line}", path.display()))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "a spawn sets RIFT_CACHE_DIR without RIFT_CONFIG_DIR and is not built by \
         rift_cmd: {offenders:?}"
    );
}

/// Calibration, both arms of the screen above, and the chokepoint's own pin.
#[test]
fn the_config_screen_flags_a_bare_builder_and_admits_the_chokepoint() {
    let bare = concat!(
        "let out = Command::new(\"git\")\n",
        "    .env(\"RIFT_",
        "CACHE_DIR\", &cache)\n",
        "    .output();\n"
    );
    assert_eq!(cache_without_config(bare), vec![2]);
    let paired = format!("{bare}    .env(\"RIFT_{}\", &conf)\n", "CONFIG_DIR");
    assert!(cache_without_config(&paired).is_empty());
    let pinned = bare.replace("Command::new(\"git\")", "rift_cmd(repo)");
    assert!(cache_without_config(&pinned).is_empty());

    let tmp = tempfile::tempdir().expect("tempdir");
    let cmd = rift_cmd(tmp.path());
    let pinned_root = cmd
        .get_envs()
        .find(|(k, _)| *k == "RIFT_CONFIG_DIR")
        .and_then(|(_, v)| v);
    assert_eq!(pinned_root, Some(test_config_root().as_os_str()));
    assert!(test_config_root().starts_with(std::env::temp_dir()));
}

/// Calibration, both arms: a tempdir is admitted; the repository under test —
/// the leaked spawn's exact shape — is refused.
#[test]
fn the_guard_refuses_the_repo_under_test_and_admits_a_tempdir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert_hermetic_cwd(tmp.path());

    let inside = repo_under_test();
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_hermetic_cwd(&inside);
    }))
    .is_err();
    assert!(refused, "a cwd inside the repository under test must red");
}
