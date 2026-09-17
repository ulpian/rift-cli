//! Counting the child processes a measured verb launches.
//!
//! A wall time says how long a hook took; it does not say what the time was
//! made of. The `hook-session-start` row is the case in point: 488.54 ms of it
//! was almost fully *process creation*, because the identity derivation
//! asked for three signals eight times and every ask was a `git config` or an
//! `ioreg`. That is invisible in a latency number and obvious in a count.
//!
//! The count is taken the way a black-box harness has to take it: a directory
//! of shims goes first on the child's `PATH`, one script per observed program.
//! Each shim appends its own name to a log and then `exec`s the **absolute**
//! path of the real program (never a `PATH` lookup, or it would re-enter
//! itself), so the child under test behaves exactly as it would unshimmed —
//! only slower, by one `sh` exec per spawn.
//!
//! Two rules keep it honest, both inherited from [`crate::rss`]:
//!
//! * the probe runs in its **own untimed pass**, never inside a latency sample;
//! * a host where the shims cannot be built (no `/bin/sh`, an unresolvable
//!   program) reports the count as *unavailable* rather than as zero. Zero
//!   spawns is a real and excellent result, so it must never be the way a
//!   failed measurement looks.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use crate::{
    hermetic::Hermetic,
    measure::{run_once, Invocation},
    report::SpawnCount,
};

/// The programs whose invocations are counted.
///
/// `git` and `ioreg` are the two the identity derivation reaches for on this
/// platform (`ioreg` prints the machine fingerprint; Linux reads a file instead
/// and so has no third shim to build). A program that does not resolve here is
/// simply not shimmed, and its absence is visible in the published breakdown.
pub const OBSERVED: &[&str] = &["git", "ioreg"];

/// Where a shim writes its record.
const LOG_VAR: &str = "RIFT_BENCH_SPAWN_LOG";

/// A prepared set of counting shims.
#[derive(Debug, Clone)]
pub struct SpawnProbe {
    /// Directory prepended to the child's `PATH`.
    shim_dir: PathBuf,
    /// The append-only log the shims write to.
    log: PathBuf,
    /// The programs actually shimmed, with the real binary each forwards to.
    shimmed: BTreeMap<String, PathBuf>,
    /// `PATH` as the harness inherited it, kept so the shims are a *prefix*
    /// rather than a replacement.
    inherited_path: String,
}

impl SpawnProbe {
    /// Build shims for `programs` under `root`, or `None` when this host cannot
    /// host them (no `/bin/sh`, nothing resolvable, unwritable root).
    #[must_use]
    pub fn prepare(root: &Path, programs: &[&str], path_var: Option<&str>) -> Option<Self> {
        if !Path::new("/bin/sh").exists() {
            return None;
        }
        let inherited_path = path_var
            .map(str::to_owned)
            .or_else(|| std::env::var("PATH").ok())?;
        let shim_dir = root.join("spawn-shims");
        fs::create_dir_all(&shim_dir).ok()?;
        let log = root.join("spawns.log");

        let mut shimmed = BTreeMap::new();
        for program in programs {
            let Some(real) = resolve(program, &inherited_path) else {
                continue;
            };
            if write_shim(&shim_dir, program, &real).is_ok() {
                shimmed.insert((*program).to_owned(), real);
            }
        }
        (!shimmed.is_empty()).then_some(Self {
            shim_dir,
            log,
            shimmed,
            inherited_path,
        })
    }

    /// The programs this probe can count.
    #[must_use]
    pub fn shimmed(&self) -> Vec<&str> {
        self.shimmed.keys().map(String::as_str).collect()
    }

    /// Count the child processes **one** invocation of `inv` launches.
    ///
    /// `None` when the invocation itself could not be run, or exited non-zero:
    /// a count taken from a failed run describes nothing.
    #[must_use]
    pub fn count(&self, hermetic: &Hermetic, inv: &Invocation) -> Option<SpawnCount> {
        // Start from an empty log, so the count belongs to this invocation
        // alone and a previous pass cannot inflate it.
        fs::write(&self.log, b"").ok()?;
        let mut shimmed = inv.clone();
        shimmed.env.push((
            "PATH".to_owned(),
            format!("{}:{}", self.shim_dir.display(), self.inherited_path),
        ));
        shimmed
            .env
            .push((LOG_VAR.to_owned(), self.log.display().to_string()));
        let exec = run_once(hermetic, &shimmed).ok()?;
        if !exec.sample.ok() {
            return None;
        }
        Some(self.tally(&fs::read_to_string(&self.log).ok()?))
    }

