//! Running a program and reading what it wrote.
//!
//! Every subprocess a dogfood run starts goes through one of these: they are
//! the only place the harness spells `Command`, so the `PATH` a client sees and
//! the refusal a non-zero exit raises are decided once.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Run a program to completion with `PATH` set, capturing both streams.
pub(super) fn capture_with_path(
    program: &Path,
    args: &[String],
    cwd: &Path,
    path: &str,
) -> Result<(Option<i32>, String, String), String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("PATH", path)
        .output()
        .map_err(|e| format!("dogfood: run {}: {e}", program.display()))?;
    Ok((
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

/// Run a program to completion, capturing both streams.
pub(super) fn capture(
    program: &Path,
    args: &[String],
    cwd: &Path,
) -> Result<(Option<i32>, String, String), String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("dogfood: run {}: {e}", program.display()))?;
    Ok((
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

/// Run a program and refuse if it did not exit 0.
pub(super) fn must(program: &Path, args: &[String], cwd: &Path) -> Result<String, String> {
    let (code, stdout, stderr) = capture(program, args, cwd)?;
    if code == Some(0) {
        return Ok(stdout);
    }
    Err(format!(
        "dogfood: {} {} exited {:?}\n{stderr}",
        program.display(),
        args.join(" "),
        code
    ))
}

/// Where a client's executable is, preferring a version manager's copy when
/// the machine has two — the archived measurements found a second, much older
/// build first on `PATH` with no hook surface at all.
pub(super) fn resolve_program(name: &str) -> Result<PathBuf, String> {
    let out = Command::new("/usr/bin/which")
        .arg("-a")
        .arg(name)
        .output()
        .map_err(|e| format!("dogfood: which {name}: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut paths: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    paths.dedup();
    let preferred = paths
        .iter()
        .find(|p| p.contains("/.nvm/"))
        .or_else(|| paths.first());
    preferred
        .map(|p| PathBuf::from(*p))
        .ok_or_else(|| format!("dogfood: `{name}` is not on PATH"))
}

/// The developer's own release binary, built if it is not there yet. It is
/// the whole of what a dogfood run needs: document conversion is the same
/// binary re-spawned, not a sibling to build beside it.
pub(super) fn release_binary(root: &Path) -> Result<PathBuf, String> {
    let rift = root.join("target/release/rift");
    if !rift.exists() {
        println!("dogfood: building target/release/rift …");
        must(
            Path::new("cargo"),
            &[
                "build".to_owned(),
                "--release".to_owned(),
                "-p".to_owned(),
                "rift".to_owned(),
            ],
            root,
        )?;
    }
    if !rift.exists() {
        return Err("dogfood: the release binary is still missing after a build".to_owned());
    }
    Ok(rift)
}
