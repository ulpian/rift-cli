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
//! * `ceilings` — no tracked `.rs` file over 1,500 lines and no function over
//!   80, read by a lexer that masks comments and strings first.
//! * `guide_size` — the tracked guide stays within its byte ceiling.
//! * `ignores` — every `#[ignore]` carries its reason.
//! * `cache_root` — the `rift` binary resolves its cache root in one function,
//!   the one its test build redirects to a temp directory.
//! * `plain_cipher` — no production code under `bins/` or the recall crates
//!   names the no-key `PlainCipher`, so sensitive columns stay encrypted.
//!
//! This file holds what the screens share — the file read, the two walkers
//! that define "every first-party source file" and "every tracked path", the
//! two-way list comparison — and [`run_all`], which is the order the
//! `ci-meta` command runs them in, with the [`MetaReport`] it returns.
//!
//! `REQUIRED_JOBS` is why a job cannot be quietly dropped from the workflow:
//! the jobs enforce the invariants, and this list enforces the jobs.

mod cache_root;
mod ceilings;
mod deleted_paths;
mod fuzz;
mod fuzzy_matcher;
mod guide_size;
mod ignores;
mod lints;
mod network;
mod plain_cipher;
mod proptest_newlines;
mod retired_vocabulary;
mod schema;
mod workflow;

use std::path::Path;

pub use cache_root::one_cache_root_in_the_rift_binary;
pub use ceilings::{ceilings_hold, Measured};
pub use deleted_paths::deleted_paths_stay_deleted;
pub use fuzz::fuzz_targets_all_run_in_ci;
pub use fuzzy_matcher::fuzzy_matcher_reads_within_bounds;
pub use guide_size::{guide_within_ceiling, GUIDE_BYTE_CEILING};
pub use ignores::every_ignore_carries_a_reason;
pub use lints::{
    lint_table_matches_baseline, no_blanket_allow_at_crate_root, unsafe_only_in_sanctioned_modules,
};
pub use network::{no_network_crate_in_the_resolved_graph, no_network_deps_in_capture_crates};
pub use plain_cipher::no_plain_cipher_in_production;
pub use proptest_newlines::proptest_generators_reach_newlines;
pub use retired_vocabulary::retired_vocabulary_screen;
pub use schema::no_schema_fastpath_in_bins;
pub(crate) use schema::production_half;
pub use workflow::{
    dialect_conformance_job_runs_every_suite, required_jobs_present, workflow_permissions_read_only,
};

/// What [`run_all`] found: every violation, and how much each counting
/// screen examined.
///
/// The counts travel with the result so the caller can print them: a screen
/// over an empty corpus is a screen that guards nothing, and the numbers on
/// the ok line are what make that visible without reading this code. They
/// are also how a test sees that the runner dispatched a screen at all.
#[derive(Debug, Default)]
pub struct MetaReport {
    /// Tracked files the retired-vocabulary screen read.
    pub vocabulary_files: usize,
    /// `.rs` files and functions the ceiling screen measured.
    pub ceilings: Measured,
    /// `#[ignore]` attributes the reason screen read.
    pub ignores: usize,
    /// `bins/rift/src/` files the cache-root screen read.
    pub cache_root_files: usize,
    /// `bins/` and recall-crate files the plain-cipher screen read.
    pub plain_cipher_files: usize,
    /// Bytes in the tracked guide, measured against its ceiling.
    pub guide_bytes: u64,
    /// Everything any screen objects to. Empty means `ci-meta` passes.
    pub violations: Vec<String>,
}

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

