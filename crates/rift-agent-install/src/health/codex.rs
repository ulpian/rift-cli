//! Detector (a): Codex skipping Rift's hooks, or Rift unable to tell.
//!
//! Consumes [`CodexTrust`] exactly as the trust check computes it; no hashing
//! happens here, so the detector and the status row can never disagree.

use crate::native::CodexTrust;

use super::{Finding, Reason};

/// The Codex registry token the findings are filed under.
const CODEX: &str = "codex";

/// At most one finding: skipped hooks outrank an unverifiable answer, because
/// a handler Codex is known to skip is capture being lost now.
pub(super) fn codex_findings(trust: Option<&CodexTrust>) -> Vec<Finding> {
    let Some(trust) = trust else {
        return Vec::new();
    };
    let skipped = trust.needs_review();
    if !skipped.is_empty() {
        return vec![Finding::new(
            Reason::CodexHooksSkipped,
            CODEX,
            None,
            skipped.join(", "),
        )];
    }
    let reason = match trust.state() {
        "unverifiable" => Reason::CodexTrustUnverifiable,
        "unsupported_version" => Reason::CodexVersionUnsupported,
        _ => return Vec::new(),
    };
    vec![Finding::new(
        reason,
        CODEX,
        None,
        unverifiable_detail(trust),
    )]
}

/// What the recorded client says about the version, as identifiers: the
/// version itself, or which of the two facts is absent.
fn unverifiable_detail(trust: &CodexTrust) -> String {
    match &trust.client {
        None => "no recorded session".to_owned(),
        Some(client) => match &client.version {
            None => "no recorded version".to_owned(),
            Some(version) => format!("codex {version}"),
        },
    }
}
