//! Conversion in a child process, under a deadline.
//!
//! The fourteen document parsers are linked into `rift`, and the conversion
//! itself still happens in a one-shot child process — `rift` re-spawned on
//! its hidden `hooks doc` route — spawned per conversion, that reads bytes,
//! prints one JSON object and exits. It is not a daemon, not a server and not
//! something that outlives the call that made it. Being a process is the
//! point, and the point survived the parsers moving into the binary: the
//! deadline below is a kill, and a container that decompresses past memory
//! kills the child and nothing else.
//!
//! Three properties matter more than speed here, because the caller is on a
//! hook path where the developer is waiting:
//!
//! - **The deadline is the parent's.** [`DEFAULT_BUDGET`] is 250 ms, measured
//!   from before the spawn, and it bounds the caller's whole wait: the spawn,
//!   the kill and the reap happen on a worker thread, and the caller leaves
//!   when the budget does. The child is stopped by killing it rather than by
//!   asking it to stop. A parser wedged on a hostile file cannot be reasoned
//!   with, and the read it was going to improve still has to happen.
//! - **Every failure is one failure.** A missing helper, a deadline, a crash,
//!   a truncated pipe and a stranger's program under the same name all reach
//!   the caller as a [`DocError`], so the hook path has exactly one thing to
//!   do about all of them: nothing, and let the agent read the file it asked
//!   for.
//! - **Nothing the child says is believed except its shape.** See
//!   [`crate::wire`].
//!
//! The cheap refusals — over the size cap, not a document extension — are made
//! here, before a process is spawned at all: they are two comparisons, and a
//! process that is never started cannot miss a deadline.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::{is_supported_extension, wire, ConvertCaps, DocConverter, DocError, Markdown};

/// The conversion budget on a hook path.
///
/// 250 ms is the standing decision for this path, and it is a ceiling on
/// what a developer waits rather than an estimate of what a conversion
/// costs: past it the read simply proceeds unconverted.
pub const DEFAULT_BUDGET: Duration = Duration::from_millis(250);

/// How much larger than its input a conversion's answer may be.
///
/// The input cap does not bound the output: `docx`, `xlsx`, `pptx` and `epub`
/// are zip containers, and a few kilobytes of them decompress to as much as
/// their author chose. Being a separate process is most of the answer — a
/// child that exhausts its memory dies, and its death reaches the parent as
/// one more refusal — but the parent must also refuse to *read* an unbounded
/// answer, or the child's problem becomes its own.
pub const OUTPUT_MULTIPLE: u64 = 4;

/// The smallest output ceiling, so that a deliberately tiny input cap still
/// leaves room for an answer to the question that was asked.
pub const MIN_OUTPUT_BYTES: u64 = 64 * 1024;

/// The most output a conversion under `caps` may produce before the helper is
/// treated as having lost its footing.
#[must_use]
pub fn output_ceiling(caps: &ConvertCaps) -> u64 {
    caps.max_input_bytes
        .saturating_mul(OUTPUT_MULTIPLE)
        .max(MIN_OUTPUT_BYTES)
}

/// The helper could not be started at all.
const HELPER_MISSING: &str = "helperMissing";
/// The helper ran but did not produce an envelope.
const HELPER_BROKEN: &str = "helper";
/// The helper was still running when the budget ran out.
const DEADLINE: &str = "deadline";

/// Conversion in a child process that speaks the [`crate::wire`] protocol.
///
/// The child is `program` run with `prefix_args` and then the conversion's own
/// arguments (`convert - --ext … --json`). A dedicated helper binary has an
/// empty prefix; a program that reaches its converter through a subcommand —
/// `rift` re-spawning itself on a hidden route — names that route here, and
/// the argument list the child sees after it is byte-for-byte the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildProcessConverter {
    program: PathBuf,
    prefix_args: Vec<OsString>,
    budget: Duration,
}

/// One conversion's worth of state, moved whole onto its worker thread.
struct Call {
    command: Command,
    payload: Vec<u8>,
    /// The most output the parent will read; see [`output_ceiling`].
    ceiling: u64,
    /// The instant before the spawn, which the deadline is measured from.
    started: Instant,
    budget: Duration,
    watch: Watch,
}

