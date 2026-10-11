//! `gate-intercept` — the read-only guard on the read-interception surface.
//!
//! Every leg below answers one question about the tree AS TRACKED BY GIT, with
//! no build and no cargo subprocess: the gate is meant to run in seconds on a
//! checkout, before anything expensive, and to say plainly which of its legs
//! passed.
//!
//! What each leg holds:
//!
//! * `one-gate-chokepoint` — one file composes an agent's hook response.
//! * `pinned-channels` — the channels Rift answers on are reviewable as a list.
//! * `no-store-on-gate-path` — the decision path opens no store and loads no
//!   tokenizer.
//! * `one-reason-renderer` — one module writes the sentences a model reads.
//! * `closed-rewrite-forms` — every tool-call rewrite is one of three pinned forms.
//! * `family-keyed-caches` — a cache is keyed by the repository family, never
//!   by which checkout happened to be open.
//! * `one-cache-read` — the decision path reads one family cache, through one
//!   named function, keyed by the common dir and the content address.
//! * `regions-at-boundaries` — no hook and no detached worker reaches the
//!   writer of a Rift-managed region.
//! * `load-bearing` — not built; see [`load_bearing`].
//!
//! Each leg reports its own violations; the command fails if any leg does.
//!
//! **The honest limits, stated once.** These are text screens over source, not
//! a compiler. They read the production half of a file (everything before its
//! trailing test module, the same horizon the CI meta-checks use), with
//! comments and string bodies masked out where a leg is asking about code
//! rather than prose. They resolve a call by bare function name, which
//! over-matches (a method and a free function of the same name are one node)
//! and never follows a trait object or a function pointer. Every limit is
//! chosen so a screen errs toward reporting: an ambiguous shape is a finding,
//! not an exemption.

mod cache_read;
mod channels;
mod chokepoint;
mod family_caches;
mod gate_path;
mod managed_regions;
mod renderer;
mod rewrites;
mod text;
mod unbuilt;

use std::path::Path;

use crate::ci_meta::tracked_paths;

pub use cache_read::one_cache_read;
pub use channels::pinned_channels;
pub use chokepoint::one_gate_chokepoint;
pub use family_caches::family_keyed_caches;
pub use gate_path::no_store_on_gate_path;
pub use managed_regions::regions_at_boundaries;
pub use renderer::one_reason_renderer;
pub use rewrites::closed_rewrite_forms;
pub use text::{calls_within, fn_bodies, mask_code, mask_comments, FnBody};
pub use unbuilt::load_bearing;

/// One leg's outcome: its name, whether it was built, and its findings.
#[derive(Debug)]
pub struct Leg {
    /// The leg's name, as the plan and the printed line spell it.
    pub name: &'static str,
    /// What the leg concluded, in one line, whether it passed or failed.
    pub detail: String,
    /// Everything the leg objects to. Empty means the leg holds.
    pub violations: Vec<String>,
}

impl Leg {
    fn new(name: &'static str, detail: String, violations: Vec<String>) -> Self {
        Self {
            name,
            detail,
            violations,
        }
    }
}

/// Run every leg over the tracked tree.
///
/// # Errors
/// If a leg cannot read what it screens.
pub fn run_all(root: &Path) -> Result<Vec<Leg>, String> {
    Ok(vec![
        one_gate_chokepoint(root)?,
        pinned_channels(root)?,
        no_store_on_gate_path(root)?,
        one_reason_renderer(root)?,
        closed_rewrite_forms(root)?,
        family_keyed_caches(root)?,
        one_cache_read(root)?,
        regions_at_boundaries(root)?,
        load_bearing(),
    ])
}

/// Every tracked `.rs` file under `crates/` and `bins/`, as `(path, text)`.
///
/// Tracked, not walked: an untracked scratch file is not screened, and a
/// tracked file deleted on disk is a hard error rather than a quiet pass.
///
/// # Errors
/// If `git ls-files` fails or a tracked file cannot be read.
fn rust_corpus(root: &Path) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for rel in tracked_paths(root)? {
        let is_rust = Path::new(&rel)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"));
        if !is_rust || !(rel.starts_with("crates/") || rel.starts_with("bins/")) {
            continue;
        }
        let text = std::fs::read_to_string(root.join(&rel))
            .map_err(|e| format!("read tracked file {rel}: {e}"))?;
        out.push((rel, text));
    }
    if out.is_empty() {
        return Err("gate-intercept: the tracked Rust corpus is empty".to_owned());
    }
    Ok(out)
}

/// True for a path whose contents are test scaffolding rather than shipped
/// code: an integration-test file, a bench, or a pinned fixture.
fn is_test_surface(rel: &str) -> bool {
    rel.contains("/tests/") || rel.contains("/benches/") || rel.contains("/fixtures/")
}

/// Line number (1-based) of the byte offset `at` in `text`.
fn line_of(text: &str, at: usize) -> usize {
    text.get(..at).unwrap_or("").matches('\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The gate as it stands on this tree is green.** A gate whose only
    /// evidence is synthetic is a gate nobody has pointed at the real thing.
    #[test]
    fn every_built_leg_holds_on_this_workspace() {
        let legs = run_all(&crate::workspace_root()).expect("every leg runs");
        for leg in &legs {
            assert!(
                leg.violations.is_empty(),
                "{}: {:#?}",
                leg.name,
                leg.violations
            );
        }
        assert_eq!(legs.len(), 9, "every leg reports, built or not");
    }

    /// The two call-graph walks are dispatched by the runner, not only
    /// callable: their planting tests call the walks directly, so they would
    /// stay green with a leg dropped from the command.
    #[test]
    fn the_runner_dispatches_both_call_graph_walks() {
        let legs = run_all(&crate::workspace_root()).expect("every leg runs");
        assert!(
            legs.iter().any(|leg| leg.name == "no-store-on-gate-path"),
            "{:?}",
            legs.iter().map(|leg| leg.name).collect::<Vec<_>>()
        );
        assert!(
            legs.iter().any(|leg| leg.name == "regions-at-boundaries"),
            "{:?}",
            legs.iter().map(|leg| leg.name).collect::<Vec<_>>()
        );
    }
}
