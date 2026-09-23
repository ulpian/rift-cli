//! Where and when a run happened.
//!
//! Provenance only: nothing in the harness derives behaviour from either
//! figure, and both travel with the numbers so a reader can tell which machine
//! and which sitting produced them.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Where the run happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    /// `std::env::consts::OS`.
    pub os: String,
    /// `std::env::consts::ARCH`.
    pub arch: String,
    /// Logical CPUs, when the platform reports them.
    pub logical_cpus: Option<usize>,
    /// How peak RSS was obtained (or `unavailable`).
    pub rss_probe: String,
}

impl Host {
    /// Describe the current host.
    #[must_use]
    pub fn detect(rss_probe: String) -> Self {
        Self {
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            logical_cpus: std::thread::available_parallelism().ok().map(Into::into),
            rss_probe,
        }
    }
}

/// Wall-clock seconds, for report provenance only.
#[must_use]
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}
