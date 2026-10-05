//! Screens over `.github/workflows/ci.yml`: the jobs the workflow must
//! declare, the permissions it may request, and the dialect suites the
//! named job has to run.
//!
//! One module because all three read the same file and share its parser:
//! a job body is located once, by `job_body`, rather than three times.
use std::path::Path;

use super::{list_violations, read};

/// The jobs the CI workflow must always declare.
const REQUIRED_JOBS: &[&str] = &[
    "fmt:",
    "clippy:",
    "test:",
    "release-build:",
    "msrv:",
    "coverage:",
    "security:",
    "miri:",
    "fuzz-smoke:",
    "red-team:",
    "dialect-conformance:",
    "docs:",
    "ci-meta:",
    "gate-intercept:",
    "bench-harness:",
];

/// Every required job name appears in `.github/workflows/ci.yml`.
///
/// # Errors
/// If the workflow file cannot be read.
pub fn required_jobs_present(root: &Path) -> Result<Vec<String>, String> {
    let ci = read(root, ".github/workflows/ci.yml")?;
    Ok(REQUIRED_JOBS
        .iter()
        .filter(|j| !ci.contains(&format!("\n  {j}")))
        .map(|j| {
            format!(
                "ci.yml: required job `{}` is missing",
                j.trim_end_matches(':')
            )
        })
        .collect())
}

/// The workflow grants read-only permissions and never requests a write scope.
///
/// # Errors
/// If the workflow file cannot be read.
pub fn workflow_permissions_read_only(root: &Path) -> Result<Vec<String>, String> {
    let ci = read(root, ".github/workflows/ci.yml")?;
    let mut v = Vec::new();
    if !ci.contains("permissions:\n  contents: read") {
        v.push("ci.yml: top-level `permissions: contents: read` is missing".to_string());
    }
    if ci.contains(": write") || ci.contains("write-all") {
        v.push("ci.yml: a write permission scope crept in".to_string());
    }
    Ok(v)
}

/// The dialect-conformance suites: every
/// integration-test target the `dialect-conformance` job must run, by
/// `--test` name. Grow this when a dialect suite lands; the job's `--test`
/// words must equal it exactly.
const DIALECT_SUITES: [&str; 5] = [
    "legacy_import_golden",
    "dual_source",
    "shard_goldens",
    "parity_anchors",
    "gix_walker",
];

/// The body of one top-level job in the workflow: from its `\n  <job>:` line
/// to the next two-space-indented key. Empty when the job is absent.
fn job_body<'a>(ci: &'a str, job: &str) -> &'a str {
    let Some((_, rest)) = ci.split_once(&format!("\n  {job}:")) else {
        return "";
    };
    let end = rest
        .match_indices("\n  ")
        .find(|(i, _)| {
            rest.get(i + 3..)
                .is_some_and(|after| after.starts_with(|c: char| c.is_ascii_alphanumeric()))
        })
        .map_or(rest.len(), |(i, _)| i);
    rest.get(..end).unwrap_or("")
}

/// Every `--test <name>` word in a job body.
fn test_targets(body: &str) -> Vec<String> {
    let mut words = body.split_whitespace().peekable();
    let mut out = Vec::new();
    while let Some(w) = words.next() {
        if w == "--test" {
            if let Some(name) = words.next() {
                out.push(name.to_owned());
            }
        }
    }
    out
}

