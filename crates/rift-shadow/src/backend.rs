//! The strangler-fig seam for the Rust rewrite path (P7 S2).
//!
//! All of `rift-shadow`'s write-side capability is expressed as one trait,
//! [`ShadowBackend`], so callers (condense/executor) are **backend-agnostic**.
//! Both the wrapper ([`EntireBackend`]) and the native rewrite ([`NativeBackend`])
//! implement it identically; [`RoutedBackend`] routes **each capability**
//! individually (`Wrapper | DualRun | Native`, default `Wrapper`) so the rewrite
//! proceeds subsystem-by-subsystem.
//!
//! Under `DualRun` both backends execute, the **wrapper's** (canonical) result is
//! returned, and any divergence is **recorded but not committed** — so a native bug
//! during validation can never corrupt the ledger. A global kill-switch forces
//! every capability back to the wrapper. Promotion `DualRun → Native` is gated on
//! fixture parity (P7 S1) plus zero field divergence over a soak (operational).
//!
//! In this codebase hashing/attribution/cost/redaction are *already* native (they
//! never had an Entire equivalent); only the **git write** (checkpoint archival)
//! is wrapped. So the routable capabilities here are the checkpoint root (BYTE) and
//! the archive (git write) — the rest is shared native code in both backends.

use std::cell::RefCell;
use std::collections::BTreeMap;

use rift_core::{CanonicalEvent, Result, RiftError, Sha256Id};
use rift_shadow_entire::{CaptureIntent, EntireRunner, SubstrateError};
use rift_store::{
    record_verification_divergence, record_verification_run, CanonicalStore,
    VerificationDivergenceRow, VerificationRunRow,
};

use crate::sha256_hex;

fn map_archive_err(e: SubstrateError) -> RiftError {
    RiftError::storage(format!("capture backend archive failed: {e}"))
}

/// One write-side capability that can be routed independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    /// The checkpoint Merkle root (BYTE-critical; validated first).
    CheckpointRoot,
    /// Checkpoint archival (the git write orchestration).
    Archive,
}

impl Capability {
    /// The capability's token in `.rift/rewrite.toml` and divergence logs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CheckpointRoot => "hashing",
            Self::Archive => "git_write",
        }
    }
}

/// The route for a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Use the trusted wrapper (the safe default + rollback target).
    Wrapper,
    /// Run both, return the wrapper's result, record divergences (validating).
    DualRun,
    /// Use the promoted native implementation.
    Native,
}

impl Route {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().trim_matches('"') {
            "wrapper" => Some(Self::Wrapper),
            "dual_run" => Some(Self::DualRun),
            "native" => Some(Self::Native),
            _ => None,
        }
    }
}

/// The per-capability rollout dial (machine-local `.rift/rewrite.toml`). Default
/// route is `Wrapper`; a global `kill_switch` forces every capability to `Wrapper`.
#[derive(Debug, Clone, Default)]
pub struct RouteTable {
    routes: BTreeMap<Capability, Route>,
    kill_switch: bool,
}

impl RouteTable {
    /// An all-wrapper table (behaviorally identical to today's wrapped path).
    #[must_use]
    pub fn all_wrapper() -> Self {
        Self::default()
    }

    /// Set a capability's route.
    #[must_use]
    pub fn with(mut self, cap: Capability, route: Route) -> Self {
        self.routes.insert(cap, route);
        self
    }

    /// Trip the global kill-switch (every capability reverts to the wrapper).
    pub fn trip_kill_switch(&mut self) {
        self.kill_switch = true;
    }

    /// The effective route for a capability (kill-switch forces `Wrapper`).
    #[must_use]
    pub fn route(&self, cap: Capability) -> Route {
        if self.kill_switch {
            return Route::Wrapper;
        }
        self.routes.get(&cap).copied().unwrap_or(Route::Wrapper)
    }

    /// Parse a `[routes]` table from `.rift/rewrite.toml` (minimal: `key = "route"`).
    /// Unknown keys/values are ignored (the default route is `Wrapper`).
    #[must_use]
    pub fn from_toml_str(s: &str) -> Self {
        let mut table = Self::default();
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
                continue;
            }
            if let Some((key, val)) = line.split_once('=') {
                let cap = match key.trim() {
                    "hashing" => Some(Capability::CheckpointRoot),
                    "git_write" | "archive" => Some(Capability::Archive),
                    _ => None,
                };
                if let (Some(cap), Some(route)) = (cap, Route::parse(val)) {
                    table.routes.insert(cap, route);
                }
            }
        }
        table
    }
}

