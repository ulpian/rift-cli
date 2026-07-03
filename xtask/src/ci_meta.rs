//! CI/lint meta-checks (build-plan stage-01 phase-02 steps 01 + 03): guards on
//! the guards. Each check returns the list of violations (empty ⇒ pass); the
//! `ci-meta` command fails if any check reports one.
//!
//! * `required_jobs_present` — the workflow still declares every gate job.
//! * `workflow_permissions_read_only` — `permissions: contents: read`, no writes.
//! * `lint_table_matches_baseline` — `[workspace.lints.*]` vs
//!   `ci/lint-baseline.txt`, so a silently dropped/downgraded lint (the exact
//!   drift step-03 anticipated for `indexing_slicing`) fails CI.
//! * `unsafe_only_in_sanctioned_modules` — no `unsafe` outside an allowlist
//!   (empty today; the workspace is `#![forbid(unsafe_code)]`).
//! * `no_blanket_allow_at_crate_root` — crate roots carry no `#![allow(...)]`
//!   of workspace-managed lints; narrow, justified allows live at the site.
//! * `no_schema_fastpath_in_bins` — production code opens projections through
//!   the checksum-locked migration engine, never `create_canonical_schema`.

use std::path::Path;

/// The jobs the CI workflow must always declare.
const REQUIRED_JOBS: &[&str] = &[
    "fmt:",
    "clippy:",
    "test:",
    "msrv:",
    "coverage:",
    "security:",
    "miri:",
    "fuzz-smoke:",
    "ci-meta:",
];

fn read(root: &Path, rel: &str) -> Result<String, String> {
    std::fs::read_to_string(root.join(rel)).map_err(|e| format!("read {rel}: {e}"))
}

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

/// Normalize a lint-table body: keep `[section]` headers and `key = value`
/// lines, dropping comments and blanks.
fn normalized_lint_lines(text: &str, sections: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut keep = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            keep = sections.iter().any(|s| line == format!("[{s}]"));
            if keep {
                out.push(line.to_string());
            }
            continue;
        }
        if keep && !line.is_empty() && !line.starts_with('#') {
            out.push(line.to_string());
        }
    }
    out
}

/// `[workspace.lints.rust]` + `[workspace.lints.clippy]` in the root manifest
/// match `ci/lint-baseline.txt` exactly (order-sensitive: the table is policy).
///
/// # Errors
/// If either file cannot be read.
pub fn lint_table_matches_baseline(root: &Path) -> Result<Vec<String>, String> {
    let sections = ["workspace.lints.rust", "workspace.lints.clippy"];
    let manifest = normalized_lint_lines(&read(root, "Cargo.toml")?, &sections);
    let baseline = normalized_lint_lines(
        &read(root, "ci/lint-baseline.txt")?,
        &["workspace.lints.rust", "workspace.lints.clippy"],
    );
    if manifest == baseline {
        return Ok(Vec::new());
    }
    let mut v = vec![
        "workspace lint table diverges from ci/lint-baseline.txt (change both, with rationale):"
            .to_string(),
    ];
    for line in &manifest {
        if !baseline.contains(line) {
            v.push(format!("  manifest has: {line}"));
        }
    }
    for line in &baseline {
        if !manifest.contains(line) {
            v.push(format!("  baseline has: {line}"));
        }
    }
    Ok(v)
}