/// Run every meta-check, returning each counting screen's count and all
/// violations.
///
/// # Errors
/// If any input file cannot be read.
pub fn run_all(root: &Path) -> Result<MetaReport, String> {
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
    let (vocabulary_files, retired) = retired_vocabulary_screen(root)?;
    v.extend(retired);
    let (ceilings, oversized) = ceilings_hold(root)?;
    v.extend(oversized);
    let (ignores, bare) = every_ignore_carries_a_reason(root)?;
    v.extend(bare);
    let (cache_root_files, resolved) = one_cache_root_in_the_rift_binary(root)?;
    v.extend(resolved);
    let (plain_cipher_files, plain) = no_plain_cipher_in_production(root)?;
    v.extend(plain);
    let (guide_bytes, oversized_guide) = guide_within_ceiling(root)?;
    v.extend(oversized_guide);
    Ok(MetaReport {
        vocabulary_files,
        ceilings,
        ignores,
        cache_root_files,
        plain_cipher_files,
        guide_bytes,
        violations: v,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_root;

    /// The scratch names handed out in this process: tests run on threads,
    /// so two sharing a name would clear each other's fixtures.
    static CLAIMED_NAMES: std::sync::Mutex<std::collections::BTreeSet<String>> =
        std::sync::Mutex::new(std::collections::BTreeSet::new());

    /// A deterministic, ignored scratch directory for the fixtures below —
    /// under the build directory, named for the test, no clock and no
    /// randomness. The root is keyed by process id as well as by name: two
    /// test processes in one checkout (a gate beside a worker's leg) would
    /// otherwise clear and refill each other's fixtures, and isolation wins
    /// over a re-run reusing the same path. A reused pid finds its root
    /// cleared on first use. The empty per-process directory a run leaves
    /// behind is a few bytes and goes with `cargo clean`; a held `TempDir`
    /// was rejected because a static is never dropped, so it would not clean
    /// either, and a guard per call would change every caller.
    pub(super) fn scratch_dir(name: &str) -> std::path::PathBuf {
        let fresh = CLAIMED_NAMES.lock().unwrap().insert(name.to_owned());
        assert!(
            fresh,
            "two fixtures in one process share the scratch name `{name}`"
        );
        scratch_dir_for(name, std::process::id())
    }

    fn scratch_dir_for(name: &str, process: u32) -> std::path::PathBuf {
        let root = workspace_root()
            .join("target")
            .join("xtask-fixtures")
            .join(process.to_string())
            .join(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// Two processes running these tests in one checkout — a gate beside a
    /// worker's leg — each clear and refill their fixture root; sharing one
    /// root per name let one delete the other's files mid-test.
    #[test]
    fn one_fixture_name_in_two_processes_gets_two_disjoint_roots() {
        // The name carries this run's pid so two concurrent runs of this
        // very test never share the two fixed discriminators' roots.
        let name = format!("two-processes-{}", std::process::id());
        let first = scratch_dir_for(&name, u32::MAX);
        std::fs::write(first.join("planted.txt"), "first").unwrap();

        let second = scratch_dir_for(&name, u32::MAX - 1);

        assert_ne!(first, second);
        assert_eq!(
            std::fs::read_to_string(first.join("planted.txt")).unwrap(),
            "first"
        );
        std::fs::remove_dir_all(&first).unwrap();
        std::fs::remove_dir_all(&second).unwrap();
    }

    /// The meta-checks pass on the real repository — this is the actual gate,
    /// run both here (cargo test -p xtask) and by the `ci-meta` CI job.
    #[test]
    fn repository_passes_all_meta_checks() {
        let report = run_all(&workspace_root()).expect("meta checks run");
        assert!(
            report.violations.is_empty(),
            "ci-meta violations:\n{}",
            report.violations.join("\n")
        );
        assert!(
            report.vocabulary_files > 400,
            "the screen examined only {} files",
            report.vocabulary_files
        );
    }

    /// The runner dispatches the ceiling and ignore screens, not only their
    /// functions: their planting tests call the screens directly, so they
    /// would stay green with either dropped from `run_all`. The runner cannot
    /// run over a synthetic root — it resolves the real workspace's
    /// dependency graph and reads its workflow and lock files — so it runs
    /// over this tree, and each screen's count is the proof it ran.
    #[test]
    fn the_runner_dispatches_the_ceiling_and_ignore_screens() {
        let report = run_all(&workspace_root()).expect("meta checks run");
        assert!(
            report.ceilings.files > 500 && report.ceilings.functions > 10_000,
            "the ceiling screen measured {:?}",
            report.ceilings
        );
        assert!(
            report.ignores >= 4,
            "the ignore screen read {} attributes",
            report.ignores
        );
    }

    /// The runner dispatches the cache-root and plain-cipher screens: their
    /// planting tests call the screens directly, so only the counts they
    /// leave on the report show that `run_all` ran them.
    #[test]
    fn the_runner_dispatches_the_cache_root_screen() {
        let report = run_all(&workspace_root()).expect("meta checks run");
        assert!(
            report.cache_root_files > 100,
            "the cache-root screen read {} files",
            report.cache_root_files
        );
        assert!(
            report.plain_cipher_files > report.cache_root_files,
            "the plain-cipher screen read {} files",
            report.plain_cipher_files
        );
    }
}
