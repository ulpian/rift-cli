//! PII detection. A reference email pass today; phone/address are TODO.

use std::sync::OnceLock;

use regex::Regex;

use crate::pipeline::{Finding, RedactStage, Span};

#[derive(Default)]
pub struct PiiStage;

fn email_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").expect("email regex is valid")
    })
}

impl RedactStage for PiiStage {
    fn name(&self) -> &'static str {
        "pii"
    }

    fn scan(&self, text: &str) -> Vec<Finding> {
        // TODO: phone numbers, postal addresses, government IDs.
        email_re()
            .find_iter(text)
            .map(|m| Finding {
                stage: "pii",
                kind: "email".to_string(),
                span: Span {
                    start: m.start(),
                    end: m.end(),
                },
                confidence: 0.8,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_an_email() {
        let findings = PiiStage.scan("contact ada@example.com please");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, "email");
    }
}
