//! A testable wrapper around the `entire` CLI.
//!
//! `entire` is a Go binary, so Rift shells out to it rather than linking. The
//! [`EntireRunner`] trait is the seam: [`RealRunner`] drives the actual process,
//! [`MockRunner`] scripts it for tests, and [`locate`] resolves which binary to
//! invoke.

pub mod error;
pub mod locate;
pub mod runner;

#[cfg(any(test, feature = "mock"))]
pub mod mock;

pub use error::EntireError;
pub use locate::locate;
pub use runner::{EntireRunner, Output, RealRunner};

#[cfg(any(test, feature = "mock"))]
pub use mock::MockRunner;
