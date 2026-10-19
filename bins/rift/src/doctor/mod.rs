//! `rift doctor`: diagnose this repository's capture health without any
//! network I/O, one leg per concern, each leg a line and a JSON object.
//!
//! The legs are grouped by what they read: `setup` the repository's own
//! configuration and the memory chain, `capture` the checkpoint record
//! against its projection, `stores` the caches beside the projection,
//! `git_hooks` the hooks git runs and the binary they name, `health` whether
//! each wired agent's capture is arriving, `worktrees` the family's members,
//! `family` the recorded identity against the one its minting rule derives,
//! `queues` the work each member still owes, `spool` the originals of
//! rewritten results still spooled, `redaction` the checkpoints whose
//! transcripts the redaction gate sealed by its per-line fallback,
//! `turn_seals` the sessions whose last turn-end seal fell back or lags,
//! `set_asides` the recall
//! projections a rebuild set aside, `prices` the observed models the price
//! catalog cannot price. This file assembles the report
//! in the order the legs print.

mod capture;
mod completeness;
mod family;
mod git_hooks;
pub(crate) mod health;
mod prices;
mod queues;
mod redaction;
mod set_asides;
mod setup;
mod spool;
mod stores;
mod turn_seals;
mod worktrees;

use std::path::Path;

use crate::cli::Outcome;
use crate::projection::worktree_root;

use capture::{
    doctor_capture_chain, doctor_shadow_refs, doctor_soak, doctor_stale_rewrite_toml,
    doctor_turn_boundaries, doctor_watermark,
};
use completeness::doctor_capture_completeness;
use family::doctor_family_identity;
use git_hooks::{doctor_binary_of_record, doctor_hooks_target};
use health::doctor_integration_health;
use prices::doctor_unpriced_models;
use queues::{doctor_capture_queue, doctor_push_queue, doctor_session_sweep};
use redaction::doctor_redaction_fallback;
use set_asides::doctor_recall_set_asides;
use setup::{
    doctor_config_load, doctor_memory_chain, doctor_retired_config_keys, doctor_sibling_tool_hooks,
};
use spool::doctor_original_spool;
use stores::{doctor_basis_store, doctor_document_store, doctor_window_journals};
use turn_seals::doctor_turn_seals;
use worktrees::{
    doctor_orphan_projections, doctor_phantom_gitdirs, doctor_prunable_worktrees,
    doctor_worktree_wiring,
};

/// `rift doctor`: diagnose this repository's capture health without any network
/// I/O. Repo-scoped legs diagnose the discovered worktree root.
pub(crate) fn cmd_doctor(start: &Path, fix: bool) -> Outcome {
    cmd_doctor_with_mode(start, fix, false)
}

/// Viewer-safe doctor: every leg is report-only, existing stores are opened
/// read-only, and one-shot capture markers are observed without consumption.
/// The palette's `/doctor`, and so the dashboard's alone.
#[cfg(feature = "tui")]
pub(crate) fn cmd_doctor_read_only(start: &Path) -> Outcome {
    cmd_doctor_with_mode(start, false, true)
}

/// The report, one leg at a time: a line appended to the human text and an
/// object stored under the leg's key. The two are written in the same call
/// so a leg cannot be printed and left out of `--json`, or the reverse.
struct DoctorReport {
    human: String,
    json: serde_json::Map<String, serde_json::Value>,
}

impl DoctorReport {
    fn leg(&mut self, key: &str, (human, json): (String, serde_json::Value)) {
        self.human.push_str(&human);
        self.json.insert(key.to_owned(), json);
    }
}

/// Every leg in print order. `fix` alone drives the legs whose repair is safe
/// from the viewer too; the ones that write to a store or a hooks directory
/// take `repair`, which the read-only tier never sets.
fn cmd_doctor_with_mode(start: &Path, fix: bool, strict_read_only: bool) -> Outcome {
    let root = &worktree_root(start);
    let repair = fix && !strict_read_only;
    // A report publishes no job and moves no file: every run-state path a
    // report-only leg resolves is found, never recorded or journaled.
    let _report = (!fix).then(crate::family::run_state::report_only);
    let mut report = DoctorReport {
        human: String::from("rift doctor"),
        json: serde_json::Map::new(),
    };
    report.leg("memory_chain", doctor_memory_chain(root, strict_read_only));
    report.leg("sessions", doctor_session_sweep(root, fix));
    report.leg("capture_queue", doctor_capture_queue(root, fix));
    report.leg("capture_chain", doctor_capture_chain(root, fix));
    report.leg("watermark", doctor_watermark(root, fix, strict_read_only));
    report.leg("shadow_refs", doctor_shadow_refs(root));
    report.leg("soak", doctor_soak(root, strict_read_only));
    report.leg("stale_rewrite_toml", doctor_stale_rewrite_toml(root));
    report.leg("phantom_gitdirs", doctor_phantom_gitdirs(root, fix));
    report.leg("push_queue", doctor_push_queue(root, repair));
    report.leg("prunable_worktrees", doctor_prunable_worktrees(root, fix));
    report.leg("orphan_projections", doctor_orphan_projections(root));
    report.leg("family_identity", doctor_family_identity(root));
    report.leg(
        "turn_boundaries",
        doctor_turn_boundaries(root, strict_read_only),
    );
    report.leg("prices", doctor_unpriced_models(root));
    report.leg("hooks_target", doctor_hooks_target(root, repair));
    report.leg("binary_of_record", doctor_binary_of_record(root));
    report.leg("integration_health", doctor_integration_health(root));
    report.leg("worktree_wiring", doctor_worktree_wiring(root, repair));
    report.leg("window_journals", doctor_window_journals(root));
    report.leg("config_load", doctor_config_load(root));
    report.leg("retired_config_keys", doctor_retired_config_keys(root));
    report.leg("document_store", doctor_document_store(root));
    report.leg("basis_store", doctor_basis_store(root));
    report.leg("original_spool", doctor_original_spool(root));
    report.leg("redaction_fallback", doctor_redaction_fallback(root));
    report.leg("turn_seals", doctor_turn_seals(root));
    report.leg("completeness", doctor_capture_completeness(root));
    report.leg("recall_set_asides", doctor_recall_set_asides(root));
    report.leg("sibling_tool_hooks", doctor_sibling_tool_hooks(root));
    report.json.insert("fix".to_owned(), serde_json::json!(fix));
    Outcome::ok(report.human, serde_json::Value::Object(report.json))
}