/// Walk all first-party `.rs` files under `crates/`, `bins/`, `xtask/`.
fn source_files(root: &Path) -> Vec<std::path::PathBuf> {
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

/// No `unsafe` code outside the (currently empty) sanctioned-module allowlist.
///
/// # Errors
/// If a source file cannot be read.
pub fn unsafe_only_in_sanctioned_modules(root: &Path) -> Result<Vec<String>, String> {
    const SANCTIONED: &[&str] = &[]; // extend with FFI seams as they land
    let mut v = Vec::new();
    for f in source_files(root) {
        let rel = f.strip_prefix(root).unwrap_or(&f).display().to_string();
        if SANCTIONED.iter().any(|s| rel.starts_with(s)) {
            continue;
        }
        let text = std::fs::read_to_string(&f).map_err(|e| format!("read {rel}: {e}"))?;
        // needles assembled via concat! so this scanner never matches itself
        const NEEDLES: [&str; 3] = [
            concat!("unsafe", " fn"),
            concat!("unsafe", " impl"),
            concat!("unsafe", " {"),
        ];
        for (i, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if NEEDLES.iter().any(|n| code.contains(n)) {
                v.push(format!(
                    "{rel}:{}: `unsafe` outside a sanctioned module",
                    i + 1
                ));
            }
        }
    }
    Ok(v)
}

/// Crate roots carry no blanket `#![allow(clippy::...)]` of workspace-managed
/// lints — the workspace table is the single policy point; site allows must be
/// narrow and justified. (`#![cfg_attr(test, allow(...))]` is fine: test-only.)
///
/// # Errors
/// If a crate-root file cannot be read.
pub fn no_blanket_allow_at_crate_root(root: &Path) -> Result<Vec<String>, String> {
    let mut v = Vec::new();
    for f in source_files(root) {
        let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let is_crate_root = (name == "lib.rs" || name == "main.rs")
            && f.parent().is_some_and(|p| p.ends_with("src"));
        if !is_crate_root {
            continue;
        }
        let rel = f.strip_prefix(root).unwrap_or(&f).display().to_string();
        let text = std::fs::read_to_string(&f).map_err(|e| format!("read {rel}: {e}"))?;
        for (i, line) in text.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("#![allow(clippy::") {
                v.push(format!(
                    "{rel}:{}: blanket crate-root `#![allow(clippy::…)]` — manage the lint \
                     in [workspace.lints] or allow narrowly at the site",
                    i + 1
                ));
            }
        }
    }
    Ok(v)
}

/// `bins/` production code never uses the `create_canonical_schema` fast-path
/// (it bypasses the checksum-locked migration ledger). Occurrences after a
/// file's `#[cfg(test)]` marker are test code (test modules sit at file
/// bottom, per the workspace convention) and are exempt.
///
/// # Errors
/// If a source file cannot be read.
pub fn no_schema_fastpath_in_bins(root: &Path) -> Result<Vec<String>, String> {
    let mut v = Vec::new();
    for f in source_files(root) {
        let rel = f.strip_prefix(root).unwrap_or(&f).display().to_string();
        if !rel.starts_with("bins/") || rel.contains("/tests/") {
            continue;
        }
        let text = std::fs::read_to_string(&f).map_err(|e| format!("read {rel}: {e}"))?;
        let production = text.split("#[cfg(test)]").next().unwrap_or("");
        if production.contains("create_canonical_schema") {
            v.push(format!(
                "{rel}: `create_canonical_schema` in production bin code — open the \
                 projection via `rift_store::open_migrated`/`open_capture_projection`"
            ));
        }
    }
    Ok(v)
}

/// Run every meta-check, returning all violations.
///
/// # Errors
/// If any input file cannot be read.
pub fn run_all(root: &Path) -> Result<Vec<String>, String> {
    let mut v = Vec::new();
    v.extend(required_jobs_present(root)?);
    v.extend(workflow_permissions_read_only(root)?);
    v.extend(lint_table_matches_baseline(root)?);
    v.extend(unsafe_only_in_sanctioned_modules(root)?);
    v.extend(no_blanket_allow_at_crate_root(root)?);
    v.extend(no_schema_fastpath_in_bins(root)?);
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_root;

    /// The meta-checks pass on the real repository — this is the actual gate,
    /// run both here (cargo test -p xtask) and by the `ci-meta` CI job.
    #[test]
    fn repository_passes_all_meta_checks() {
        let violations = run_all(&workspace_root()).expect("meta checks run");
        assert!(
            violations.is_empty(),
            "ci-meta violations:\n{}",
            violations.join("\n")
        );
    }

    #[test]
    fn lint_baseline_drift_is_detected() {
        // A manifest whose clippy table dropped indexing_slicing ≠ the baseline.
        let manifest = "[workspace.lints.clippy]\nunwrap_used = \"warn\"\n";
        let baseline =
            "[workspace.lints.clippy]\nunwrap_used = \"warn\"\nindexing_slicing = \"warn\"\n";
        let m = normalized_lint_lines(manifest, &["workspace.lints.clippy"]);
        let b = normalized_lint_lines(baseline, &["workspace.lints.clippy"]);
        assert_ne!(m, b, "drift must be visible after normalization");
    }
}
