//! Regex-based secret detection.
//!
//! This module owns the compiled-rule machinery shared by the built-in stage
//! and user rule packs ([`super::custom_pack`]).

use regex::Regex;

use crate::pipeline::{Finding, RedactStage, Span};
use crate::stages::custom_pack::RulePack;

/// The embedded default rule pack.
const DEFAULT_PACK: &str = include_str!("../packs/default.toml");

/// A rule whose pattern has been compiled.
pub struct CompiledRule {
    pub kind: String,
    pub re: Regex,
    pub confidence: f32,
}

/// Compile every rule, failing on the first invalid pattern.
///
/// # Errors
/// Returns the [`regex::Error`] of the first pattern that fails to compile.
pub fn compile(pack: &RulePack) -> Result<Vec<CompiledRule>, regex::Error> {
    pack.rules
        .iter()
        .map(|r| {
            Ok(CompiledRule {
                kind: r.kind.clone(),
                re: Regex::new(&r.pattern)?,
                confidence: r.confidence,
            })
        })
        .collect()
}

/// Compile rules, skipping (and logging) any that fail to compile.
#[must_use]
pub fn compile_lenient(pack: &RulePack) -> Vec<CompiledRule> {
    pack.rules
        .iter()
        .filter_map(|r| match Regex::new(&r.pattern) {
            Ok(re) => Some(CompiledRule {
                kind: r.kind.clone(),
                re,
                confidence: r.confidence,
            }),
            Err(error) => {
                tracing::warn!(kind = %r.kind, %error, "skipping invalid regex in rule pack");
                None
            }
        })
        .collect()
}

/// Run a compiled rule set over `text`, attributing findings to `stage`.
pub(crate) fn scan_rules(rules: &[CompiledRule], text: &str, stage: &'static str) -> Vec<Finding> {
    let mut findings = Vec::new();
    for rule in rules {
        for m in rule.re.find_iter(text) {
            findings.push(Finding {
                stage,
                kind: rule.kind.clone(),
                span: Span {
                    start: m.start(),
                    end: m.end(),
                },
                confidence: rule.confidence,
            });
        }
    }
    findings
}

/// The always-on secret-regex stage.
pub struct SecretRegexStage {
    rules: Vec<CompiledRule>,
}

impl SecretRegexStage {
    /// Build the stage from the embedded default rule pack.
    ///
    /// # Panics
    /// Panics if the embedded `default.toml` is malformed — that is a build-time
    /// bug, caught by this module's tests.
    #[must_use]
    pub fn builtin() -> Self {
        let pack = RulePack::from_toml_str(DEFAULT_PACK).expect("embedded default.toml parses");
        let rules = compile(&pack).expect("embedded default.toml patterns compile");
        Self { rules }
    }
}

impl RedactStage for SecretRegexStage {
    fn name(&self) -> &'static str {
        "secret_regex"
    }
    fn scan(&self, text: &str) -> Vec<Finding> {
        scan_rules(&self.rules, text, "secret_regex")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_pack_compiles() {
        // Must not panic.
        let _ = SecretRegexStage::builtin();
    }

    #[test]
    fn flags_an_aws_key() {
        let stage = SecretRegexStage::builtin();
        let findings = stage.scan("export KEY=AKIAIOSFODNN7EXAMPLE rest");
        assert!(findings.iter().any(|f| f.kind == "aws_access_key_id"));
    }
}
