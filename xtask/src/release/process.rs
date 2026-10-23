//! Bounded subprocess capture without pipe deadlock or unbounded output buffers.

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use super::Result;

pub(super) fn capture(command: &mut Command, seconds: u64) -> Result<Output> {
    let mut stdout = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut stderr = tempfile::tempfile().map_err(|e| e.to_string())?;
    command
        .stdout(stdout.try_clone().map_err(|e| e.to_string())?)
        .stderr(stderr.try_clone().map_err(|e| e.to_string())?)
        .stdin(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|e| format!("launch {}: {e}", command.get_program().display()))?;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let outcome = loop {
        match stdout
            .metadata()
            .and_then(|out| stderr.metadata().map(|err| out.len() + err.len()))
        {
            Ok(bytes) if bytes <= 4_000_000 => (),
            Ok(_) => break Err("unexpected subprocess output volume".to_owned()),
            Err(error) => break Err(error.to_string()),
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => break Err("subprocess deadline exceeded".to_owned()),
            Err(error) => break Err(error.to_string()),
        }
    };
    if outcome.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    use std::io::{Read, Seek};
    let load = |file: &mut std::fs::File| -> Result<Vec<u8>> {
        super::ensure(
            file.metadata().map_err(|e| e.to_string())?.len() <= 4_000_000,
            "unexpected subprocess output volume",
        )?;
        file.rewind().map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        Ok(bytes)
    };
    Ok(Output {
        status: outcome?,
        stdout: load(&mut stdout)?,
        stderr: load(&mut stderr)?,
    })
}

pub(super) fn must(command: &mut Command, seconds: u64) -> Result<String> {
    let output = capture(command, seconds)?;
    if !output.status.success() {
        return Err(format!(
            "{} failed: {}{}",
            command.get_program().display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

pub(super) fn git(args: &[&str]) -> Result<String> {
    must(
        Command::new("git")
            .args(args)
            .current_dir(crate::workspace_root()),
        20,
    )
    .map(|s| s.trim().to_owned())
}