    /// Turn a shim log into a count, keeping every shimmed program in the
    /// breakdown — a program that was watched and never ran is a `0`, which is
    /// information; a program that was never watched is absent, which is a
    /// different statement.
    #[must_use]
    fn tally(&self, log: &str) -> SpawnCount {
        let mut by_program: BTreeMap<String, u64> =
            self.shimmed.keys().map(|k| (k.clone(), 0)).collect();
        let mut total = 0;
        for line in log.lines() {
            let name = line.trim();
            if name.is_empty() {
                continue;
            }
            if let Some(slot) = by_program.get_mut(name) {
                *slot += 1;
                total += 1;
            }
        }
        SpawnCount { total, by_program }
    }

    /// Human-readable state for the report.
    #[must_use]
    pub fn describe(&self) -> String {
        format!("PATH shims for {}", self.shimmed().join(", "))
    }
}

/// First match for `program` on `path`, as an absolute path. Only executable
/// regular files count, so a directory of the same name is not "found".
#[must_use]
fn resolve(program: &str, path: &str) -> Option<PathBuf> {
    path.split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join(program))
        .find(|candidate| is_executable(candidate))
}

#[must_use]
fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Write one counting shim: record the name, then become the real program.
fn write_shim(dir: &Path, program: &str, real: &Path) -> std::io::Result<()> {
    let target = real.display().to_string();
    // The target is single-quoted in the script; a quote in the path would end
    // the quoting and turn the rest into shell. Refuse rather than emit it.
    if target.contains('\'') || program.contains('\'') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "unquotable program path",
        ));
    }
    let path = dir.join(program);
    // `exec` replaces the shell, so the child's own pid, signals and exit code
    // are the real program's. The log append is a single short line, which is
    // atomic under `O_APPEND` — concurrent children cannot interleave halves.
    // With no log configured the shim is a pure pass-through, so a stray copy
    // on someone's `PATH` can never break the program it stands in for.
    let body = format!(
        "#!/bin/sh\nprintf '{program}\\n' >> \"${{{LOG_VAR}:-/dev/null}}\"\nexec '{target}' \"$@\"\n"
    );
    fs::write(&path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(root: &Path) -> Option<SpawnProbe> {
        SpawnProbe::prepare(root, OBSERVED, None)
    }

    #[cfg(unix)]
    #[test]
    fn a_shim_counts_every_spawn_and_stays_faithful() {
        let tmp = tempfile::tempdir().unwrap();
        let hermetic = Hermetic::prepare(tmp.path().join("hermetic")).unwrap();
        let Some(probe) = probe(tmp.path()) else {
            return; // no /bin/sh, no git: nothing to assert on this host
        };
        assert!(probe.shimmed().contains(&"git"));

        // Two spawns of a shimmed program, one of something else fully.
        let inv = Invocation::new("/bin/sh", tmp.path()).args([
            "-c",
            "git --version > /dev/null; git --version > /dev/null; /bin/echo hi > /dev/null",
        ]);
        let count = probe.count(&hermetic, &inv).expect("the shell exits 0");
        assert_eq!(count.by_program.get("git"), Some(&2));
        assert_eq!(count.total, 2, "{count:?}");

        // The shim forwards faithfully: same stdout as the real program, and a
        // second pass starts from zero rather than accumulating.
        let direct = Invocation::new("git", tmp.path()).args(["--version"]);
        let plain = run_once(&hermetic, &direct).unwrap();
        let count = probe.count(&hermetic, &direct).unwrap();
        assert_eq!(count.total, 1, "the log is truncated per pass");
        let shimmed_out = {
            let mut inv = direct.clone();
            inv.env.push((
                "PATH".to_owned(),
                format!("{}:{}", probe.shim_dir.display(), probe.inherited_path),
            ));
            inv.env
                .push((LOG_VAR.to_owned(), probe.log.display().to_string()));
            run_once(&hermetic, &inv).unwrap()
        };
        assert_eq!(shimmed_out.stdout, plain.stdout);
        assert!(shimmed_out.stdout.starts_with("git version"));
    }

    #[cfg(unix)]
    #[test]
    fn a_watched_program_that_never_ran_is_a_zero_not_an_absence() {
        let tmp = tempfile::tempdir().unwrap();
        let hermetic = Hermetic::prepare(tmp.path().join("hermetic")).unwrap();
        let Some(probe) = probe(tmp.path()) else {
            return;
        };
        let inv = Invocation::new("/bin/sh", tmp.path()).args(["-c", "exit 0"]);
        let count = probe.count(&hermetic, &inv).unwrap();
        assert_eq!(count.total, 0);
        for program in probe.shimmed() {
            assert_eq!(count.by_program.get(program), Some(&0), "{program}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_invocation_yields_no_count_at_all() {
        let tmp = tempfile::tempdir().unwrap();
        let hermetic = Hermetic::prepare(tmp.path().join("hermetic")).unwrap();
        let Some(probe) = probe(tmp.path()) else {
            return;
        };
        // It spawned something, but it also failed: a count off a failed run
        // would describe an aborted code path, not the measured one.
        let inv = Invocation::new("/bin/sh", tmp.path()).args(["-c", "git --version; exit 3"]);
        assert_eq!(probe.count(&hermetic, &inv), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_shim_without_a_log_is_a_pure_pass_through() {
        // A shim that broke its program when the log variable is unset would be
        // a benchmark that changes the thing it measures.
        let tmp = tempfile::tempdir().unwrap();
        let hermetic = Hermetic::prepare(tmp.path().join("hermetic")).unwrap();
        let Some(probe) = probe(tmp.path()) else {
            return;
        };
        let mut inv = Invocation::new("git", tmp.path()).args(["--version"]);
        inv.env.push((
            "PATH".to_owned(),
            format!("{}:{}", probe.shim_dir.display(), probe.inherited_path),
        ));
        let exec = run_once(&hermetic, &inv).unwrap();
        assert!(exec.sample.ok(), "stderr: {}", exec.stderr);
        assert!(exec.stdout.starts_with("git version"));
    }

    #[test]
    fn a_probe_with_nothing_to_shim_does_not_exist() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(SpawnProbe::prepare(tmp.path(), &["definitely-not-a-program"], None).is_none());
        // An empty PATH resolves nothing either.
        assert!(SpawnProbe::prepare(tmp.path(), OBSERVED, Some("")).is_none());
    }

    #[test]
    fn resolution_takes_the_first_executable_file_on_the_path() {
        let tmp = tempfile::tempdir().unwrap();
        let early = tmp.path().join("early");
        let late = tmp.path().join("late");
        fs::create_dir_all(&early).unwrap();
        fs::create_dir_all(&late).unwrap();
        // A directory named like the program is not the program.
        fs::create_dir_all(early.join("tool")).unwrap();
        fs::write(late.join("tool"), "#!/bin/sh\nexit 0\n").unwrap();
        let path = format!("{}::{}", early.display(), late.display());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(resolve("tool", &path), None, "not executable yet");
            fs::set_permissions(late.join("tool"), fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(resolve("tool", &path), Some(late.join("tool")));
        }
        assert_eq!(resolve("nope", &path), None);
    }

    #[cfg(unix)]
    #[test]
    fn only_watched_programs_are_tallied() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(probe) = probe(tmp.path()) else {
            return;
        };
        // A log line for something nobody watched cannot inflate the total.
        let count = probe.tally("git\n\ngit\nnot-watched\n  git  \n");
        assert_eq!(count.total, 3);
        assert_eq!(count.by_program.get("git"), Some(&3));
        assert!(!count.by_program.contains_key("not-watched"));
        assert!(probe.describe().contains("git"));
    }
}