/// What a test can see of the reap that happens after the caller has left.
///
/// Empty outside tests. Inside them, `hold` keeps the worker at the kill
/// until the test lets it go, so the helper's being alive after the call
/// returned is a fact rather than a race, and `reaped` reports the reap, so
/// a test waits for that event instead of for a length of time.
#[derive(Default)]
struct Watch {
    #[cfg(test)]
    hold: Option<mpsc::Receiver<()>>,
    #[cfg(test)]
    reaped: Option<mpsc::Sender<()>>,
}

impl ChildProcessConverter {
    /// A converter that runs `program`, with `prefix_args` ahead of the
    /// conversion's own arguments.
    pub fn new(
        program: impl Into<PathBuf>,
        prefix_args: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        Self {
            program: program.into(),
            prefix_args: prefix_args.into_iter().map(Into::into).collect(),
            budget: DEFAULT_BUDGET,
        }
    }

    /// A converter that runs **this very executable** on `prefix_args`.
    ///
    /// The path is canonicalized first, for the reason every self-spawn in
    /// this tree does it: a hook may have entered through a symlink whose
    /// name is a git hook's, and re-running that entry path would route the
    /// child through the hook dispatcher instead of the converter. `None`
    /// only when the running executable cannot name itself at all.
    #[must_use]
    pub fn self_exec(prefix_args: impl IntoIterator<Item = impl Into<OsString>>) -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        Some(Self::new(exe, prefix_args))
    }

    /// A converter that runs exactly this program, with no prefix.
    pub fn at(helper: impl Into<PathBuf>) -> Self {
        Self::new(helper, std::iter::empty::<OsString>())
    }

    /// The same converter under a different deadline.
    #[must_use]
    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// The program this converter runs.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// The arguments the program is given ahead of the conversion's own.
    #[must_use]
    pub fn prefix_args(&self) -> &[OsString] {
        &self.prefix_args
    }

    /// The deadline this converter enforces.
    #[must_use]
    pub fn budget(&self) -> Duration {
        self.budget
    }

    /// Run the helper over `bytes` and return whatever it wrote, or the one
    /// reason it wrote nothing usable.
    ///
    /// The clock starts before the spawn, and the spawn happens on a worker
    /// thread. `spawn()` returns only once the exec has happened, which is a
    /// few milliseconds on a quiet machine and seconds under load, while the
    /// system assesses a program it has not run before, and nothing can
    /// interrupt it. A deadline started after it, or a kill and reap done
    /// here after the deadline, would add both to what the developer waits.
    /// So this thread waits on one channel for at most the budget and
    /// leaves; the worker, whenever its spawn returns, finishes the call on
    /// its own deadline, kills and reaps the child, and its answer goes
    /// unread.
    fn run(
        &self,
        bytes: &[u8],
        ext: &str,
        caps: &ConvertCaps,
        watch: Watch,
    ) -> Result<Vec<u8>, DocError> {
        let started = Instant::now();
        let call = Call {
            command: self.command(ext, caps),
            // Copied, because the worker may outlive this call.
            payload: bytes.to_vec(),
            ceiling: output_ceiling(caps),
            started,
            budget: self.budget,
            watch,
        };
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::Builder::new().spawn(move || {
            let _ = sender.send(converse(call));
        });
        if worker.is_err() {
            return Err(DocError::Engine {
                code: HELPER_MISSING,
            });
        }
        match receiver.recv_timeout(self.budget.saturating_sub(started.elapsed())) {
            Ok(outcome) => outcome,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(DocError::Engine { code: DEADLINE }),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(DocError::Engine {
                code: HELPER_BROKEN,
            }),
        }
    }

    /// The helper's command line: the prefix, then `convert - --ext …
    /// --json`, with stdin and stdout piped.
    fn command(&self, ext: &str, caps: &ConvertCaps) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.prefix_args)
            .arg("convert")
            .arg("-")
            .arg("--ext")
            .arg(ext)
            .arg("--table-rows")
            .arg(caps.table_rows.to_string())
            .arg("--max-input-bytes")
            .arg(caps.max_input_bytes.to_string())
            .arg("--pdf-min-chars-per-page")
            .arg(caps.pdf_min_chars_per_page.to_string())
            .arg("--json")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // The helper's diagnostics are for a human at a terminal. On this
            // path they would be an unread pipe that fills and blocks a parser
            // mid-document, which is the one thing the deadline cannot fix.
            .stderr(Stdio::null());
        command
    }
}

