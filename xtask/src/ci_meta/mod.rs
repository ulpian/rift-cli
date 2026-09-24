//! CI/lint meta-checks: guards on the guards. Each check returns the list of
//! violations (empty ⇒ pass); the `ci-meta` command fails if any check
//! reports one.
//!
//! One module per concern, because a screen and the shapes that must still
//! trip it are read together:
//!
//! * `workflow` — the jobs `.github/workflows/ci.yml` must declare, the
//!   permissions it may request, and the dialect suites its named job runs.
//! * `lints` — the workspace lint table against `ci/lint-baseline.txt`,
//!   crate-root blanket allows, and `unsafe` outside a sanctioned module.
//! * `schema` — `bins/` production code never opens a projection through
//!   the `create_canonical_schema` fast-path.
//! * `proptest_newlines` — no new proptest generator is newline-blind.
//! * `fuzz` — every fuzz target on disk is declared and actually run.
//! * `deleted_paths` — the paths an owner decision deleted stay deleted.
//! * `retired_vocabulary` — no tracked path or tracked byte carries a
//!   retired program identifier.
//! * `network` — neither shipped binary reaches an HTTP client, in the
//!   workspace manifests or in the default-resolved graph.
//! * `fuzzy_matcher` — the matcher's pin still keeps its reads in bounds.
//!
//! This file holds what the screens share — the file read, the two walkers
//! that define "every first-party source file" and "every tracked path", the
//! two-way list comparison — and [`run_all`], which is the order the
//! `ci-meta` command runs them in.
//!
//! `REQUIRED_JOBS` is why a job cannot be quietly dropped from the workflow:
//! the jobs enforce the invariants, and this list enforces the jobs.

mod deleted_paths;
mod fuzz;
mod fuzzy_matcher;
mod lints;
mod network;
mod proptest_newlines;
mod retired_vocabulary;
mod schema;
mod workflow;

use std::path::Path;

pub use deleted_paths::deleted_paths_stay_deleted;
pub use fuzz::fuzz_targets_all_run_in_ci;
pub use fuzzy_matcher::fuzzy_matcher_reads_within_bounds;
pub use lints::{
    lint_table_matches_baseline, no_blanket_allow_at_crate_root, unsafe_only_in_sanctioned_modules,
};
pub use network::{no_network_crate_in_the_resolved_graph, no_network_deps_in_capture_crates};
pub use proptest_newlines::proptest_generators_reach_newlines;
pub use retired_vocabulary::retired_vocabulary_screen;
pub use schema::no_schema_fastpath_in_bins;
pub(crate) use schema::production_half;
pub use workflow::{
    dialect_conformance_job_runs_every_suite, required_jobs_present, workflow_permissions_read_only,
};

fn read(root: &Path, rel: &str) -> Result<String, String> {
    std::fs::read_to_string(root.join(rel)).map_err(|e| format!("read {rel}: {e}"))
}

/// Walk all first-party `.rs` files under `crates/`, `bins/`, `xtask/`.
///
/// `pub(crate)` so that every guard here shares ONE definition of
/// "first-party": two walkers with two definitions is how a file ends up
/// guarded by neither.
pub(crate) fn source_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = ["crates", "bins", "xtask"]
        .iter()
        .map(|d| root.join(d))
        .collect();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    out
}

/// Every path Git tracks under `root`, relative, in `git ls-files -z` order.
///
/// ONE walker for every "all tracked files" screen, so a root document
/// (`README.md`, `.gitignore`, a workflow) is either visible to all of them
/// or to none — never to one screen and not another. The walk is the index,
/// not the filesystem: an untracked scratch file is not scanned, a tracked
/// file that has been deleted on disk still is (and the caller's read reds).
///
/// # Errors
/// If `git ls-files` fails or returns a non-UTF-8 path.
pub(crate) fn tracked_paths(root: &Path) -> Result<Vec<String>, String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output()
        .map_err(|e| format!("git ls-files (tracked-path walk): {e}"))?;
    if !output.status.success() {
        return Err("git ls-files (tracked-path walk) failed".to_owned());
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|p| !p.is_empty())
        .map(|raw| {
            String::from_utf8(raw.to_vec())
                .map_err(|_| "git ls-files returned a non-UTF-8 path".to_owned())
        })
        .collect()
}

/// One fuzz list against the targets on disk, in both directions.
fn list_violations(label: &str, on_disk: &[String], listed: &[String]) -> Vec<String> {
    let mut v = Vec::new();
    for target in on_disk {
        if !listed.contains(target) {
            v.push(format!(
                "{label} fuzz target `{target}` — a target nobody runs is not a gate"
            ));
        }
    }
    for target in listed {
        if !on_disk.contains(target) {
            v.push(format!(
                "a fuzz list names `{target}`, but fuzz/fuzz_targets/{target}.rs does not exist"
            ));
        }
    }
    v
}

/// Run every meta-check, returning the retired-vocabulary screen's scanned
/// count and all violations.
///
/// The count travels with the result so the caller can print it: a screen
/// over an empty corpus is a screen that guards nothing, and the number on
/// the ok line is what makes that visible without reading this code.
///
/// # Errors
/// If any input file cannot be read.
pub fn run_all(root: &Path) -> Result<(usize, Vec<String>), String> {
    let mut v = Vec::new();
    v.extend(required_jobs_present(root)?);
    v.extend(workflow_permissions_read_only(root)?);
    v.extend(lint_table_matches_baseline(root)?);
    v.extend(unsafe_only_in_sanctioned_modules(root)?);
    v.extend(no_blanket_allow_at_crate_root(root)?);
    v.extend(no_schema_fastpath_in_bins(root)?);
    v.extend(fuzz_targets_all_run_in_ci(root)?);
    v.extend(proptest_generators_reach_newlines(root)?);
    v.extend(no_network_deps_in_capture_crates(root)?);
    v.extend(dialect_conformance_job_runs_every_suite(root)?);
    v.extend(fuzzy_matcher_reads_within_bounds(root)?);
    v.extend(no_network_crate_in_the_resolved_graph(root)?);
    v.extend(deleted_paths_stay_deleted(root)?.1);
    let (scanned, retired) = retired_vocabulary_screen(root)?;
    v.extend(retired);
    Ok((scanned, v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_root;

    /// A deterministic, ignored scratch directory for the fixtures below —
    /// under the build directory, named for the test, no clock and no
    /// randomness, so a re-run reuses the same path.
    pub(super) fn scratch_dir(name: &str) -> std::path::PathBuf {
        let root = workspace_root()
            .join("target")
            .join("xtask-fixtures")
            .join(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// The meta-checks pass on the real repository — this is the actual gate,
    /// run both here (cargo test -p xtask) and by the `ci-meta` CI job.
    #[test]
    fn repository_passes_all_meta_checks() {
        let (scanned, violations) = run_all(&workspace_root()).expect("meta checks run");
        assert!(
            violations.is_empty(),
            "ci-meta violations:\n{}",
            violations.join("\n")
        );
        assert!(scanned > 400, "the screen examined only {scanned} files");
    }
}
