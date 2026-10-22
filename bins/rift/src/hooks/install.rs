//! `rift hook install`: writing the absolute-path git-hook entries into the
//! directory git runs hooks from, removing the entries Rift wrote there under
//! names it no longer installs, and the family-site bookkeeping that records
//! which binary a family's hooks point at. Separate because it is the
//! one surface that edits a checkout's hook directory; the git hooks the rest
//! of this tree answers are the entries it wrote.

use std::path::{Path, PathBuf};

use rift_core::Result;
use rift_shadow::hooks::{
    install_hooks_with, FamilySites, HookInstall, HookInstallPolicy, HookInstallReport, RecordedBy,
    TransientReason,
};

use super::git::capture_sync_note;
use super::{current_dir_or_dot, flag, projection_db_at, resolve_hooks_git_dir};

/// The binary a command runs as, and the screen that decides whether a family
/// may be moved to it.
///
/// The screen is a field so tests can name a binary in a temp dir as if it
/// were a release install: every fixture lives under a temporary root, which
/// the real screen refuses.
pub(crate) struct RunningBinary {
    pub(crate) path: PathBuf,
    pub(crate) screen: fn(&Path, &FamilySites) -> Option<TransientReason>,
}

impl RunningBinary {
    /// This process, under the real transient screen.
    pub(crate) fn current() -> Result<Self> {
        Ok(Self {
            path: std::env::current_exe()?,
            screen: rift_shadow::hooks::is_transient,
        })
    }
}

/// `rift hook install [--repoint] [--binary PATH] [--allow-transient]`: write
/// the absolute-path entries into the directory git runs hooks from.
///
/// Plain, it is as conservative as `rift init` — a family whose hooks already
/// work keeps them — silently re-pointing a working install is a footgun.
/// `--repoint` is how a developer moves the
/// family between a debug and a release build on purpose, and the only spelling
/// that will.
pub(super) fn cmd_install(args: &[String]) -> Result<()> {
    let cwd = current_dir_or_dot();
    let git_dir = hooks_git_dir_or_refusal(flag(args, "--git-dir").map(PathBuf::from), &cwd)?;
    let hooks_dir = flag(args, "--hooks-dir").map(PathBuf::from);
    let rift_bin = requested_binary(args)?;
    let work_tree = rift_shadow::hooks::session_cache::work_tree_of(&git_dir)
        .unwrap_or_else(current_dir_or_dot);
    // `--repoint` is the only way to move a working family's binary, and
    // `--allow-transient` the only way past the screen that refuses a binary
    // living in a checkout that may vanish. Without them this command is as
    // conservative as `rift init`.
    let allow_transient = args.iter().any(|arg| arg == "--allow-transient");
    let policy = if args.iter().any(|arg| arg == "--repoint") {
        HookInstallPolicy::Repoint { allow_transient }
    } else {
        HookInstallPolicy::KeepIfHealthy { allow_transient }
    };
    let report = install_family_hooks(
        &git_dir,
        &work_tree,
        hooks_dir,
        rift_bin,
        policy,
        RecordedBy::HooksInstall,
    )?;
    // Asked for in so many words, a refusal is an error rather than a line:
    // the person typed a binary and has to know it is not the one in force.
    if let Some(refused) = report.refused {
        return Err(rift_core::RiftError::validity(refused.to_string()));
    }
    for line in report_lines(&report) {
        println!("rift: {line}");
    }
    // The paths themselves belong to this command and not to the summary a
    // `rift init` folds into its own report: someone who typed `hooks install`
    // is looking for exactly where the entries went.
    for path in report.written().iter().chain(report.kept.iter()) {
        println!("  {}", path.display());
    }
    for line in warm_capture_state(&work_tree) {
        println!("  {line}");
    }
    // Named here because this is the moment someone is wiring capture up, and
    // because a shipped variable that only exists in a source file is a
    // variable nobody can use and nobody can audit.
    println!("  {}", capture_sync_note());
    Ok(())
}

/// The hooks directory to install into, or the refusal owed to a working
/// directory that is not in a repository at all.
///
/// Split out from [`cmd_install`] so the refusal can be *tested*: `cmd_install`
/// reads the process working directory, and in a unit test that directory is
/// the repository under test — a test that called it would install into the
/// developer's own repository, which is the class `bins/rift/tests/common`
/// exists to keep out of the spawned binary, and this keeps out of this one.
pub(super) fn hooks_git_dir_or_refusal(explicit: Option<PathBuf>, cwd: &Path) -> Result<PathBuf> {
    resolve_hooks_git_dir(explicit, cwd).ok_or_else(|| {
        rift_core::RiftError::NotFound(format!(
            "not inside a git repository: {} — there is nothing here to install hooks into. \
             Run this from a checkout, or pass --git-dir <dir>",
            cwd.display()
        ))
    })
}