impl DocConverter for ChildProcessConverter {
    fn convert(&self, bytes: &[u8], ext: &str, caps: &ConvertCaps) -> Result<Markdown, DocError> {
        let size = bytes.len() as u64;
        if size > caps.max_input_bytes {
            return Err(DocError::TooLarge {
                bytes: size,
                cap: caps.max_input_bytes,
            });
        }
        let ext = ext.strip_prefix('.').unwrap_or(ext).to_ascii_lowercase();
        if !is_supported_extension(&ext) {
            return Err(DocError::UnsupportedExtension(ext));
        }
        let out = self.run(bytes, &ext, caps, Watch::default())?;
        wire::decode(&out, &ext, size, caps).unwrap_or(Err(DocError::Engine {
            code: HELPER_BROKEN,
        }))
    }
}

/// The whole of a call off the caller's thread: spawn, write, read, kill,
/// reap. Its answer is only read if it arrives inside the budget.
fn converse(call: Call) -> Result<Vec<u8>, DocError> {
    let Call {
        mut command,
        payload,
        ceiling,
        started,
        budget,
        watch,
    } = call;
    let Ok(mut child) = command.spawn() else {
        return Err(DocError::Engine {
            code: HELPER_MISSING,
        });
    };
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        reap(&mut child, watch);
        return Err(DocError::Engine {
            code: HELPER_BROKEN,
        });
    };

    // Both pipes get a thread of their own, and neither is joined. A child
    // writing its answer while the parent is still writing its question
    // deadlocks the pair if one thread serves both, and *waiting* for either
    // thread would hand the deadline back to the child: a killed process
    // whose own descendant still holds the pipe open would keep the worker
    // for as long as that descendant lives, and the child unreaped with it.
    // So the worker's deadline is a channel timeout too, and the two pipe
    // threads end when the pipes close, whenever that is.
    std::thread::spawn(move || {
        let _ = stdin.write_all(&payload);
        drop(stdin);
    });
    let (sender, receiver) = mpsc::channel();
    // One byte past the ceiling, so that reaching the ceiling and passing it
    // are distinguishable and the read still stops either way.
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let read = stdout.take(ceiling.saturating_add(1)).read_to_end(&mut out);
        let _ = sender.send(read.map(|_| out));
    });
    let outcome = match receiver.recv_timeout(budget.saturating_sub(started.elapsed())) {
        Ok(Ok(out)) if out.len() as u64 > ceiling => Err(DocError::Engine {
            code: HELPER_BROKEN,
        }),
        Ok(Ok(out)) => Ok(out),
        Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => Err(DocError::Engine {
            code: HELPER_BROKEN,
        }),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(DocError::Engine { code: DEADLINE }),
    };
    // Unconditionally, on every path: a helper that answered and then kept
    // running is as much of a problem as one that never answered, and the
    // exit status is not the channel this reads anyway.
    reap(&mut child, watch);
    outcome
}