/// A recorded parity divergence (wrapper vs native) seen during `DualRun`. These
/// accumulate but are **never committed** to the ledger; they gate promotion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    /// The capability that diverged.
    pub capability: Capability,
    /// The wrapper's (canonical) result, rendered (hex / message).
    pub wrapper: String,
    /// The native result, rendered (hex / message).
    pub native: String,
}

/// The seam the strangler grows along. Both backends implement it identically.
pub trait ShadowBackend {
    /// Archive a checkpoint shard for a commit (the git-write orchestration).
    ///
    /// # Errors
    /// [`RiftError`] on an archive failure.
    fn archive_checkpoint(&self, intent: &CaptureIntent) -> Result<()>;

    /// The checkpoint Merkle root over its ordered event leaves (BYTE-critical).
    /// Both backends share the native implementation (no Entire equivalent), so
    /// this defaults to it.
    fn checkpoint_root(&self, events: &[CanonicalEvent]) -> Sha256Id {
        crate::checkpoint_merkle_root(events)
    }
}

/// The wrapper backend: archival shells out to the pinned capture binary via the
/// [`EntireRunner`] seam. Borrows the runner so callers/tests retain it.
pub struct EntireBackend<'a, R: EntireRunner> {
    runner: &'a R,
}

impl<'a, R: EntireRunner> EntireBackend<'a, R> {
    /// Wrap a runner.
    pub fn new(runner: &'a R) -> Self {
        Self { runner }
    }
}

impl<R: EntireRunner> ShadowBackend for EntireBackend<'_, R> {
    fn archive_checkpoint(&self, intent: &CaptureIntent) -> Result<()> {
        self.runner.run(intent).map_err(map_archive_err)?;
        Ok(())
    }
}

/// The native backend. Hashing/attribution/cost/redaction are already native (the
/// default trait methods); the **git write** archival is the remaining route, which
/// uses the native plumbing in `rift-git` (P7 S3). The byte/layout-compatible native
/// archive lands with the parity work (P7 S1) — until promoted it is never used (the
/// route stays `Wrapper`), and is reported here as not-yet-implemented.
#[derive(Default)]
pub struct NativeBackend;

impl ShadowBackend for NativeBackend {
    fn archive_checkpoint(&self, _intent: &CaptureIntent) -> Result<()> {
        Err(RiftError::validity(
            "native checkpoint archive is not yet promoted (P7 parity pending)",
        ))
    }
}

/// The composite backend that routes each capability per the [`RouteTable`].
pub struct RoutedBackend<E: ShadowBackend, N: ShadowBackend> {
    entire: E,
    native: N,
    routes: RouteTable,
    divergences: RefCell<Vec<Divergence>>,
}

impl<E: ShadowBackend, N: ShadowBackend> RoutedBackend<E, N> {
    /// Compose a wrapper + native backend under a route table.
    pub fn new(entire: E, native: N, routes: RouteTable) -> Self {
        Self {
            entire,
            native,
            routes,
            divergences: RefCell::new(Vec::new()),
        }
    }

    /// Drain the recorded field-parity divergences (for promotion gating / logging).
    pub fn take_divergences(&self) -> Vec<Divergence> {
        self.divergences.borrow_mut().drain(..).collect()
    }
}

impl<E: ShadowBackend, N: ShadowBackend> ShadowBackend for RoutedBackend<E, N> {
    fn archive_checkpoint(&self, intent: &CaptureIntent) -> Result<()> {
        match self.routes.route(Capability::Archive) {
            Route::Wrapper => self.entire.archive_checkpoint(intent),
            Route::Native => self.native.archive_checkpoint(intent),
            Route::DualRun => {
                // The wrapper result is canonical and is what the ledger receives.
                let w = self.entire.archive_checkpoint(intent);
                // The native path is exercised for validation only; an error is
                // recorded as a divergence and never affects the wrapper result.
                if let Err(e) = self.native.archive_checkpoint(intent) {
                    self.divergences.borrow_mut().push(Divergence {
                        capability: Capability::Archive,
                        wrapper: "ok".into(),
                        native: format!("error: {e}"),
                    });
                }
                w
            }
        }
    }

    fn checkpoint_root(&self, events: &[CanonicalEvent]) -> Sha256Id {
        match self.routes.route(Capability::CheckpointRoot) {
            Route::Wrapper => self.entire.checkpoint_root(events),
            Route::Native => self.native.checkpoint_root(events),
            Route::DualRun => {
                let w = self.entire.checkpoint_root(events);
                let n = self.native.checkpoint_root(events);
                if w != n {
                    self.divergences.borrow_mut().push(Divergence {
                        capability: Capability::CheckpointRoot,
                        wrapper: w.to_hex(),
                        native: n.to_hex(),
                    });
                }
                w // the trusted wrapper result is always returned during DualRun
            }
        }
    }
}