/// The binary this install is to point at: `--binary PATH` if given, else the
/// running executable.
///
/// An explicit path is made absolute (a hook is run from git's cwd, never
/// ours) and must be an executable file: `--binary` exists so a developer can
/// name a release build, and pointing a whole family at a typo is the failure
/// this command is here to stop, not to commit.
fn requested_binary(args: &[String]) -> Result<PathBuf> {
    let Some(explicit) = flag(args, "--binary") else {
        return Ok(std::env::current_exe()?);
    };
    let path = PathBuf::from(explicit);
    let path = if path.is_absolute() {
        path
    } else {
        current_dir_or_dot().join(path)
    };
    if !rift_shadow::hooks::is_executable_file(&path) {
        return Err(rift_core::RiftError::NotFound(format!(
            "--binary {} is not an executable file",
            path.display()
        )));
    }
    Ok(path)
}

/// The hooks directory **git will actually run**, asked of git rather than
/// assumed.
///
/// `<git_dir>/hooks` is only the default. `core.hooksPath` moves it, and with
/// `extensions.worktreeConfig` one member can move it for itself alone — and
/// either leaves `<common_dir>/hooks` reading perfectly healthy while every
/// commit runs a different directory. So the install sites and the health probe
/// ask `git rev-parse --git-path hooks` through the argv-safe seam, with this
/// vantage's `GIT_DIR`, which is the same view git will take when it runs the
/// hook.
///
/// One `git` process, paid at install and diagnosis time. **No hook pays it** —
/// hooks live inside a 100 ms git-latency budget and may not spawn `git` — and a
/// repository where the answer cannot be had falls back to the default rather
/// than failing.
pub(crate) fn effective_hooks_dir(work_tree: &Path) -> Option<PathBuf> {
    let paths = rift_git::discover(work_tree).ok()?;
    let out = rift_git::run_git_capture(
        &paths,
        &[
            "rev-parse".to_owned(),
            "--git-path".to_owned(),
            "hooks".to_owned(),
        ],
        &[],
    )
    .ok()?;
    let answer = String::from_utf8(out).ok()?;
    let answer = Path::new(answer.trim());
    if answer.as_os_str().is_empty() {
        return None;
    }
    if answer.is_absolute() {
        return Some(answer.to_path_buf());
    }
    // A relative `core.hooksPath` comes back relative to the git process's own
    // working directory, which is ours: `relhooks` from the top of the work
    // tree, `../relhooks` from a subdirectory. Joining our cwd is what makes
    // git's answer the path git will run.
    Some(current_dir_or_dot().join(answer))
}

/// Install this repository **family's** git hooks.
///
/// The one place in this binary that turns "a repository" into the four facts
/// an install needs: where git will look for hooks, where the family records
/// its binary, which checkouts a binary may not be reached through, and which
/// act is asking. Every install site goes through it, so no surface can point
/// the family's hooks somewhere by accident — which is precisely what happened
/// on 2026-08-26, from a command that was not about hooks at all.
///
/// # Errors
/// [`rift_core::RiftError`] on a filesystem failure. A *refused* binary is not
/// an error: it is reported in the [`HookInstallReport`], with the hooks left
/// exactly as they were found.
pub(crate) fn install_family_hooks(
    git_dir: &Path,
    work_tree: &Path,
    hooks_dir: Option<PathBuf>,
    rift_bin: PathBuf,
    policy: HookInstallPolicy,
    recorded_by: RecordedBy,
) -> Result<HookInstallReport> {
    install_hooks_with(
        &HookInstall {
            hooks_dir: hooks_dir.or_else(|| effective_hooks_dir(work_tree)),
            family: family_sites(git_dir),
            git_dir: git_dir.to_path_buf(),
            rift_bin,
            recorded_by,
        },
        policy,
    )
}

