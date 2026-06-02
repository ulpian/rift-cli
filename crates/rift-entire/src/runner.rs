//! The [`EntireRunner`] seam: every call into `entire` goes through this trait
//! so the binary and the TUI never touch [`std::process::Command`] directly,
//! and tests can substitute a mock.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::error::EntireError;

/// Captured result of a non-interactive `entire` invocation.
#[derive(Debug, Clone, Default)]
pub struct Output {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Abstraction over invoking the `entire` binary.
pub trait EntireRunner {
    /// Run `entire <args>` capturing stdout/stderr. Use for `--json`/data commands.
    ///
    /// # Errors
    /// Returns [`EntireError::Spawn`] if the process cannot start.
    fn run_captured(&self, args: &[OsString]) -> Result<Output, EntireError>;

    /// Run `entire <args>` inheriting the terminal, returning its exit code.
    /// Use for interactive or pass-through commands.
    ///
    /// # Errors
    /// Returns [`EntireError::Spawn`] if the process cannot start.
    fn run_inherited(&self, args: &[OsString]) -> Result<i32, EntireError>;

    /// Return the `entire version` string (trimmed).
    ///
    /// # Errors
    /// Returns an error if the process fails or exits non-zero.
    fn version(&self) -> Result<String, EntireError>;

    /// Whether `entire` is invocable (locatable and `version` exits zero).
    fn is_installed(&self) -> bool {
        self.version().is_ok()
    }
}

/// [`EntireRunner`] backed by the real `entire` binary.
pub struct RealRunner {
    bin: PathBuf,
}

impl RealRunner {
    #[must_use]
    pub fn new(bin: PathBuf) -> Self {
        Self { bin }
    }

    /// The resolved binary path.
    #[must_use]
    pub fn bin(&self) -> &PathBuf {
        &self.bin
    }

    fn command(&self, args: &[OsString]) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args);
        cmd
    }
}

fn display_args(args: &[OsString]) -> String {
    args.iter()
        .map(|a| a.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

impl EntireRunner for RealRunner {
    fn run_captured(&self, args: &[OsString]) -> Result<Output, EntireError> {
        let out = self
            .command(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|source| EntireError::Spawn {
                path: self.bin.clone(),
                source,
            })?;
        Ok(Output {
            status: out.status.code().unwrap_or(-1),
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }

    fn run_inherited(&self, args: &[OsString]) -> Result<i32, EntireError> {
        let status = self
            .command(args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|source| EntireError::Spawn {
                path: self.bin.clone(),
                source,
            })?;
        Ok(status.code().unwrap_or(-1))
    }

    fn version(&self) -> Result<String, EntireError> {
        let args = [OsStr::new("version").to_owned()];
        let out = self.run_captured(&args)?;
        if out.status != 0 {
            return Err(EntireError::NonZeroExit {
                args: display_args(&args),
                code: out.status,
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }
}
