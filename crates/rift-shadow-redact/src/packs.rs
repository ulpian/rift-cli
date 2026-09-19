//! Rule packs as they arrive from disk, and how a loaded pack becomes
//! detectors. The on-disk format and its loader live in `rift-config`; this
//! module owns the plain shape between the two crates and the compile step,
//! because compiling a pattern and running its self-tests against the real
//! pipeline is redaction semantics and belongs beside the pipeline.

use std::borrow::Cow;

use regex::Regex;

use crate::builtin_tables::builtin_shape_tables;
use crate::entropy::ENTROPY_DETECTOR_ID;
use crate::span::ShapeDetector;
use crate::{DetectorClass, RulePackSet};

/// A rule pack as it arrives from disk, ready to be compiled into detectors.
///
/// The on-disk format and its loader live in `rift-config`; this is the plain
/// owned shape the redaction crate compiles, which is what keeps the two crates
/// independent of one another.
#[derive(Debug, Clone)]
pub struct PackSpec {
    /// Pack name. Namespaces every rule id in the report.
    pub name: String,
    /// Pack version, recorded in [`crate::RedactionReport::rule_pack_versions`] as
    /// `<name>@<version>`.
    pub version: String,
    /// The rules, in file order.
    pub rules: Vec<PackRule>,
}

/// One pack rule: an id, a pattern, and the samples that prove the pattern does
/// what its author believes.
#[derive(Debug, Clone)]
pub struct PackRule {
    /// Unique within the pack.
    pub id: String,
    /// The pattern, compiled here — never by the loader.
    pub regex: String,
    /// Self-tests. Never fatal.
    pub samples: Vec<PackSample>,
}

/// A rule's self-test: is this input expected to match?
#[derive(Debug, Clone)]
pub struct PackSample {
    /// The text the rule runs against.
    pub input: String,
    /// Whether the rule is expected to match it.
    pub redacted: bool,
}

/// Something a pack asked for that was not granted.
///
/// Never fatal, and never carrying sample text: a sample is written to look
/// exactly like a secret, and a warning is a log line, so a mismatch reports the
/// sample's *length* and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackWarning {
    /// The pattern did not compile. That rule is dropped; the rest of the pack
    /// still loads.
    RegexRejected {
        /// The namespaced rule id.
        rule: String,
        /// The regex engine's reason.
        reason: String,
    },
    /// The namespaced id collides with a built-in detector id, which would make
    /// the report's per-detector counts ambiguous. Dropped; renaming it loads.
    IdCollidesWithBuiltin {
        /// The namespaced rule id.
        rule: String,
    },
    /// The pattern matches the empty string, so it matches at *every* position
    /// in every input. Dropped: a rule that names nothing cannot seal anything,
    /// and honouring its matches would fill the text with placeholders and grow
    /// it again on the next pass. `x*` is the whole class; `x+` is the fix.
    MatchesEmptyString {
        /// The namespaced rule id.
        rule: String,
    },
    /// The rule and its own sample disagree. The rule is **kept**.
    SampleMismatch {
        /// The namespaced rule id.
        rule: String,
        /// Which sample, by position.
        index: usize,
        /// The sample's length — never its text.
        input_len: usize,
        /// What the pack claimed.
        expected: bool,
        /// What the compiled rule did.
        observed: bool,
    },
}

impl RulePackSet {
    /// Compile a **loaded** rule pack into this set.
    ///
    /// The on-disk format, its caps and its diagnostics live in `rift-config`
    /// (`redactor_packs`), which owns policy-as-code; this crate owns redaction
    /// semantics and therefore compiles the patterns and runs the self-tests
    /// against the real pipeline. Neither crate depends on the other — a
    /// [`PackSpec`] is the plain owned shape between them.
    ///
    /// Packs are **additive**: each rule is one more detector, and nothing a
    /// pack can contain disables a built-in one — the built-in pipeline is not
    /// addressable from a pack at all. That asymmetry (packs only add; only the
    /// allowlist subtracts, and it counts what it keeps) is the whole safety
    /// story for user-supplied redaction policy.
    ///
    /// Nothing here is fatal, and that is deliberate: this runs on the capture
    /// path, where a policy file someone is editing must never be able to fail a
    /// commit. A rule whose pattern will not compile is dropped **on its own**
    /// with a [`PackWarning`]; a rule that disagrees with its own sample is
    /// **kept**, because a wrong self-test is not a reason to stop redacting.
    #[must_use]
    pub fn with_pack(mut self, spec: &PackSpec) -> (Self, Vec<PackWarning>) {
        let mut warnings = Vec::new();
        for rule in &spec.rules {
            let id = format!("{}.{}", spec.name, rule.id);
            let re = match compile_rule(&id, rule) {
                Ok(re) => re,
                Err(warning) => {
                    warnings.push(warning);
                    continue;
                }
            };
            warnings.extend(sample_mismatches(&id, &re, &rule.samples));
            self.extra.push(ShapeDetector {
                id: Cow::Owned(id),
                class: DetectorClass::VendorSecret,
                re,
                group: 0,
                gate: None,
            });
        }
        let version = format!("{}@{}", spec.name, spec.version);
        if !self.versions.iter().any(|v| v == &version) {
            self.versions.push(version);
        }
        (self, warnings)
    }
}

/// Whether `id` is already the id of a built-in detector. A pack rule may not
/// take one: `report.by_detector` has to stay attributable to exactly one
/// detector, or a count means nothing.
pub(crate) fn is_builtin_detector_id(id: &str) -> bool {
    id == ENTROPY_DETECTOR_ID
        || builtin_shape_tables()
            .iter()
            .flat_map(|table| table.iter())
            .any(|d| d.id == id)
}

/// A rule's pattern compiled, or the one warning that drops the rule: an id
/// a built-in already answers to, a pattern that will not compile, or one
/// that matches the empty string.
fn compile_rule(id: &str, rule: &PackRule) -> Result<Regex, PackWarning> {
    if is_builtin_detector_id(id) {
        return Err(PackWarning::IdCollidesWithBuiltin {
            rule: id.to_owned(),
        });
    }
    let re = Regex::new(&rule.regex).map_err(|e| PackWarning::RegexRejected {
        rule: id.to_owned(),
        reason: e.to_string(),
    })?;
    if re.is_match("") {
        return Err(PackWarning::MatchesEmptyString {
            rule: id.to_owned(),
        });
    }
    Ok(re)
}

/// The self-tests the compiled rule disagrees with. Never fatal, and never
/// carrying the sample's text.
fn sample_mismatches(id: &str, re: &Regex, samples: &[PackSample]) -> Vec<PackWarning> {
    let mut warnings = Vec::new();
    for (index, sample) in samples.iter().enumerate() {
        let observed = re.is_match(&sample.input);
        if observed != sample.redacted {
            warnings.push(PackWarning::SampleMismatch {
                rule: id.to_owned(),
                index,
                input_len: sample.input.len(),
                expected: sample.redacted,
                observed,
            });
        }
    }
    warnings
}