/// Move the family's git hooks and its binary of record to the running binary
/// when the record names another one, and say what happened in one line.
///
/// `None` when there is nothing to move: no repository, no record yet (first
/// contact is `init`'s install), the record already names this binary, or the
/// command runs in a linked worktree — a member never moves the family's
/// record, because its build lives in a checkout `git worktree remove` deletes.
///
/// Hooks and record move together. Moving the record alone was undone by the
/// next `init`, which makes the record follow hooks that still work. A debug
/// build or a throwaway build never becomes the binary of record (hooks never
/// point at a debug binary); the line then says why the hooks were left alone.
pub(crate) fn follow_running_binary(work_tree: &Path, running: &RunningBinary) -> Option<String> {
    let paths = rift_git::discover(work_tree).ok()?;
    if is_linked_member(&paths) {
        return None;
    }
    follow_in_family(&paths, work_tree, running)
}

/// [`follow_running_binary`] for a git hook, which may fire in a linked member.
///
/// A member's own commands never move the record, because their build lives in
/// a checkout `git worktree remove` deletes. A git hook is different: the
/// binary it runs as is the family's hook binary, wherever the checkout is, so
/// the record follows it before the hook wires the member from the record. The
/// transient screen still refuses a binary that lives inside a member.
///
/// A hook enters through its own `.git/hooks/<name>` link, and macOS's
/// `current_exe()` keeps that spelling. The entry is never the binary — moving
/// the record to it pointed every hook at `post-checkout` and `post-checkout`
/// at itself — so the binary followed is the one the link names. One link is
/// read, not the whole path canonicalized: the target is the spelling the
/// install wrote, and a canonical one (a Homebrew `Cellar/<version>/`) is the
/// path an upgrade deletes.
pub(crate) fn follow_from_hook(
    paths: &rift_git::RepoPaths,
    running: &RunningBinary,
) -> Option<String> {
    let resolved = RunningBinary {
        path: hook_link_target(&running.path).unwrap_or_else(|| running.path.clone()),
        screen: running.screen,
    };
    follow_in_family(paths, &paths.work_tree, &resolved)
}

/// The binary a direct hook link names, `None` when `entry` is not a link (a
/// shim execs the binary by its own path).
fn hook_link_target(entry: &Path) -> Option<PathBuf> {
    let target = std::fs::read_link(entry).ok()?;
    match entry.parent() {
        Some(dir) if target.is_relative() => Some(dir.join(target)),
        _ => Some(target),
    }
}

/// The move itself, once the caller has decided this vantage may make it.
fn follow_in_family(
    paths: &rift_git::RepoPaths,
    work_tree: &Path,
    running: &RunningBinary,
) -> Option<String> {
    let sites = family_sites(&paths.common_dir);
    let record = rift_shadow::hooks::read_binary_of_record(&sites.dir)?;
    let old = record.path();
    // Two spellings of one file — `~/.local/bin/rift` and the build it links
    // to — are one binary of record, not a move.
    if same_file(old, &running.path) {
        return None;
    }
    if let Some(why) = never_of_record(running, &sites) {
        return Some(format!(
            "git hooks: left at {} because {} is {why}, which never becomes the binary of record",
            old.display(),
            running.path.display()
        ));
    }
    // The screen above already ran, with the caller's screen, so the install
    // is told not to run its own a second time.
    let policy = HookInstallPolicy::Repoint {
        allow_transient: true,
    };
    let moved = install_family_hooks(
        &paths.common_dir,
        work_tree,
        None,
        running.path.clone(),
        policy,
        RecordedBy::Repoint,
    );
    Some(match moved {
        Ok(_) => format!(
            "git hooks: moved from {} to {} with the binary of record",
            old.display(),
            running.path.display()
        ),
        Err(error) => format!("git hooks: not moved ({error})"),
    })
}

/// Whether this checkout is a linked member rather than the main work tree:
/// only a linked member has a git dir apart from the common dir.
fn is_linked_member(paths: &rift_git::RepoPaths) -> bool {
    !same_file(&paths.git_dir, &paths.common_dir)
}

/// Whether two paths name one file, compared canonically where they resolve.
fn same_file(a: &Path, b: &Path) -> bool {
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canonical(a) == canonical(b)
}

/// Why `running` may not become the family's binary of record, as a clause.
fn never_of_record(running: &RunningBinary, sites: &FamilySites) -> Option<&'static str> {
    if is_debug_build(&running.path) {
        return Some("a debug build");
    }
    (running.screen)(&running.path, sites).map(TransientReason::reason)
}

