//! `vs-reference` — the benchmark harness.
//!
//! Every "better in Rust" claim this project makes has to become a *number a
//! harness produced and anybody can re-run*. This crate is that harness:
//!
//! * `fixture` generates the benchmark repositories deterministically — no repo is
//!   ever cloned from the network, and the same `FixtureSpec` yields the same
//!   commit oids on every machine;
//! * [`hermetic`] pins the process environment so two legs are measured under
//!   identical conditions;
//! * the *legs* (rift, and the pinned capture backend) are pluggable binaries. The
//!   backend leg may be **absent**: the harness then runs rift-only and records the
//!   comparison as skipped rather than fabricating one. Provisioning that binary is
//!   an owner step and is deliberately not automated here — the harness
//!   downloads nothing.
//!
//! See `README.md` in this directory for the measurement protocol.

#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod attest;
pub mod audit;
pub mod displace;
pub mod driver;
pub mod facts;
pub mod fixture;
pub mod floors;
pub mod freshness;
pub mod hermetic;
pub mod hooklat;
pub mod leg;
pub mod legacy;
pub mod measure;
pub mod provenance;
pub mod report;
pub mod report_facts;
pub mod rng;
pub mod rss;
pub mod scenario;
pub mod spawns;

/// Harness error. Carries paths, argv, and status codes — never file contents.
#[derive(Debug, thiserror::Error)]
pub enum BenchError {
    /// A filesystem operation failed.
    #[error("{context}: {source}")]
    Io {
        /// What the harness was doing.
        context: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// A child process could not be launched.
    #[error("cannot launch `{program}`: {source}")]
    Spawn {
        /// The program the harness tried to run.
        program: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// A child process exited non-zero.
    #[error("`{program} {args}` exited with {status}: {stderr}")]
    Subprocess {
        /// The program that failed.
        program: String,
        /// Its arguments, space-joined.
        args: String,
        /// Its exit status, rendered.
        status: String,
        /// Trailing stderr (trimmed), for diagnosis.
        stderr: String,
    },
    /// A specification was rejected before any work started.
    #[error("invalid spec: {0}")]
    Spec(String),
    /// JSON (de)serialization of a harness artifact failed.
    #[error("{context}: {source}")]
    Json {
        /// What the harness was (de)serializing.
        context: String,
        /// The underlying error.
        #[source]
        source: serde_json::Error,
    },
}

impl BenchError {
    /// Wrap an I/O error with the operation that produced it.
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    /// Wrap a JSON error with the artifact that produced it.
    pub fn json(context: impl Into<String>, source: serde_json::Error) -> Self {
        Self::Json {
            context: context.into(),
            source,
        }
    }
}

/// The harness result alias.
pub type Result<T> = std::result::Result<T, BenchError>;