/// The `dialect-conformance` job runs exactly `DIALECT_SUITES` — set
/// equality both ways, mirroring [`fuzz_targets_all_run_in_ci`](super::fuzz::fuzz_targets_all_run_in_ci): a suite the
/// job dropped and a suite this list does not know are both violations.
///
/// # Errors
/// If the workflow file cannot be read.
pub fn dialect_conformance_job_runs_every_suite(root: &Path) -> Result<Vec<String>, String> {
    let ci = read(root, ".github/workflows/ci.yml")?;
    let body = job_body(&ci, "dialect-conformance");
    if body.is_empty() {
        return Ok(vec![
            "ci.yml: the `dialect-conformance` job is absent".to_owned()
        ]);
    }
    let listed = test_targets(body);
    let expected: Vec<String> = DIALECT_SUITES.iter().map(|s| (*s).to_owned()).collect();
    let mut v = list_violations(
        "ci.yml dialect-conformance does not run",
        &expected,
        &listed,
    );
    v.extend(list_violations(
        "DIALECT_SUITES does not declare (add it or drop it from the job)",
        &listed,
        &expected,
    ));
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci_meta::tests::scratch_dir;
    use crate::workspace_root;

    /// ★ The clippy job builds the binary's feature-off configuration.
    ///
    /// `bins/rift`'s manifest advertises `--no-default-features` — no terminal
    /// backend, no search seam, no document seam — and every other leg in CI
    /// builds the binary with its features ON, so an item that exists only for
    /// one of them, or a `use` of an optional crate that is not behind its
    /// feature, is invisible everywhere else. Both have happened here. The
    /// step is one line and deleting it would be one line, hence this: the
    /// job has to carry a leg that names the configuration, the package and
    /// the denial that makes it a gate rather than a report.
    #[test]
    fn the_clippy_job_checks_the_feature_off_configuration() {
        let ci = std::fs::read_to_string(workspace_root().join(".github/workflows/ci.yml"))
            .expect("read ci.yml");
        let body = job_body(&ci, "clippy");
        assert!(!body.is_empty(), "ci.yml: the `clippy` job is absent");
        let leg = body
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("- run:") || line.starts_with("cargo "))
            .find(|line| line.contains("--no-default-features"))
            .expect("ci.yml: the clippy job runs no `--no-default-features` leg");
        for word in ["-p rift", "--all-targets", "-D warnings"] {
            assert!(
                leg.contains(word),
                "ci.yml: the feature-off leg no longer says `{word}`: {leg}"
            );
        }
    }

    /// The dialect job's suite list is held by set equality: a dropped
    /// `--test`, an unknown one, and an absent job are each a violation; the
    /// real workflow has none.
    #[test]
    fn the_dialect_job_is_held_to_the_suite_list_both_ways() {
        let job = |run: &str| {
            format!("jobs:\n  red-team:\n    steps:\n      - run: x\n  dialect-conformance:\n    runs-on: ubuntu-latest\n    steps:\n      - run: {run}\n  miri:\n    steps:\n      - run: cargo test --test not_ours\n")
        };
        let full = DIALECT_SUITES
            .iter()
            .map(|s| format!("--test {s}"))
            .collect::<Vec<_>>()
            .join(" ");
        let real = job(&format!("cargo test -p rift-shadow {full}"));
        let body = job_body(&real, "dialect-conformance");
        assert!(!body.contains("not_ours"), "the body stops at the next job");
        let mut got = test_targets(body);
        got.sort_unstable();
        let mut want: Vec<String> = DIALECT_SUITES.iter().map(|s| (*s).to_owned()).collect();
        want.sort_unstable();
        assert_eq!(got, want);
        let dropped = job("cargo test -p rift-shadow --test legacy_import_golden");
        assert!(!list_violations(
            "x",
            &want,
            &test_targets(job_body(&dropped, "dialect-conformance"))
        )
        .is_empty());
        let unknown = job(&format!("cargo test {full} --test stranger"));
        assert!(!list_violations(
            "x",
            &test_targets(job_body(&unknown, "dialect-conformance")),
            &want
        )
        .is_empty());
        assert!(job_body("jobs:\n  miri:\n    steps: []\n", "dialect-conformance").is_empty());
        assert!(dialect_conformance_job_runs_every_suite(&workspace_root())
            .unwrap()
            .is_empty());
    }

    /// A workflow with one required job removed is caught and the missing job
    /// is named — the proof that this screen's silence over the real workflow
    /// means the jobs are declared, and not that the list is read as empty.
    #[test]
    fn a_workflow_missing_a_required_job_is_reported() {
        let root = scratch_dir("required-jobs");
        std::fs::create_dir_all(root.join(".github/workflows")).unwrap();
        let dropped = "miri:";
        let declared = REQUIRED_JOBS
            .iter()
            .filter(|job| **job != dropped)
            .map(|job| format!("  {job}\n    runs-on: ubuntu-latest"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(
            root.join(".github/workflows/ci.yml"),
            format!("jobs:\n{declared}"),
        )
        .unwrap();

        let violations = required_jobs_present(&root).expect("the screen runs");
        assert_eq!(
            violations,
            vec!["ci.yml: required job `miri` is missing".to_owned()]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The real workflow with its release build renamed away is reported, so a
    /// change that drops `cargo build --release` from CI cannot land silently.
    #[test]
    fn a_workflow_without_its_release_build_is_reported() {
        let root = scratch_dir("release-build");
        let dir = root.join(".github/workflows");
        std::fs::create_dir_all(&dir).unwrap();
        let ci = std::fs::read_to_string(workspace_root().join(".github/workflows/ci.yml"))
            .expect("the workflow is readable");
        assert!(
            ci.contains("\n  release-build:"),
            "the real workflow declares it"
        );
        let dropped = ci.replace("\n  release-build:", "\n  renamed-build:");
        std::fs::write(dir.join("ci.yml"), dropped).unwrap();

        let violations = required_jobs_present(&root).expect("the screen runs");
        assert_eq!(
            violations,
            vec!["ci.yml: required job `release-build` is missing".to_owned()]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Both halves of the permissions screen plant: a workflow that declares
    /// no read-only grant, and one that asks for a write scope.
    #[test]
    fn a_workflow_without_read_only_permissions_is_reported() {
        let root = scratch_dir("workflow-permissions");
        let dir = root.join(".github/workflows");
        std::fs::create_dir_all(&dir).unwrap();
        let plant = |body: &str| std::fs::write(dir.join("ci.yml"), body).unwrap();
        let screen = || workflow_permissions_read_only(&root).expect("the screen runs");

        plant("permissions:\n  contents: read\njobs:\n  fmt:\n");
        assert!(screen().is_empty(), "the read-only shape is the clean one");

        plant("jobs:\n  fmt:\n");
        assert_eq!(
            screen(),
            vec!["ci.yml: top-level `permissions: contents: read` is missing".to_owned()]
        );

        plant("permissions:\n  contents: read\n  packages: write\n");
        assert_eq!(
            screen(),
            vec!["ci.yml: a write permission scope crept in".to_owned()]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The screen itself over a planted workflow, not only its parsers: a job
    /// that dropped a suite and runs one the list does not know is reported in
    /// both directions, and a workflow without the job reports it absent.
    #[test]
    fn a_dialect_job_that_drifted_from_the_suite_list_is_reported() {
        let root = scratch_dir("dialect-job");
        let dir = root.join(".github").join("workflows");
        std::fs::create_dir_all(&dir).unwrap();
        let plant = |body: &str| std::fs::write(dir.join("ci.yml"), body).unwrap();
        let screen = || dialect_conformance_job_runs_every_suite(&root).expect("the screen runs");
        let dropped = DIALECT_SUITES[0];
        let kept = DIALECT_SUITES
            .iter()
            .skip(1)
            .map(|s| format!("--test {s}"))
            .collect::<Vec<_>>()
            .join(" ");

        plant(&format!(
            "jobs:\n  dialect-conformance:\n    steps:\n      - run: cargo test {kept} --test stranger\n"
        ));
        let violations = screen();
        assert_eq!(violations.len(), 4, "{violations:?}");
        for named in [dropped, "stranger"] {
            assert_eq!(
                violations
                    .iter()
                    .filter(|v| v.contains(&format!("`{named}`")))
                    .count(),
                2,
                "{violations:?}"
            );
        }

        plant("jobs:\n  miri:\n    steps: []\n");
        assert_eq!(
            screen(),
            vec!["ci.yml: the `dialect-conformance` job is absent".to_owned()]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