/// A binary cargo built in a debug profile (`target/debug/rift`, or
/// `target/<triple>/debug/rift`). Its build dir is the one `cargo test`
/// rewrites all day, so a family pointed at it breaks on the next edit.
pub(crate) fn is_debug_build(path: &Path) -> bool {
    path.parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "debug")
}

/// The family's manifest directory and its **linked** members, for a common dir.
///
/// `binary.json` sits beside `family.json`, so it follows the same
/// `RIFT_FAMILY_DIR` override: a hermetic run records into the override root
/// rather than into a real `.git` it does not own. The members come from the
/// family enumeration, minus the main work tree — whose `target/` is the
/// binary this repository builds for its own use, and is deliberately allowed.
pub(crate) fn family_sites(common_dir: &Path) -> FamilySites {
    let canonical = common_dir
        .canonicalize()
        .unwrap_or_else(|_| common_dir.to_path_buf());
    let dir = rift_git::family::family_dir_under(|k| std::env::var(k).ok(), &canonical);
    let linked_work_trees = rift_git::discover(&canonical).map_or_else(
        |_| Vec::new(),
        |paths| {
            rift_git::family_members(&paths)
                .into_iter()
                .filter(|member| !member.is_main)
                .map(|member| member.work_tree)
                .collect()
        },
    );
    FamilySites {
        dir,
        linked_work_trees,
    }
}

/// What an install did, as the lines a person reads.
///
/// The outcome is one line and comes first — a kept family says so, because
/// "nothing happened" is the *point* of the default policy and a command that
/// printed nothing would read as one that failed, and because `rift init`
/// folds that first line into a report of its own. A sweep of the names Rift
/// no longer installs adds a second: it is the rare line, and removing an
/// entry from a person's hooks directory is never done without saying so.
pub(crate) fn report_lines(report: &HookInstallReport) -> Vec<String> {
    let mut lines = vec![outcome_line(report)];
    if !report.removed.is_empty() {
        lines.push(format!(
            "removed {} retired git hook(s): {}",
            report.removed.len(),
            hook_names(&report.removed)
        ));
    }
    lines
}

/// Hook paths as the names git knows them by, in one clause.
pub(crate) fn hook_names(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| {
            path.file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The one line the outcome is worth: refused, kept, or written.
fn outcome_line(report: &HookInstallReport) -> String {
    let of_record = report.binary.as_ref().map_or_else(
        || " (no binary of record)".to_owned(),
        |bin| format!(" (binary of record: {})", bin.display()),
    );
    if let Some(refused) = &report.refused {
        return format!("git hooks: left alone — {refused}");
    }
    if !report.kept.is_empty() {
        return format!(
            "git hooks: {} already healthy, left alone{of_record}",
            report.kept.len()
        );
    }
    let verb = if report.repointed.is_empty() {
        "installed"
    } else {
        "re-pointed"
    };
    format!(
        "{verb} {} git hooks in {}{of_record}",
        report.written().len(),
        report.hooks_dir.display()
    )
}

/// Create, here and now, the two things capture otherwise creates lazily on a
/// developer's first commit: the per-repo projection and the per-install salt.
///
/// Both are one-time costs and neither belongs on a hook. The first commit in a
/// fresh repository measured ~0.9 s because it was creating the 405 KB
/// `projection.db`, and the salt is an OS keychain round trip (~22 ms, and a
/// *mint* the first time) that a hook pays at whatever moment a session first
/// appears. An install command is exactly where a progress line is normal.
///
/// Best-effort and quiet about it: every line returned is informational, and a
/// failure here changes nothing about correctness — the lazy path is still
/// there, still creates both, and still works. That is why it reports what it
/// did rather than returning a `Result` a caller would have to decide about.
fn warm_capture_state(work_tree: &Path) -> Vec<String> {
    let mut out = Vec::new();
    match projection_db_at(work_tree).and_then(rift_store::open_capture_projection) {
        Ok(_) => out.push("projection ready".to_string()),
        Err(e) => out.push(format!("projection deferred to the first commit ({e})")),
    }
    // Touching the salt is what mints it. The value is deliberately dropped:
    // this process has no use for it, and holding it would only widen where a
    // secret-adjacent value lives.
    let salt = crate::identity::install_salt();
    out.push("identity salt ready".to_string());
    // The machine id's one derivation (a child process on macOS) belongs
    // here too, not on a session's first hook; after this every hook reads
    // the salted digest from the cache root.
    if crate::identity::warm_machine_id(&salt) {
        out.push("machine id cached".to_string());
    }
    out
}