/// Persist a soak window's divergences as an append-only `verification_run` + its
/// `verification_divergence` rows (the CI/rollout dashboard). A run with zero
/// divergences records the run with `exit_code = 0` and no divergence rows.
///
/// # Errors
/// [`RiftError::Storage`] on an insert failure.
pub async fn persist_divergences(
    store: &impl CanonicalStore,
    run_id: &str,
    divergences: &[Divergence],
    now_ms: i64,
) -> Result<()> {
    let exit_code = i64::from(!divergences.is_empty());
    let outcome = sha256_hex(&[run_id.as_bytes(), b"\0", &exit_code.to_le_bytes()]);
    record_verification_run(
        store,
        &VerificationRunRow {
            run_id: run_id.to_owned(),
            branch: "rift/checkpoints/v1".to_owned(),
            exit_code,
            checked_at: now_ms,
            outcome_sha256: outcome,
            schema_version: 1,
        },
    )
    .await?;
    for (i, d) in divergences.iter().enumerate() {
        record_verification_divergence(
            store,
            &VerificationDivergenceRow {
                run_id: run_id.to_owned(),
                ordinal: i64::try_from(i).unwrap_or(0),
                checkpoint_id: String::new(),
                layer: d.capability.as_str().to_owned(),
                site: "DualRunWrapperVsNative".to_owned(),
                detail: format!("wrapper={} native={}", d.wrapper, d.native),
                schema_version: 1,
            },
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_shadow_entire::MockRunner;

    /// A backend whose checkpoint_root deliberately diverges (for dual-run tests).
    struct DivergentNative;
    impl ShadowBackend for DivergentNative {
        fn archive_checkpoint(&self, _intent: &CaptureIntent) -> Result<()> {
            Ok(())
        }
        fn checkpoint_root(&self, _events: &[CanonicalEvent]) -> Sha256Id {
            Sha256Id::from_bytes([0xAB; 32]) // never matches the real root
        }
    }

    fn intent() -> CaptureIntent {
        CaptureIntent::ArchiveCheckpoint {
            repo: std::path::PathBuf::from("/tmp/r"),
            commit: "abc123".to_owned(),
        }
    }

    #[test]
    fn router_defaults_to_wrapper() {
        let t = RouteTable::all_wrapper();
        assert_eq!(t.route(Capability::Archive), Route::Wrapper);
        assert_eq!(t.route(Capability::CheckpointRoot), Route::Wrapper);
    }

    #[test]
    fn wrapper_route_invokes_the_runner() {
        let runner = MockRunner::default();
        let rb = RoutedBackend::new(
            EntireBackend::new(&runner),
            NativeBackend,
            RouteTable::all_wrapper(),
        );
        rb.archive_checkpoint(&intent()).unwrap();
        assert_eq!(
            runner.calls.borrow().len(),
            1,
            "wrapper archived via the runner"
        );
    }

    #[test]
    fn dual_run_returns_wrapper_and_records_divergence() {
        let runner = MockRunner::default();
        let routes = RouteTable::all_wrapper().with(Capability::CheckpointRoot, Route::DualRun);
        let rb = RoutedBackend::new(EntireBackend::new(&runner), DivergentNative, routes);
        // The wrapper (real native impl) result is returned, but native diverges →
        // exactly one divergence is recorded (and the wrapper result is canonical).
        let _ = rb.checkpoint_root(&[]);
        let div = rb.take_divergences();
        assert_eq!(div.len(), 1);
        assert_eq!(div[0].capability, Capability::CheckpointRoot);
    }

    #[test]
    fn kill_switch_forces_wrapper() {
        let mut routes = RouteTable::all_wrapper().with(Capability::Archive, Route::Native);
        assert_eq!(routes.route(Capability::Archive), Route::Native);
        routes.trip_kill_switch();
        assert_eq!(routes.route(Capability::Archive), Route::Wrapper);
    }

    #[test]
    fn rewrite_toml_parses_routes() {
        let toml = "[routes]\nhashing = \"native\"\ngit_write = \"dual_run\"\n";
        let t = RouteTable::from_toml_str(toml);
        assert_eq!(t.route(Capability::CheckpointRoot), Route::Native);
        assert_eq!(t.route(Capability::Archive), Route::DualRun);
    }
}
