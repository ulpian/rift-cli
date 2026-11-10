//! `rift doctor --report`: one paste-able findings bundle for an alpha user to
//! send back, with nothing in it that names their work.
//!
//! The alpha exists to gather findings, and Rift has no telemetry: the only
//! way a finding reaches its authors is a person pasting it. So the report is
//! assembled from what `rift doctor` and `rift recall status` already compute,
//! read-only, and then scrubbed. The scrub keeps every key (keys are written by
//! this program), every number, boolean and null, and only those strings that
//! are short, tag-shaped tokens — a mode, a state, a verb, a version. Anything
//! else (a path, a branch name with a slash, a message quoting a file, a
//! command line) becomes its length, so the report says that something was
//! there without saying what. Map keys are judged the same way, because some
//! maps are keyed by data. Nothing is sent: the person decides what to paste.

use std::path::Path;

use serde_json::{Map, Value};

use crate::cli::Outcome;

/// The longest string kept verbatim. Long enough for a mode, a state tag or a
/// version; too short for most paths, sentences or identifiers worth hiding.
const MAX_TAG_CHARS: usize = 40;

/// The report for the repository at `start`.
pub(crate) fn cmd_doctor_report(start: &Path) -> Outcome {
    let root = crate::projection::worktree_root(start);
    let doctor = super::cmd_doctor_with_mode(&root, false, true);
    let status = crate::recall::run(&crate::cli::RecallCmd::Status, &root).map_or_else(
        |error| serde_json::json!({ "unavailable": error_tag(&error) }),
        |outcome| outcome.json,
    );
    let report = serde_json::json!({
        "report": "rift-doctor-report-v1",
        "build": {
            "version": env!("CARGO_PKG_VERSION"),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        },
        "doctor_exit": doctor.code,
        "doctor": scrub(&doctor.json),
        "recall_status": scrub(&status),
        "hook_timings": scrub(&hook_timings(&root)),
    });
    let text = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into());
    let timings = report
        .get("hook_timings")
        .map_or_else(String::new, timing_lines);
    Outcome::ok(
        format!(
            "Rift findings report. It holds counts, states and versions only: every \
             path, name and message is replaced by its length. Nothing is sent \
             anywhere. Review it, then paste everything between the fences into your \
             report.\n{timings}```json\n{text}\n```"
        ),
        report,
    )
}

/// One line per hook verb for the person reading the report before pasting.
fn timing_lines(summary: &Value) -> String {
    let Some(verbs) = summary.get("verbs").and_then(Value::as_object) else {
        return String::new();
    };
    let mut out = String::new();
    for (verb, s) in verbs {
        let num = |key: &str| s.get(key).and_then(Value::as_f64).unwrap_or(0.0);
        out.push_str(&format!(
            "hook {verb}: {} runs, p50 {} ms, p95 {} ms, max {} ms, {:.1}% over 100 ms\n",
            s.get("runs").and_then(Value::as_u64).unwrap_or(0),
            num("p50_ms"),
            num("p95_ms"),
            num("max_ms"),
            num("over_100ms_share") * 100.0,
        ));
    }
    out
}

/// p50/p95/max per hook verb and the share over 100 ms, from the family's
/// timing ring; an empty summary for a repository with no family here.
fn hook_timings(root: &Path) -> Value {
    let runs = crate::family::family_dir_here(root)
        .map(|dir| crate::hooktime::ring::read_runs(&dir))
        .unwrap_or_default();
    crate::hooktime::ring::summary(&runs)
}

/// The kind of an error, never its message: messages can quote paths.
fn error_tag(error: &rift_core::RiftError) -> String {
    let debug = format!("{error:?}");
    debug
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .next()
        .unwrap_or("error")
        .to_owned()
}

