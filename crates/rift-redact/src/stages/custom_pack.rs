//! User-supplied rule packs loaded from TOML.

use std::path::Path;

use serde::Deserialize;

use crate::pipeline::{Finding, RedactStage};
use crate::stages::secret_regex::{compile_lenient, scan_rules, CompiledRule};

fn default_confidence() -> f32 {
    0.9
}

/// A single detection rule.
#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    pub kind: String,
    pub pattern: String,
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}

/// A collection of rules, deserialized from a `[[rules]]` TOML document.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RulePack {
    #[serde(default)]
    pub rules: Vec<Rule>,
}

/// Errors loading a rule pack from disk.
#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("io error reading rule pack: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid rule pack TOML: {0}")]
    Toml(#[from] toml::de::Error),
}

impl RulePack {
    /// Parse a rule pack from a TOML string.
    ///
    /// # Errors
    /// Returns a [`toml::de::Error`] if the document is malformed.
    pub fn from_toml_str(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Load a rule pack from a `.toml` file.
    ///
    /// # Errors
    /// Returns [`PackError`] if the file cannot be read or parsed.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, PackError> {
        let s = std::fs::read_to_string(path)?;
        Ok(Self::from_toml_str(&s)?)
    }
}

/// A stage backed by a user rule pack. Invalid patterns are skipped (logged),
/// not fatal, so one bad rule never disables the whole pack.
pub struct CustomPackStage {
    rules: Vec<CompiledRule>,
}

impl CustomPackStage {
    #[must_use]
    pub fn new(pack: RulePack) -> Self {
        Self {
            rules: compile_lenient(&pack),
        }
    }
}

impl RedactStage for CustomPackStage {
    fn name(&self) -> &'static str {
        "custom_pack"
    }
    fn scan(&self, text: &str) -> Vec<Finding> {
        scan_rules(&self.rules, text, "custom_pack")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_and_applies_a_pack() {
        let pack =
            RulePack::from_toml_str("[[rules]]\nkind = \"ticket\"\npattern = \"ACME-[0-9]+\"\n")
                .unwrap();
        let stage = CustomPackStage::new(pack);
        let findings = stage.scan("see ACME-1234 for details");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, "ticket");
    }
}
