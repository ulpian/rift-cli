#![no_main]
//! Differential fuzz target: every line the shell recogniser accepts, `bash -n`
//! accepts too.
//!
//! The recogniser books a saving on a line it reads as a set of file reads, and
//! that booking is only honest if the shell would run the line at all. A line
//! the grammar admits but the shell refuses to parse is a claim about output
//! that was never produced, so it is a bug in the grammar however harmless the
//! words look. The converse is not asserted: the grammar refuses far more than
//! the shell rejects, and a refusal only costs a saving.
//!
//! `bash -n` parses without executing, so the oracle runs nothing the line
//! names. Where no `bash` can be started the target skips every input rather
//! than failing, so the required smoke stays meaningful on a host that has one
//! and harmless on one that does not. Only accepted lines reach the process
//! spawn, which keeps the budget on the classification.
//!
//! The seed corpus is the `shell_read` target's, so the comparison starts from
//! the command shapes the recogniser is already known to admit.

use std::process::{Command, Stdio};
use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use rift_agent::shell_read::{recognise, MAX_COMMAND_BYTES};

/// Whether a `bash` can be started here at all, asked once.
fn bash_present() -> bool {
    static PRESENT: OnceLock<bool> = OnceLock::new();
    *PRESENT.get_or_init(|| bash_parses(":") == Some(true))
}

/// Whether `bash -n` accepts `line`, or [`None`] when no `bash` ran.
fn bash_parses(line: &str) -> Option<bool> {
    let status = Command::new("bash")
        .args(["--norc", "--noprofile", "-n", "-c", line])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    Some(status.success())
}

fuzz_target!(|data: &[u8]| {
    let data = &data[..data.len().min(MAX_COMMAND_BYTES + 64)];
    // A NUL cannot reach a shell as an argument, so it is no line to compare.
    let Ok(command) = std::str::from_utf8(data) else {
        return;
    };
    if command.contains('\0') || recognise(command).is_none() || !bash_present() {
        return;
    }
    assert_eq!(
        bash_parses(command),
        Some(true),
        "the grammar accepted a line `bash -n` rejects"
    );
});