/// `value` with every string that is not a short tag replaced by its length.
fn scrub(value: &Value) -> Value {
    match value {
        Value::String(text) if is_tag(text) => Value::String(text.clone()),
        Value::String(text) => serde_json::json!({ "withheld_chars": text.chars().count() }),
        Value::Array(items) => Value::Array(items.iter().map(scrub).collect()),
        // Keys are written by this program, except where a map is keyed by
        // data (a session id, a file name): those are judged like any string
        // and numbered in order, so the shape survives without the name.
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .enumerate()
                .map(|(index, (key, value))| {
                    let key = if is_tag(key) {
                        key.clone()
                    } else {
                        format!("withheld_key_{index}")
                    };
                    (key, scrub(value))
                })
                .collect::<Map<_, _>>(),
        ),
        other => other.clone(),
    }
}

/// A short token of lowercase letters, digits and `_ : + -`: the shape of a
/// mode, a state or a key this program wrote. A `.` is allowed only in a
/// version (`1.0.0-alpha.2`), so `secrets.env` is withheld; more than six
/// digits is an identifier (a session id, a hash), not a tag.
fn is_tag(text: &str) -> bool {
    let version = text.starts_with(|c: char| c.is_ascii_digit());
    text.chars().count() <= MAX_TAG_CHARS
        && text.chars().filter(char::is_ascii_digit).count() <= 6
        && text.chars().all(|c| {
            c.is_ascii_lowercase()
                || c.is_ascii_digit()
                || matches!(c, '_' | ':' | '+' | '-')
                || (c == '.' && version)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_names_and_messages_are_reduced_to_their_length() {
        let raw = serde_json::json!({
            "mode": "enforce",
            "version": "1.0.0-alpha.2",
            "path": "/Users/someone/clients/acme/src/lib.rs",
            "windows_path": "C:\\Users\\someone\\repo",
            "message": "config.toml could not be read",
            "branch": "Feature-Secret",
            "count": 7,
            "ok": true,
            "list": ["observe", "src/auth.rs"],
            "nested": { "detail": "ANTHROPIC_API_KEY=sk-ant-abc" },
        });
        let scrubbed = scrub(&raw);
        assert_eq!(scrubbed["mode"], "enforce");
        assert_eq!(scrubbed["version"], "1.0.0-alpha.2");
        assert_eq!(scrubbed["count"], 7);
        assert_eq!(scrubbed["ok"], true);
        assert_eq!(scrubbed["list"][0], "observe");
        let text = scrubbed.to_string();
        for leaked in [
            "someone",
            "acme",
            "lib.rs",
            "could not",
            "Secret",
            "auth.rs",
            "sk-ant",
            "API_KEY",
        ] {
            assert!(
                !text.contains(leaked),
                "{leaked} reached the report: {text}"
            );
        }
        assert_eq!(scrubbed["path"]["withheld_chars"], 38);
    }

    #[test]
    fn a_long_lowercase_token_is_withheld_too() {
        let long = "a".repeat(MAX_TAG_CHARS + 1);
        assert!(!is_tag(&long));
        assert!(is_tag(&"a".repeat(MAX_TAG_CHARS)));
        assert!(!is_tag("has space"));
        assert!(!is_tag("secrets.env"));
        assert!(!is_tag("3f0dd9e2-1b74-45b9-a107-3be3e262de54"));
        assert!(is_tag("1.0.0-alpha.2"));
    }

    #[test]
    fn a_map_keyed_by_data_keeps_its_shape_and_loses_its_names() {
        let raw = serde_json::json!({
            "sessions": { "3f0dd9e2-1b74-45b9-a107-3be3e262de54": 4, "pending": 1 },
            "files": { "secrets.env": "dirty" },
        });
        let scrubbed = scrub(&raw);
        let text = scrubbed.to_string();
        assert!(
            !text.contains("3f0dd9e2") && !text.contains("secrets"),
            "{text}"
        );
        assert_eq!(scrubbed["sessions"]["pending"], 1);
        assert_eq!(scrubbed["sessions"]["withheld_key_0"], 4);
    }
}
