//! Wire the repository's loaded redactor packs into the capture path.
//!
//! Two crates already do the halves of this and neither can do the whole:
//! `rift-config` reads `.rift/policies/redactors/**` into data and compiles
//! nothing, `rift-shadow-redact` compiles rules and reads no files. This module
//! is the only place that depends on both, which is what keeps the loader out of
//! the redactor and the regex engine out of the loader.
//!
//! **Nothing here may fail the hook.** A missing directory is "no packs
//! configured"; a malformed file, a bad regex, a colliding id or a failing
//! self-test costs that one rule or that one file and nothing else. The
//! built-in detectors are always present, so the worst case for a broken pack is
//! the coverage the repository had before it was written — never less.

use std::path::Path;

use rift_shadow::redact::{PackRule, PackSample, PackSpec, RulePackSet};

/// The rule pack set the capture path should redact with for `work_tree`:
/// the built-ins, plus every rule the repository's packs contribute.
///
/// Diagnostics and warnings are returned rather than printed so the caller
/// decides where they belong — the hook path has a developer's terminal to
/// respect, and the detached worker has no terminal at all.
pub(crate) fn capture_rule_packs(work_tree: &Path) -> (RulePackSet, Vec<String>) {
    let dir = rift_config::redactor_pack_dir(&work_tree.join(".rift"));
    let loaded = rift_config::load_redactor_packs(&dir);
    let mut notes: Vec<String> = loaded
        .diagnostics
        .iter()
        .map(|d| format!("redactor pack {}: {}", d.path.display(), d.reason))
        .collect();

    let mut set = RulePackSet::builtin();
    for pack in &loaded.packs {
        let spec = PackSpec {
            name: pack.name.clone(),
            version: pack.version.clone(),
            rules: pack
                .rules
                .iter()
                .map(|r| PackRule {
                    id: r.id.clone(),
                    regex: r.regex.clone(),
                    samples: r
                        .samples
                        .iter()
                        .map(|s| PackSample {
                            input: s.input.clone(),
                            redacted: s.redacted,
                        })
                        .collect(),
                })
                .collect(),
        };
        let (next, warnings) = set.with_pack(&spec);
        set = next;
        notes.extend(warnings.iter().map(render_warning));
    }
    (set, notes)
}

/// One line per warning, naming the rule and what happened to it.
///
/// Written here rather than as a `Display` on the warning because the type is
/// the redactor's and this wording is the CLI's. Two properties are load-bearing
/// and both are the redactor's design, preserved rather than re-decided: a
/// warning carries a sample's **length and never its text**, and a mismatched
/// sample **keeps** its rule — so the note says "kept", or a reader would assume
/// coverage had been dropped and go looking for a hole that is not there.
fn render_warning(w: &rift_shadow::redact::PackWarning) -> String {
    use rift_shadow::redact::PackWarning as W;
    match w {
        W::RegexRejected { rule, reason } => {
            format!("redactor rule {rule} dropped: pattern did not compile ({reason})")
        }
        W::IdCollidesWithBuiltin { rule } => {
            format!("redactor rule {rule} dropped: id collides with a built-in detector")
        }
        W::MatchesEmptyString { rule } => {
            format!("redactor rule {rule} dropped: pattern matches the empty string")
        }
        W::SampleMismatch {
            rule,
            index,
            input_len,
            expected,
            observed,
        } => format!(
            "redactor rule {rule} KEPT, but its sample {index} ({input_len} bytes) \
             expected redacted={expected} and observed={observed}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_core::SecretBytes;

    fn write_pack(work_tree: &Path, name: &str, body: &str) {
        let dir = rift_config::redactor_pack_dir(&work_tree.join(".rift"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.toml")), body).unwrap();
    }

    #[test]
    fn a_repository_with_no_packs_gets_the_builtins_and_no_notes() {
        // The overwhelmingly common case, and the one the hook path pays for on
        // every commit: it must cost nothing and say nothing.
        let tmp = tempfile::tempdir().unwrap();
        let (set, notes) = capture_rule_packs(tmp.path());
        assert!(notes.is_empty(), "{notes:?}");
        let out = rift_shadow::redact::redact_with(
            &SecretBytes::new(b"key AKIAIOSFODNN7EXAMPLE here".to_vec()),
            &set,
        )
        .unwrap();
        assert!(
            !String::from_utf8_lossy(out.redacted.as_slice()).contains("AKIAIOSFODNN7EXAMPLE"),
            "the built-ins are still in the set"
        );
    }

    #[test]
    fn a_loaded_rule_masks_bytes_the_builtins_leave_alone() {
        // The point of the wiring, asserted end to end: a value no built-in
        // detector knows about is masked ONLY because the repository's file was
        // read, compiled and folded into the set the capture path uses.
        let tmp = tempfile::tempdir().unwrap();
        write_pack(
            tmp.path(),
            "acme",
            r#"schema_version = 1
name = "acme"
version = "2026.08.0"
description = "org-internal tokens"
[[rules]]
id = "badge"
description = "employee badge"
regex = "ACME-[0-9]{6}"
"#,
        );
        let raw = SecretBytes::new(b"badge ACME-123456 on file\n".to_vec());

        let control = rift_shadow::redact::redact(&raw).unwrap();
        assert!(
            String::from_utf8_lossy(control.redacted.as_slice()).contains("ACME-123456"),
            "control: no built-in detector knows this shape"
        );

        let (set, notes) = capture_rule_packs(tmp.path());
        assert!(notes.is_empty(), "{notes:?}");
        let out = rift_shadow::redact::redact_with(&raw, &set).unwrap();
        let text = String::from_utf8_lossy(out.redacted.as_slice()).into_owned();
        assert!(
            !text.contains("ACME-123456"),
            "masked by the loaded rule: {text}"
        );
        assert!(
            out.report
                .rule_pack_versions
                .iter()
                .any(|v| v == "acme@2026.08.0"),
            "the pack version reaches the report: {:?}",
            out.report.rule_pack_versions
        );
    }

    #[test]
    fn a_broken_pack_costs_its_own_rule_and_never_the_builtins() {
        // The failure posture the hook path depends on: a pack that cannot be
        // used must degrade to the coverage the repository already had, with a
        // note, and never to less.
        let tmp = tempfile::tempdir().unwrap();
        write_pack(
            tmp.path(),
            "broken",
            r#"schema_version = 1
name = "broken"
version = "1"
[[rules]]
id = "unclosed"
regex = "ACME-[0-9"
"#,
        );
        let (set, notes) = capture_rule_packs(tmp.path());
        assert!(
            notes.iter().any(|n| n.contains("unclosed")),
            "the reason is reported: {notes:?}"
        );
        let out = rift_shadow::redact::redact_with(
            &SecretBytes::new(b"key AKIAIOSFODNN7EXAMPLE here".to_vec()),
            &set,
        )
        .unwrap();
        assert!(
            !String::from_utf8_lossy(out.redacted.as_slice()).contains("AKIAIOSFODNN7EXAMPLE"),
            "the built-ins survive a broken pack"
        );
    }
}
