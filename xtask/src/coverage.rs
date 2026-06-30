//! Per-crate coverage gate over a `cargo llvm-cov --json` report.
//!
//! The workspace floor (≥85% line) is enforced directly by `cargo llvm-cov
//! --fail-under-lines 85` in CI; this module enforces the higher **≥95%** bar
//! for the security-critical crates listed in `ci/security-crates.txt` (testing
//! strategy 06 §6.3, step-05). It is a tiny Rust check rather than a `jq` script
//! so the gate path carries no external tool, and it **fails closed**: a listed
//! crate absent from the report is a violation, never a silent pass.

use std::path::Path;

use serde_json::Value;

/// The default per-crate line-coverage threshold for the security set.
pub const SECURITY_THRESHOLD: f64 = 95.0;

/// Read the security-crate registry (`ci/security-crates.txt`): one crate per
/// line, `#` comments and blanks ignored.
///
/// # Errors
/// If the registry file cannot be read.
pub fn read_security_crates(root: &Path) -> Result<Vec<String>, String> {
    let path = root.join("ci").join("security-crates.txt");
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect())
}

/// Aggregate line coverage (covered, total) for `crate_name` across every source
/// file in a `cargo llvm-cov --json` report. `None` ⇒ the crate contributed no
/// files (absent from the report).
fn crate_line_coverage(report: &Value, crate_name: &str) -> Option<(u64, u64)> {
    // llvm export schema: { "data": [ { "files": [ { "filename", "summary":
    // { "lines": { "count", "covered" } } } ] } ] }
    let marker = format!("/{crate_name}/src/");
    let files = report
        .get("data")
        .and_then(Value::as_array)
        .and_then(|d| d.first())
        .and_then(|d0| d0.get("files"))
        .and_then(Value::as_array)?;

    let mut covered = 0_u64;
    let mut total = 0_u64;
    let mut matched = false;
    for file in files {
        let name = file.get("filename").and_then(Value::as_str).unwrap_or("");
        if !name.contains(&marker) {
            continue;
        }
        matched = true;
        let lines = file.get("summary").and_then(|s| s.get("lines"));
        covered += lines
            .and_then(|l| l.get("covered"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        total += lines
            .and_then(|l| l.get("count"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
    }
    matched.then_some((covered, total))
}

/// Check each `crates` entry's line coverage against `min_percent` in a
/// `cargo llvm-cov --json` report. Returns the list of violations (empty ⇒ pass).
/// A crate absent from the report, or with zero measurable lines, is a violation
/// (fail-closed).
///
/// # Errors
/// If `json` is not valid JSON.
pub fn check_crate_coverage(
    json: &str,
    crates: &[String],
    min_percent: f64,
) -> Result<Vec<String>, String> {
    let report: Value =
        serde_json::from_str(json).map_err(|e| format!("parse coverage json: {e}"))?;
    let mut violations = Vec::new();
    for c in crates {
        match crate_line_coverage(&report, c) {
            None => violations.push(format!("{c}: absent from coverage report (fail-closed)")),
            Some((_, 0)) => violations.push(format!("{c}: zero measurable lines (fail-closed)")),
            Some((covered, total)) => {
                let pct = (covered as f64 / total as f64) * 100.0;
                if pct < min_percent {
                    violations.push(format!(
                        "{c}: line coverage {pct:.2}% < {min_percent:.0}% ({covered}/{total})"
                    ));
                }
            }
        }
    }
    Ok(violations)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal llvm-cov export with two files for one crate.
    fn report(covered: u64, count: u64) -> String {
        format!(
            r#"{{"data":[{{"files":[
              {{"filename":"/repo/crates/rift-crypto/src/lib.rs",
                "summary":{{"lines":{{"count":{count},"covered":{covered}}}}}}},
              {{"filename":"/repo/crates/rift-core/src/lib.rs",
                "summary":{{"lines":{{"count":10,"covered":10}}}}}}
            ]}}]}}"#
        )
    }

    fn crates() -> Vec<String> {
        vec!["rift-crypto".to_string()]
    }

    #[test]
    fn passes_when_at_or_above_threshold() {
        let v = check_crate_coverage(&report(96, 100), &crates(), 95.0).unwrap();
        assert!(v.is_empty(), "expected pass, got {v:?}");
    }

    #[test]
    fn fails_when_below_threshold() {
        let v = check_crate_coverage(&report(94, 100), &crates(), 95.0).unwrap();
        assert_eq!(v.len(), 1);
        assert!(v[0].contains("rift-crypto"));
    }

    #[test]
    fn missing_crate_fails_closed() {
        // Report has no rift-shadow-redact files → absent → violation.
        let v = check_crate_coverage(&report(100, 100), &["rift-shadow-redact".to_string()], 95.0)
            .unwrap();
        assert_eq!(v.len(), 1);
        assert!(v[0].contains("absent"));
    }

    #[test]
    fn aggregates_lines_across_a_crates_files() {
        // 96/100 over the matched file → 96% ≥ 95%.
        assert!(check_crate_coverage(&report(96, 100), &crates(), 95.0)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn invalid_json_is_an_error() {
        assert!(check_crate_coverage("{ not json", &crates(), 95.0).is_err());
    }
}
