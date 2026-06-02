//! A scripted [`EntireRunner`] for tests.
//!
//! Available to this crate's own tests always, and to downstream crates when
//! the `mock` feature is enabled.

use std::cell::RefCell;
use std::ffi::OsString;

use crate::error::EntireError;
use crate::runner::{EntireRunner, Output};

/// Records the invocations made against it and replays canned responses.
#[derive(Default)]
pub struct MockRunner {
    version: String,
    captured: Output,
    /// Every `run_*` arg list, in call order, for assertions.
    pub calls: RefCell<Vec<Vec<OsString>>>,
}

impl MockRunner {
    /// A mock reporting the given version string and an empty successful capture.
    #[must_use]
    pub fn with_version(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            captured: Output {
                status: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
            calls: RefCell::new(Vec::new()),
        }
    }

    /// Set the stdout returned by [`EntireRunner::run_captured`].
    #[must_use]
    pub fn with_captured_stdout(mut self, stdout: impl Into<Vec<u8>>) -> Self {
        self.captured.stdout = stdout.into();
        self
    }

    fn record(&self, args: &[OsString]) {
        self.calls.borrow_mut().push(args.to_vec());
    }
}

impl EntireRunner for MockRunner {
    fn run_captured(&self, args: &[OsString]) -> Result<Output, EntireError> {
        self.record(args);
        Ok(self.captured.clone())
    }

    fn run_inherited(&self, args: &[OsString]) -> Result<i32, EntireError> {
        self.record(args);
        Ok(self.captured.status)
    }

    fn version(&self) -> Result<String, EntireError> {
        Ok(self.version.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_calls_and_replays_output() {
        let mock =
            MockRunner::with_version("Entire CLI 0.6.2").with_captured_stdout(b"hi".to_vec());
        assert!(mock.is_installed());

        let out = mock
            .run_captured(&[OsString::from("status"), OsString::from("--json")])
            .unwrap();
        assert_eq!(out.stdout, b"hi");
        assert_eq!(mock.calls.borrow().len(), 1);
        assert_eq!(mock.calls.borrow()[0][0], OsString::from("status"));
    }
}