/// Kill the helper and collect its exit, so that no call leaves a zombie.
fn reap(child: &mut Child, watch: Watch) {
    #[cfg(test)]
    if let Some(hold) = &watch.hold {
        let _ = hold.recv();
    }
    let _ = child.kill();
    let _ = child.wait();
    #[cfg(test)]
    if let Some(reaped) = watch.reaped {
        let _ = reaped.send(());
    }
    #[cfg(not(test))]
    let _ = watch;
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSV: &[u8] = b"name,cost\nalpha,1\n";

    /// The budget the protocol tests run under, which is deliberately not the
    /// production one. A stand-in's first exec, the costly one, is taken out
    /// of every budget by [`warm`]; what is left is a loaded machine
    /// scheduling a shell, which is a fact about the machine and not about
    /// the helper. The deadline has its own test; these have a protocol to
    /// check.
    ///
    /// Gated with `stand_in`, its only users, until the protocol tests have a
    /// Windows stand-in of their own.
    const PATIENT: Duration = Duration::from_secs(30);

    /// The one argument on which a stand-in exits at once, before its body.
    const WARM: &str = "--warm";

    /// A stand-in helper: a script that ignores its arguments and behaves the
    /// way the test needs. Real conversions through the real binary are tested
    /// beside that binary, where cargo guarantees it has been built; what is
    /// under test here is the parent's half — the deadline, the kill, and the
    /// refusal to believe anything a child says.
    #[cfg(unix)]
    fn stand_in(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, script(body)).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        warm(&path);
        path
    }

    /// The Windows twin: the same POSIX body, run through git for Windows'
    /// bundled `sh` from a `.cmd` wrapper, because the parent spawns the
    /// helper path as an executable and a shell script is not one here. The
    /// wrapper forwards stdin, stdout and every argument, so the parent's
    /// half (deadline, kill, refusal to believe the child) is exercised
    /// exactly as on Unix.
    #[cfg(windows)]
    fn stand_in(dir: &Path, name: &str, body: &str) -> PathBuf {
        let posix = dir.join(format!("{name}.sh"));
        std::fs::write(&posix, script(body)).unwrap();
        let wrapper = dir.join(format!("{name}.cmd"));
        std::fs::write(
            &wrapper,
            format!("@\"{}\" \"%~dp0{name}.sh\" %*\r\n", bundled_sh().display()),
        )
        .unwrap();
        warm(&wrapper);
        wrapper
    }

    /// The stand-in's text: `body`, behind a first line that answers [`WARM`].
    fn script(body: &str) -> String {
        format!("#!/bin/sh\n[ \"$1\" = {WARM} ] && exit 0\n{body}\n")
    }

    /// Run a freshly written stand-in once, outside any budget.
    ///
    /// macOS assesses a new executable file on its first exec. The first exec
    /// of a script written a moment ago has been measured here at one and a
    /// half to three seconds, against three hundredths of a second for the
    /// second one, and the assessments of files exec'd at once run one after
    /// another, machine-wide. Eight stand-ins written by eight concurrent
    /// tests, on a machine building other test binaries, wait past any budget
    /// before the first line of any of them runs, so a protocol test would
    /// be measuring the queue. The warm run takes the queue out of every
    /// budget below, the deadline test's included: that test is about
    /// killing a helper that is running, not one that has not started.
    fn warm(helper: &Path) {
        let status = Command::new(helper)
            .arg(WARM)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "{} does not start", helper.display());
    }

    /// Git for Windows' own `sh.exe`, found from `git --exec-path`
    /// (`<root>/mingw64/libexec/git-core`): `<root>/usr/bin/sh.exe`, or the
    /// `<root>/bin/sh.exe` launcher. The same lookup lives beside the hook
    /// shim test in the `rift` binary's suite; the two crates share no test
    /// edge.
    #[cfg(windows)]
    fn bundled_sh() -> PathBuf {
        let out = std::process::Command::new("git")
            .arg("--exec-path")
            .output()
            .expect("git is on PATH");
        let exec_path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        let root = exec_path
            .ancestors()
            .nth(3)
            .expect("exec-path is three levels under the install root")
            .to_path_buf();
        [root.join("usr").join("bin"), root.join("bin")]
            .into_iter()
            .map(|dir| dir.join("sh.exe"))
            .find(|sh| sh.is_file())
            .expect("git for Windows ships sh.exe")
    }

    /// A named, deterministic directory under the build directory: no clock,
    /// no randomness, and emptied first so a rerun cannot read a stale run.
    fn scratch_dir(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/rift-recall-doc-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn scratch(name: &str) -> PathBuf {
        scratch_dir(name)
    }

    /// The two comparisons that are cheaper than a process, made before one is
    /// started: a converter pointed at a helper that does not exist still
    /// refuses an oversized or unconvertible input the same way the in-process
    /// engine does.
    #[test]
    fn the_cheap_refusals_are_made_without_spawning_anything() {
        let converter = ChildProcessConverter::at("/nonexistent/rift-doc");
        let caps = ConvertCaps {
            max_input_bytes: 4,
            ..ConvertCaps::default()
        };
        assert_eq!(
            converter.convert(CSV, "csv", &caps),
            Err(DocError::TooLarge {
                bytes: CSV.len() as u64,
                cap: 4
            })
        );
        assert_eq!(
            converter.convert(CSV, ".MD", &ConvertCaps::default()),
            Err(DocError::UnsupportedExtension("md".to_owned()))
        );
    }

    /// No helper is a supported state, not a crash: the caller leaves the
    /// read alone and the agent sees the file it asked for.
    #[test]
    fn an_absent_helper_is_a_refusal_the_caller_can_fall_back_on() {
        let converter = ChildProcessConverter::at("/nonexistent/rift-doc");
        assert_eq!(
            converter.convert(CSV, "csv", &ConvertCaps::default()),
            Err(DocError::Engine {
                code: HELPER_MISSING
            })
        );
    }

    #[test]
    fn a_well_formed_answer_is_returned_whole() {
        let dir = scratch("well-formed");
        let helper = stand_in(
            &dir,
            "rift-doc",
            r##"cat > /dev/null; printf '%s' '{"status":"ok","text":"# Costs\n","pages":2,"thin_pages":[2],"sheets":[]}'"##,
        );
        let markdown = ChildProcessConverter::at(helper)
            .with_budget(PATIENT)
            .convert(CSV, "csv", &ConvertCaps::default())
            .unwrap();
        assert_eq!(markdown.text, "# Costs\n");
        assert_eq!(markdown.pages, Some(2));
        assert_eq!(markdown.thin_pages, vec![2]);
    }

    /// The deadline belongs to the parent and is enforced by killing, because
    /// a parser wedged on a hostile file cannot be asked to stop.
    #[test]
    fn a_helper_that_overruns_the_budget_is_killed_and_the_read_is_left_alone() {
        let dir = scratch("slow");
        // Sleeps far past any plausible budget. The sleeper records its own
        // pid, and it is the helper's process itself: `exec` replaces the
        // shell rather than forking below it, so the kill lands on the
        // process that is sleeping.
        let helper = stand_in(
            &dir,
            "rift-doc",
            r#"cat > /dev/null; exec sh -c 'echo $$ > "$0"; exec sleep 30' "${0%/*}/sleeper.pid""#,
        );
        let (reaped, on_reaped) = mpsc::channel();
        let watch = Watch {
            hold: None,
            reaped: Some(reaped),
        };
        let result = ChildProcessConverter::at(helper)
            .with_budget(Duration::from_millis(120))
            .run(CSV, "csv", &ConvertCaps::default(), watch);
        assert_eq!(result, Err(DocError::Engine { code: DEADLINE }));
        // The kill happens off the caller's thread, so the proof waits for the
        // reap to be reported rather than for any length of time.
        on_reaped.recv().unwrap();
        assert_nothing_it_spawned_survives(&dir.join("sleeper.pid"));
    }

    /// The budget is the caller's whole wait: a helper that has not even
    /// begun its work when the deadline falls is abandoned there, and the
    /// kill and the reap happen afterwards, off the caller's thread.
    ///
    /// The stand-in's start is slow — it hands its pid through a fifo and
    /// sleeps before it would read a byte — and the reaper is held at the
    /// kill until the test lets it go, so "the call returned without waiting
    /// for the helper" is judged by the helper still running at the return,
    /// and "never a zombie" by its pid vanishing once the reap is reported.
    #[cfg(unix)]
    #[test]
    fn a_helper_slow_to_start_is_abandoned_at_the_deadline_and_reaped_after_the_return() {
        let dir = scratch("slow-start");
        let fifo = dir.join("helper.pid");
        assert!(Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        let helper = stand_in(
            &dir,
            "rift-doc",
            r#"echo $$ > "${0%/*}/helper.pid"; exec sleep 30"#,
        );
        let (release, hold) = mpsc::channel();
        let (reaped, on_reaped) = mpsc::channel();
        let watch = Watch {
            hold: Some(hold),
            reaped: Some(reaped),
        };
        let result = ChildProcessConverter::at(helper)
            .with_budget(Duration::from_millis(120))
            .run(CSV, "csv", &ConvertCaps::default(), watch);
        assert_eq!(result, Err(DocError::Engine { code: DEADLINE }));
        let events = first_of_pid_or_reap(fifo, on_reaped);
        let Some(pid) = events.recv().unwrap() else {
            panic!("the helper was reaped before the call returned");
        };
        assert!(
            is_alive(&pid),
            "the helper {pid} was not running at the return"
        );
        release.send(()).unwrap();
        assert_eq!(events.recv().unwrap(), None, "the reap was never reported");
        assert!(!is_alive(&pid), "the helper {pid} outlived its reap");
    }

    /// One channel carrying whichever comes first: the pid the stand-in
    /// writes into `fifo` (`Some`), or the reaper's report (`None`). A helper
    /// killed before it wrote its pid never opens the fifo, so waiting on the
    /// fifo alone would wait forever; the reap ends that wait instead.
    #[cfg(unix)]
    fn first_of_pid_or_reap(
        fifo: PathBuf,
        on_reaped: mpsc::Receiver<()>,
    ) -> mpsc::Receiver<Option<String>> {
        let (sender, events) = mpsc::channel();
        let pid_sender = sender.clone();
        std::thread::spawn(move || {
            let pid = std::fs::read_to_string(fifo).unwrap();
            let _ = pid_sender.send(Some(pid.trim().to_owned()));
        });
        std::thread::spawn(move || {
            if on_reaped.recv().is_ok() {
                let _ = sender.send(None);
            }
        });
        events
    }

    /// The process that was sleeping when the deadline fell is gone.
    ///
    /// The kill reaches the helper's own process and nothing below it, so a
    /// stand-in that sleeps in a child of its shell leaves that child running
    /// for as long as it sleeps, holding the pipes it inherited. No pid file
    /// means the helper was killed before it started sleeping, and then there
    /// is nothing that could have outlived it.
    #[cfg(unix)]
    fn assert_nothing_it_spawned_survives(pid_file: &Path) {
        let Ok(pid) = std::fs::read_to_string(pid_file) else {
            return;
        };
        assert!(
            !is_alive(pid.trim()),
            "the sleeper {} outlived the kill",
            pid.trim()
        );
    }

    /// Whether `pid` names a live process, by `kill -0`.
    #[cfg(unix)]
    fn is_alive(pid: &str) -> bool {
        Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    /// Windows has no `kill -0`, and the stand-in there is a wrapper whose kill
    /// never reached the shell below it anyway.
    #[cfg(windows)]
    fn assert_nothing_it_spawned_survives(_pid_file: &Path) {}

    /// Everything a child can say that is not an envelope is the same thing:
    /// one refusal, on which the caller leaves the read alone.
    #[test]
    fn nothing_a_child_says_off_the_format_is_believed() {
        let dir = scratch("hostile");
        for body in [
            "cat > /dev/null; printf 'not json'",
            "cat > /dev/null; printf '{\"status\":\"ok\"}'",
            "cat > /dev/null; printf '{\"status\":\"refused\",\"code\":\"nope\"}'",
            "cat > /dev/null; exit 3",
            "exit 0",
        ] {
            let helper = stand_in(&dir, "rift-doc", body);
            assert_eq!(
                ChildProcessConverter::at(&helper)
                    .with_budget(PATIENT)
                    .convert(CSV, "csv", &ConvertCaps::default()),
                Err(DocError::Engine {
                    code: HELPER_BROKEN
                }),
                "{body}"
            );
        }
    }

    /// The envelope is the channel and the exit status is not: a helper that
    /// answered and then exited badly still answered.
    #[test]
    fn an_answer_counts_whatever_the_exit_status_says() {
        let dir = scratch("exit-status");
        let helper = stand_in(
            &dir,
            "rift-doc",
            r#"cat > /dev/null; printf '%s' '{"status":"ok","text":"x"}'; exit 9"#,
        );
        let markdown = ChildProcessConverter::at(helper)
            .with_budget(PATIENT)
            .convert(CSV, "csv", &ConvertCaps::default())
            .unwrap();
        assert_eq!(markdown.text, "x");
    }

    /// The input cap does not bound the output, so the parent bounds what it
    /// will read. A zip-shaped document decompresses to whatever its author
    /// chose, and a helper handing back gigabytes must cost the parent one
    /// refusal rather than its memory.
    #[test]
    fn an_answer_past_the_output_ceiling_is_refused_rather_than_read() {
        let dir = scratch("flood");
        let caps = ConvertCaps {
            max_input_bytes: 32,
            ..ConvertCaps::default()
        };
        assert_eq!(output_ceiling(&caps), MIN_OUTPUT_BYTES);
        // Two ceilings' worth of a well-formed-looking answer.
        let helper = stand_in(
            &dir,
            "rift-doc",
            "cat > /dev/null; printf '{\"status\":\"ok\",\"text\":\"';              head -c 131072 /dev/zero | tr '\\000' 'a'; printf '\"}'",
        );
        assert_eq!(
            ChildProcessConverter::at(&helper)
                .with_budget(PATIENT)
                .convert(CSV, "csv", &caps),
            Err(DocError::Engine {
                code: HELPER_BROKEN
            })
        );
        // The same helper under a ceiling that fits it is believed, so the
        // refusal above is the cap acting and not the shape of the output.
        let roomy = ConvertCaps {
            max_input_bytes: 1024 * 1024,
            ..ConvertCaps::default()
        };
        let markdown = ChildProcessConverter::at(&helper)
            .with_budget(PATIENT)
            .convert(CSV, "csv", &roomy)
            .unwrap();
        assert_eq!(markdown.text.len(), 131_072);
    }

    /// A refusal the helper made travels back as a refusal, not as a crash,
    /// so the caller can tell "this document is encrypted" from "the helper
    /// fell over" if it ever wants to.
    #[test]
    fn a_refusal_from_the_helper_keeps_its_classification() {
        let dir = scratch("refusal");
        let helper = stand_in(
            &dir,
            "rift-doc",
            r#"cat > /dev/null; printf '%s' '{"status":"refused","code":"engine","engine_code":"encrypted"}'"#,
        );
        assert_eq!(
            ChildProcessConverter::at(helper)
                .with_budget(PATIENT)
                .convert(CSV, "csv", &ConvertCaps::default()),
            Err(DocError::Engine { code: "encrypted" })
        );
    }

    /// The bytes reach the child, and the caps reach it as arguments: a helper
    /// that echoes what it was given proves both without a parser.
    #[test]
    fn the_document_and_the_caps_both_reach_the_helper() {
        let dir = scratch("argv");
        let helper = stand_in(
            &dir,
            "rift-doc",
            r#"read -r line; printf '{"status":"ok","text":"%s %s"}' "$line" "$*""#,
        );
        let caps = ConvertCaps {
            table_rows: 7,
            max_input_bytes: 4096,
            pdf_min_chars_per_page: 11,
        };
        let markdown = ChildProcessConverter::at(helper)
            .with_budget(PATIENT)
            .convert(CSV, ".CSV", &caps)
            .unwrap();
        assert!(markdown.text.starts_with("name,cost "), "{markdown:?}");
        assert!(
            markdown
                .text
                .contains("convert - --ext csv --table-rows 7 ")
                && markdown.text.contains("--max-input-bytes 4096 ")
                && markdown.text.contains("--pdf-min-chars-per-page 11 --json"),
            "{markdown:?}"
        );
    }

    /// A program reached through a subcommand sees that subcommand first and
    /// the conversion's own arguments, unchanged, after it: the prefix is
    /// prepended and nothing else about the command line moves.
    #[test]
    fn prefix_args_come_before_the_conversions_own_arguments() {
        let dir = scratch("prefix");
        let helper = stand_in(
            &dir,
            "rift-doc",
            r#"cat > /dev/null; printf '{"status":"ok","text":"%s"}' "$*""#,
        );
        let markdown = ChildProcessConverter::new(helper, ["hooks", "doc"])
            .with_budget(PATIENT)
            .convert(CSV, "csv", &ConvertCaps::default())
            .unwrap();
        assert!(
            markdown
                .text
                .starts_with("hooks doc convert - --ext csv --table-rows "),
            "{markdown:?}"
        );
    }

    /// A self-spawn names the executable that is running, by its real path —
    /// and only names it; nothing is spawned to find out.
    #[test]
    fn self_exec_names_this_very_executable() {
        let converter = ChildProcessConverter::self_exec(["hooks", "doc"])
            .expect("a test binary can name itself");
        let exe = std::env::current_exe().unwrap();
        assert_eq!(
            converter.program(),
            std::fs::canonicalize(&exe).unwrap_or(exe)
        );
        assert!(converter.program().is_file());
        assert_eq!(
            converter.prefix_args(),
            &[OsString::from("hooks"), OsString::from("doc")]
        );
        assert_eq!(converter.budget(), DEFAULT_BUDGET);
    }

    #[test]
    fn the_budget_and_the_program_are_what_they_were_set_to() {
        let converter = ChildProcessConverter::at("/opt/rift/rift-doc");
        assert_eq!(converter.budget(), DEFAULT_BUDGET);
        assert_eq!(converter.program(), Path::new("/opt/rift/rift-doc"));
        assert!(converter.prefix_args().is_empty());
        let converter = converter.with_budget(Duration::from_millis(1));
        assert_eq!(converter.budget(), Duration::from_millis(1));
    }
}
