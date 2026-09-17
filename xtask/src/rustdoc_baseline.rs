//! `rustdoc-baseline` — the shrink-only baseline of rustdoc warning sites
//! (the owner decided on 2026-08-30 that the file exists even while empty).
//!
//! Runs `cargo doc --workspace --no-deps --all-features --message-format=json`
//! with `RUSTDOCFLAGS` unset — so a warning stays a warning and can be
//! enumerated instead of aborting the first crate — and keys every warning as
//! `<repo-relative file>\t<message>`: no line number, because lines move and a
//! site is a claim about a file. Both arms, like the proptest baseline: an
//! unlisted site is a violation, and a listed site that no longer warns is a
//! violation ("delete the line"). The `docs` CI job runs this beside
//! `cargo doc -D warnings`; with an empty list the two agree exactly.

use std::path::Path;
use std::process::Command;

/// The tracked baseline.
pub const BASELINE: &str = "ci/rustdoc-baseline.txt";

/// Warning sites from `cargo doc --message-format=json` output, sorted and
/// deduplicated: `file\tmessage` for every `compiler-message` at warning
/// level that carries a primary span. Lines that are not JSON, not messages,
/// or not warnings are skipped.
#[must_use]
pub fn sites_from_json(json_lines: &str) -> Vec<String> {
    let mut out: Vec<String> = json_lines
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v.get("reason").and_then(serde_json::Value::as_str) == Some("compiler-message"))
        .filter_map(|v| {
            let m = v.get("message")?;
            if m.get("level").and_then(serde_json::Value::as_str) != Some("warning") {
                return None;
            }
            let text = m.get("message")?.as_str()?.to_owned();
            let file = m
                .get("spans")?
                .as_array()?
                .iter()
                .find(|s| s.get("is_primary").and_then(serde_json::Value::as_bool) == Some(true))
                .and_then(|s| s.get("file_name"))
                .and_then(serde_json::Value::as_str)?
                .replace('\\', "/");
            Some(format!("{file}\t{text}"))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Both arms over the tracked baseline.
#[must_use]
pub fn check(found: &[String], baseline: &[String]) -> Vec<String> {
    let mut v = Vec::new();
    for site in found {
        if !baseline.contains(site) {
            v.push(format!(
                "{site}: rustdoc warns here and {BASELINE} does not list it — fix the doc comment \
                 (or, if it cannot be fixed in this commit, list the site by name)"
            ));
        }
    }
    for site in baseline {
        if !found.contains(site) {
            v.push(format!(
                "{BASELINE} names `{site}`, which no longer warns — delete the line (this baseline \
                 only shrinks)"
            ));
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for site in baseline {
        if !seen.insert(site) {
            v.push(format!("{BASELINE} lists `{site}` twice"));
        }
    }
    v
}

/// Parse the baseline: trimmed, non-empty, non-comment lines.
fn parse_baseline(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// Run rustdoc over the workspace and hold the sites to the baseline.
///
/// # Errors
/// If the baseline cannot be read or `cargo doc` cannot be spawned — the gate
/// fails closed, because "could not check" is not "checked".
pub fn run(root: &Path) -> Result<Vec<String>, String> {
    let baseline_text = std::fs::read_to_string(root.join(BASELINE))
        .map_err(|e| format!("read {BASELINE}: {e}"))?;
    let baseline = parse_baseline(&baseline_text);
    let out = Command::new("cargo")
        .args([
            "doc",
            "--workspace",
            "--no-deps",
            "--all-features",
            "--message-format=json",
        ])
        .current_dir(root)
        .env_remove("RUSTDOCFLAGS")
        .output()
        .map_err(|e| format!("spawn cargo doc: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let found = sites_from_json(&stdout);
    let mut v = check(&found, &baseline);
    if !out.status.success() && found.is_empty() {
        v.push(format!(
            "cargo doc exited {} and reported no warning site — a build error, not a doc \
             finding: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .last()
                .unwrap_or_default()
        ));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WARN: &str = r#"{"reason":"compiler-message","message":{"level":"warning","message":"public documentation for `x` links to private item `y`","spans":[{"file_name":"crates/a/src/lib.rs","is_primary":true}]}}"#;
    const ERR: &str = r#"{"reason":"compiler-message","message":{"level":"error","message":"boom","spans":[{"file_name":"crates/a/src/lib.rs","is_primary":true}]}}"#;

    #[test]
    fn warning_sites_are_keyed_by_file_and_message_without_a_line() {
        let text = format!("{WARN}\nnot json\n{ERR}\n{{\"reason\":\"build-finished\"}}\n{WARN}\n");
        let sites = sites_from_json(&text);
        assert_eq!(
            sites,
            vec![
                "crates/a/src/lib.rs\tpublic documentation for `x` links to private item `y`"
                    .to_owned()
            ]
        );
    }

    #[test]
    fn both_arms_hold_and_a_duplicate_is_a_violation() {
        let site = "crates/a/src/lib.rs\tunresolved link".to_owned();
        assert!(check(&[], &[]).is_empty());
        let unlisted = check(std::slice::from_ref(&site), &[]);
        assert_eq!(unlisted.len(), 1);
        assert!(unlisted
            .first()
            .is_some_and(|m| m.contains("does not list it")));
        let stale = check(&[], std::slice::from_ref(&site));
        assert_eq!(stale.len(), 1);
        assert!(stale.first().is_some_and(|m| m.contains("only shrinks")));
        assert!(check(std::slice::from_ref(&site), std::slice::from_ref(&site)).is_empty());
        let dup = check(std::slice::from_ref(&site), &[site.clone(), site.clone()]);
        assert!(dup.iter().any(|m| m.contains("twice")));
    }

    #[test]
    fn the_tracked_baseline_parses_and_is_empty() {
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join(BASELINE),
        )
        .expect("the baseline is tracked");
        assert!(
            parse_baseline(&text).is_empty(),
            "the 2026-08-29 clean-up left zero sites"
        );
    }
}
