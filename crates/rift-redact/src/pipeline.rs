//! The redaction pipeline: ordered, order-independent [`RedactStage`]s whose
//! findings are merged and masked.

use crate::stages::custom_pack::{CustomPackStage, RulePack};
use crate::stages::{entropy::EntropyStage, pii::PiiStage, secret_regex::SecretRegexStage};

/// A byte range `[start, end)` within the scanned text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    fn overlaps_or_adjacent(self, other: Span) -> bool {
        self.start <= other.end && other.start <= self.end
    }
}

/// One thing a stage flagged in the text.
#[derive(Debug, Clone)]
pub struct Finding {
    /// Stage that produced this finding (e.g. `"secret_regex"`).
    pub stage: &'static str,
    /// What kind of sensitive value it is (e.g. `"aws_access_key"`).
    pub kind: String,
    pub span: Span,
    pub confidence: f32,
}

/// Controls how flagged spans are rewritten. Length-preserving info is
/// deliberately discarded — every match becomes the same placeholder.
pub struct Masker {
    placeholder: String,
}

impl Default for Masker {
    fn default() -> Self {
        Self {
            placeholder: "[REDACTED]".to_string(),
        }
    }
}

impl Masker {
    #[must_use]
    pub fn new(placeholder: impl Into<String>) -> Self {
        Self {
            placeholder: placeholder.into(),
        }
    }
}

/// A single redaction pass over text. Stages must be pure and order-independent;
/// the pipeline is responsible for merging their findings.
pub trait RedactStage: Send + Sync {
    fn name(&self) -> &'static str;
    fn scan(&self, text: &str) -> Vec<Finding>;
}

/// An ordered collection of stages.
#[derive(Default)]
pub struct Pipeline {
    stages: Vec<Box<dyn RedactStage>>,
}

impl Pipeline {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The default pipeline: secret regexes, entropy, and PII.
    #[must_use]
    pub fn builtin() -> Self {
        Self::new()
            .with_stage(Box::new(SecretRegexStage::builtin()))
            .with_stage(Box::new(EntropyStage::default()))
            .with_stage(Box::new(PiiStage))
    }

    #[must_use]
    pub fn with_stage(mut self, stage: Box<dyn RedactStage>) -> Self {
        self.stages.push(stage);
        self
    }

    /// Add a user-supplied rule pack as an extra stage.
    #[must_use]
    pub fn with_pack(self, pack: RulePack) -> Self {
        self.with_stage(Box::new(CustomPackStage::new(pack)))
    }

    /// Run every stage and return all findings (unmerged), sorted by span start.
    #[must_use]
    pub fn scan(&self, text: &str) -> Vec<Finding> {
        let mut findings: Vec<Finding> = self.stages.iter().flat_map(|s| s.scan(text)).collect();
        findings.sort_by_key(|f| (f.span.start, f.span.end));
        findings
    }

    /// Produce redacted text plus the findings that drove the redaction.
    #[must_use]
    pub fn redact(&self, text: &str, masker: &Masker) -> (String, Vec<Finding>) {
        let findings = self.scan(text);
        let merged = merge_spans(findings.iter().map(|f| f.span));

        let mut out = String::with_capacity(text.len());
        let mut cursor = 0usize;
        for span in merged {
            // Copy the untouched gap, then the placeholder. Guard against any
            // out-of-range spans from a misbehaving stage.
            let start = span.start.min(text.len());
            let end = span.end.min(text.len());
            if start > cursor {
                out.push_str(&text[cursor..start]);
            }
            out.push_str(&masker.placeholder);
            cursor = end.max(cursor);
        }
        if cursor < text.len() {
            out.push_str(&text[cursor..]);
        }
        (out, findings)
    }
}

/// Merge overlapping or adjacent spans into a minimal disjoint set.
fn merge_spans(spans: impl IntoIterator<Item = Span>) -> Vec<Span> {
    let mut spans: Vec<Span> = spans.into_iter().collect();
    spans.sort_by_key(|s| (s.start, s.end));
    let mut merged: Vec<Span> = Vec::new();
    for span in spans {
        match merged.last_mut() {
            Some(last) if last.overlaps_or_adjacent(span) => {
                last.end = last.end.max(span.end);
            }
            _ => merged.push(span),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedStage(Vec<Span>);
    impl RedactStage for FixedStage {
        fn name(&self) -> &'static str {
            "fixed"
        }
        fn scan(&self, _text: &str) -> Vec<Finding> {
            self.0
                .iter()
                .map(|&span| Finding {
                    stage: "fixed",
                    kind: "test".to_string(),
                    span,
                    confidence: 1.0,
                })
                .collect()
        }
    }

    #[test]
    fn merges_overlapping_spans_before_masking() {
        let pipeline = Pipeline::new().with_stage(Box::new(FixedStage(vec![
            Span { start: 0, end: 5 },
            Span { start: 3, end: 8 },
        ])));
        let (out, findings) = pipeline.redact("0123456789", &Masker::default());
        assert_eq!(out, "[REDACTED]89");
        assert_eq!(findings.len(), 2);
    }

    #[test]
    fn leaves_clean_text_untouched() {
        let pipeline = Pipeline::new();
        let (out, findings) = pipeline.redact("nothing here", &Masker::default());
        assert_eq!(out, "nothing here");
        assert!(findings.is_empty());
    }
}
