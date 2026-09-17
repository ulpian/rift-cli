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
//!   from the spawn, and it is enforced by killing the child rather than by
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
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

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
    fn run(&self, bytes: &[u8], ext: &str, caps: &ConvertCaps) -> Result<Vec<u8>, DocError> {
        let mut child = Command::new(&self.program)
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
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| DocError::Engine {
                code: HELPER_MISSING,
            })?;

        let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(DocError::Engine {
                code: HELPER_BROKEN,
            });
        };

        // Both pipes get a thread of their own, and neither is joined. A child
        // writing its answer while the parent is still writing its question
        // deadlocks the pair if one thread serves both, and *waiting* for
        // either thread would hand the deadline back to the child: a killed
        // process whose own descendant still holds the pipe open would keep the
        // developer waiting for as long as that descendant lives. So the
        // deadline is a channel timeout on the main thread, and the two helper
        // threads end when the pipes close, whenever that is. The document is
        // copied for the same reason — a borrow could not outlive this call.
        let payload = bytes.to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&payload);
            drop(stdin);
        });
        let (sender, receiver) = mpsc::channel();
        // One byte past the ceiling, so that reaching the ceiling and passing
        // it are distinguishable and the read still stops either way.
        let ceiling = output_ceiling(caps);
        std::thread::spawn(move || {
            let mut out = Vec::new();
            let read = stdout.take(ceiling.saturating_add(1)).read_to_end(&mut out);
            let _ = sender.send(read.map(|_| out));
        });
        let outcome = match receiver.recv_timeout(self.budget) {
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
        let _ = child.kill();
        let _ = child.wait();
        outcome
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
        let out = self.run(bytes, &ext, caps)?;
        wire::decode(&out, &ext, size, caps).unwrap_or(Err(DocError::Engine {
            code: HELPER_BROKEN,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSV: &[u8] = b"name,cost\nalpha,1\n";

    /// The budget the protocol tests run under, which is deliberately not the
    /// production one. macOS puts a freshly written executable through the
    /// system policy service on its first exec, and that alone has been
    /// measured here at over half a second — a fact about a stand-in script
    /// nobody has run before, not about the helper, which is executed on every
    /// build. The deadline has its own test; these have a protocol to check.
    ///
    /// Gated with `stand_in`, its only users, until the protocol tests have a
    /// Windows stand-in of their own.
    const PATIENT: Duration = Duration::from_secs(30);

    /// A stand-in helper: a script that ignores its arguments and behaves the
    /// way the test needs. Real conversions through the real binary are tested
    /// beside that binary, where cargo guarantees it has been built; what is
    /// under test here is the parent's half — the deadline, the kill, and the
    /// refusal to believe anything a child says.
    #[cfg(unix)]
    fn stand_in(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
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
        let script = dir.join(format!("{name}.sh"));
        std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
        let wrapper = dir.join(format!("{name}.cmd"));
        std::fs::write(
            &wrapper,
            format!("@\"{}\" \"%~dp0{name}.sh\" %*\r\n", bundled_sh().display()),
        )
        .unwrap();
        wrapper
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
        // Sleeps far past any plausible budget, then would have answered.
        let helper = stand_in(
            &dir,
            "rift-doc",
            r#"cat > /dev/null; sleep 30; printf '%s' '{"status":"ok","text":"late"}'"#,
        );
        let started = std::time::Instant::now();
        let result = ChildProcessConverter::at(helper)
            .with_budget(Duration::from_millis(120))
            .convert(CSV, "csv", &ConvertCaps::default());
        assert_eq!(result, Err(DocError::Engine { code: DEADLINE }));
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the call waited for the child instead of killing it: {:?}",
            started.elapsed()
        );
    }

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
