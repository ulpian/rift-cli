//! The spawn builders every `rift` CLI suite drives the binary and git with:
//! the hermetic `rift` wrapper over `common::rift_cmd`, the git invocation a
//! developer's own configuration and environment cannot steer, and the
//! fixtures that make a repository answerable — a scratch gitdir, the
//! machine-local provisioning `.rift/` alone never proves, and the two commit
//! helpers whose pinning is what makes a frozen oid a constant of the tree.
//! One module rather than a copy per suite, so a fixture change reaches every
//! reader at once. Each suite includes this file by `#[path]` and uses a
//! subset of the builders, which is why each declaration carries
//! `#[allow(dead_code)]`: the allow names a fact about the include, not a
//! suppression.

use std::path::Path;
use std::process::{Command, Output};

use super::common;

pub(crate) const RETIRED_BRAND: &str = concat!("ent", "ire");

/// Run `rift <args>` hermetically (config + cache redirected; offline; headless).
pub(crate) fn rift(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    rift_with_detected_agents(home, cwd, args, &[])
}

/// Run Rift with a deterministic set of compatible agent executables visible.
/// Native detection only requires a project-local executable-shaped file; the
/// fixtures are never invoked.
pub(crate) fn rift_with_detected_agents(
    home: &Path,
    cwd: &Path,
    args: &[&str],
    binaries: &[&str],
) -> Output {
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

pub(crate) fn code(out: &Output) -> i32 {
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
pub(crate) fn hermetic_git(cmd: &mut Command, home: &Path) {
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
pub(crate) fn git_init(path: &Path) {
    let mut cmd = Command::new("git");
    cmd.args(["init", "-q", "--object-format=sha1"])
        .current_dir(path);
    hermetic_git(&mut cmd, &path.join("no-such-home"));
    let status = cmd.status().expect("spawn git init");
    assert!(status.success());
}

/// A git invocation that cannot be steered by the developer's own config *or*
/// environment: no system file, no global file, identity and signing pinned on
/// the argv, and the re-targeting `GIT_*` variables scrubbed by [`hermetic_git`]
/// — including `GIT_TEMPLATE_DIR`, which would otherwise run a hostile hook
/// inside every `init` and `commit` below.
pub(crate) fn git(home: &Path, cwd: &Path, args: &[&str]) -> Output {
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
pub(crate) fn rift_dir(home: &Path, repo: &Path) {
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    common::provision_here(repo, &home.join("cache"));
}

/// `git commit` with both dates pinned, so the commit oid — and therefore any
/// golden that contains it — is a constant of the fixture, not of the wall
/// clock. Everything else routes through [`git`]/[`hermetic_git`] unchanged.
pub(crate) fn git_commit_at(home: &Path, cwd: &Path, date: &str, msg: &str) -> Output {
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
pub(crate) fn git_commit_pinned(home: &Path, cwd: &Path, date: &str, msg: &str) -> Output {
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
