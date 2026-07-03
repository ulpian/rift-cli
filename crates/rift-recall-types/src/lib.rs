//! Shared recall types: content-addressed handles, the four-state response
//! envelope, and trust tags.
//!
//! These types encode the honest cache/memory semantics (`01-vision` principle
//! 10): "file unchanged" never means "the model knows the file". Every served
//! artifact is one of exactly four epistemic states ([`ArtifactState`]), and a
//! retrieval [`Handle`] always resolves to an immutable Git object / digest —
//! never a floating path — so a forged or stale handle can never serve wrong
//! bytes (`05-security` §5.5).
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

use core::fmt;
use core::str::FromStr;

pub mod checkpoint;
pub mod outcome;
pub mod scope;

pub use outcome::{LabelSubject, OutcomeKind, OUTCOME_KINDS};
pub use scope::{BranchScope, CheckpointWindow, CrossScopeGrant, DeveloperScope, Scope};

/// Source-trust classification of a served payload.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    /// Structural code fact (file content/edit).
    CodeFact,
    /// Git fact.
    GitFact,
    /// Test runner output.
    TestOutput,
    /// Arbitrary tool output (untrusted — served only as neutral digest by default).
    ToolOutputUntrusted,
    /// Transcript/prompt text (untrusted).
    TranscriptUntrusted,
}

impl Trust {
    /// Whether this trust class may ever be inlined as raw exact text.
    #[must_use]
    pub fn allows_raw_inline(self) -> bool {
        matches!(self, Self::CodeFact | Self::GitFact | Self::TestOutput)
    }

    /// Whether this is an adversary-controllable (untrusted) class. Untrusted
    /// payloads are served only as a neutral digest + handle (W9).
    #[must_use]
    pub fn is_untrusted(self) -> bool {
        matches!(self, Self::ToolOutputUntrusted | Self::TranscriptUntrusted)
    }
}

/// A 1-based, inclusive line range (`Lx-Ly`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct LineRange {
    /// First line (1-based).
    pub lo: u32,
    /// Last line (inclusive).
    pub hi: u32,
}

/// Errors parsing a [`Handle`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum HandleParseError {
    /// Scheme is not one of `file:`/`log:`/`summary:`.
    #[error("unknown handle scheme")]
    UnknownScheme,
    /// A `file:` hash was not 64 hex chars.
    #[error("file handle hash must be 64 hex chars")]
    BadHash,
    /// A path was empty, absolute, traversing (`..`), or contained NUL.
    #[error("unsafe or empty path in handle")]
    UnsafePath,
    /// A line range was malformed or inverted.
    #[error("malformed line range")]
    BadRange,
    /// The overall shape was malformed.
    #[error("malformed handle")]
    Malformed,
}

/// A content-addressed retrieval handle. Resolves to an immutable Git object or
/// the digest store — never a path on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Handle {
    /// A specific file revision (by content hash), optionally a line range.
    File {
        /// Repo-relative path (display/context only; resolution is by hash).
        path: String,
        /// 64-char lowercase hex SHA-256 of the content.
        sha256: String,
        /// Optional line range.
        range: Option<LineRange>,
    },
    /// A captured tool result, by `tool_use_id`.
    Log {
        /// The tool-use id naming the immutable captured result.
        tool_use_id: String,
    },
    /// A hash-bound summary, by its key.
    Summary {
        /// The summary key (encodes source + versions).
        key: String,
    },
}

fn path_is_safe(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.contains('\0')
        && !p.split('/').any(|seg| seg == "..")
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl fmt::Display for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File {
                path,
                sha256,
                range,
            } => {
                write!(f, "file:{path}@sha256:{sha256}")?;
                if let Some(r) = range {
                    write!(f, ":L{}-L{}", r.lo, r.hi)?;
                }
                Ok(())
            }
            Self::Log { tool_use_id } => write!(f, "log:{tool_use_id}"),
            Self::Summary { key } => write!(f, "summary:{key}"),
        }
    }
}

impl FromStr for Handle {
    type Err = HandleParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(rest) = s.strip_prefix("file:") {
            // <path>@sha256:<hash>[:Lx-Ly]
            let (path, after) = rest
                .rsplit_once("@sha256:")
                .ok_or(HandleParseError::Malformed)?;
            if !path_is_safe(path) {
                return Err(HandleParseError::UnsafePath);
            }
            let (hash, range) = match after.split_once(':') {
                Some((h, r)) => (h, Some(parse_range(r)?)),
                None => (after, None),
            };
            if !is_hex64(hash) {
                return Err(HandleParseError::BadHash);
            }
            Ok(Self::File {
                path: path.to_string(),
                sha256: hash.to_ascii_lowercase(),
                range,
            })
        } else if let Some(id) = s.strip_prefix("log:") {
            if id.is_empty() || id.contains('\0') {
                return Err(HandleParseError::Malformed);
            }
            Ok(Self::Log {
                tool_use_id: id.to_string(),
            })
        } else if let Some(key) = s.strip_prefix("summary:") {
            if key.is_empty() || key.contains('\0') {
                return Err(HandleParseError::Malformed);
            }
            Ok(Self::Summary {
                key: key.to_string(),
            })
        } else {
            Err(HandleParseError::UnknownScheme)
        }
    }
}

fn parse_range(r: &str) -> Result<LineRange, HandleParseError> {
    let (lo, hi) = r.split_once('-').ok_or(HandleParseError::BadRange)?;
    let lo = lo.strip_prefix('L').ok_or(HandleParseError::BadRange)?;
    let hi = hi.strip_prefix('L').ok_or(HandleParseError::BadRange)?;
    let lo: u32 = lo.parse().map_err(|_| HandleParseError::BadRange)?;
    let hi: u32 = hi.parse().map_err(|_| HandleParseError::BadRange)?;
    if lo == 0 || lo > hi {
        return Err(HandleParseError::BadRange);
    }
    Ok(LineRange { lo, hi })
}

/// Why an artifact's content was omitted from a response.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OmitReason {
    /// The per-turn/pack token budget was exhausted.
    BudgetExhausted,
    /// Policy denied exact retrieval.
    PolicyDeniedExact,
    /// A sensitive path requires an explicit reason.
    SensitivePathRequiresReason,
    /// The summary is invalid because the source changed.
    SummaryInvalidSourceChanged,
    /// The source was deleted or redacted away.
    SourceDeletedOrRedacted,
    /// The artifact was not requested (manifest-first default).
    NotRequested,
}

/// One of exactly four epistemic states for a served artifact. Only
/// `SummaryIncluded`/`ExactIncluded` carry content (and charge budget);
/// `KnownUnchanged`/`AvailableByHandle` carry none.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ArtifactState {
    /// The agent has already seen this exact hash; bytes are NOT re-sent.
    KnownUnchanged {
        /// The retrieval handle (if the agent wants the bytes again).
        handle: String,
        /// The content hash this claim is based on.
        basis_sha256: String,
        /// The turn the agent last saw this hash (if known).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_seen_turn: Option<u64>,
    },
    /// A validity-checked digest is inline; source unchanged vs `basis_sha256`.
    SummaryIncluded {
        /// The retrieval handle.
        handle: String,
        /// The content hash the summary is bound to.
        basis_sha256: String,
        /// The neutral digest.
        summary: String,
        /// The hash-bound summary key (so a consumer can re-resolve it).
        summary_key: String,
    },
    /// Exact bytes inline (policy-gated + audited).
    ExactIncluded {
        /// The retrieval handle.
        handle: String,
        /// The content hash of the bytes.
        basis_sha256: String,
        /// The exact content.
        content: String,
        /// The line range served, when a partial range (else the whole artifact).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        range: Option<LineRange>,
    },
    /// Nothing inline; fetch via `handle` (manifest-first default / out of budget).
    AvailableByHandle {
        /// The retrieval handle.
        handle: String,
        /// The basis content hash, when one is known (absent for deleted sources).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        basis_sha256: Option<String>,
        /// Why the content was omitted.
        reason: OmitReason,
    },
}

impl ArtifactState {
    /// Whether this state carries inline content (and therefore charges budget).
    #[must_use]
    pub fn carries_content(&self) -> bool {
        matches!(
            self,
            Self::SummaryIncluded { .. } | Self::ExactIncluded { .. }
        )
    }
}

/// A per-artifact response: its path, trust class, epistemic state, and the
/// tokens it costs (0 for content-free states).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Envelope {
    /// Repo-relative path (display/context).
    pub path: String,
    /// Source-trust class.
    pub trust: Trust,
    /// The epistemic state.
    pub state: ArtifactState,
    /// Estimated tokens this envelope contributes to the prompt.
    pub estimated_tokens_returned: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_and_parses_all_schemes() {
        let cases = [
            "file:src/main.rs@sha256:".to_string() + &"a".repeat(64),
            format!("file:src/main.rs@sha256:{}:L10-L42", "b".repeat(64)),
            "log:toolu_abc123".to_string(),
            "summary:deadbeefkey".to_string(),
        ];
        for c in cases {
            let h: Handle = c.parse().unwrap();
            assert_eq!(h.to_string(), c, "roundtrip failed for {c}");
        }
    }

    #[test]
    fn rejects_unsafe_paths_and_bad_hashes() {
        let h = format!("file:../etc/passwd@sha256:{}", "a".repeat(64));
        assert_eq!(h.parse::<Handle>(), Err(HandleParseError::UnsafePath));
        let h = "file:/abs@sha256:".to_string() + &"a".repeat(64);
        assert_eq!(h.parse::<Handle>(), Err(HandleParseError::UnsafePath));
        let h = "file:ok@sha256:short";
        assert_eq!(h.parse::<Handle>(), Err(HandleParseError::BadHash));
        assert_eq!(
            "ftp:whatever".parse::<Handle>(),
            Err(HandleParseError::UnknownScheme)
        );
    }

    #[test]
    fn rejects_inverted_range() {
        let h = format!("file:a@sha256:{}:L42-L10", "a".repeat(64));
        assert_eq!(h.parse::<Handle>(), Err(HandleParseError::BadRange));
        let h = format!("file:a@sha256:{}:L0-L5", "a".repeat(64));
        assert_eq!(h.parse::<Handle>(), Err(HandleParseError::BadRange));
    }

    #[test]
    fn only_content_states_carry_content() {
        let known = ArtifactState::KnownUnchanged {
            handle: "h".into(),
            basis_sha256: "x".into(),
            last_seen_turn: None,
        };
        let summ = ArtifactState::SummaryIncluded {
            handle: "h".into(),
            basis_sha256: "x".into(),
            summary: "3 hunks".into(),
            summary_key: "k".into(),
        };
        assert!(!known.carries_content());
        assert!(summ.carries_content());
    }

    #[test]
    fn envelope_serializes_with_state_tag() {
        let env = Envelope {
            path: "a.rs".into(),
            trust: Trust::CodeFact,
            state: ArtifactState::AvailableByHandle {
                handle: "summary:k".into(),
                basis_sha256: None,
                reason: OmitReason::BudgetExhausted,
            },
            estimated_tokens_returned: 0,
        };
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"state\":\"available_by_handle\""));
        assert!(json.contains("budget_exhausted"));
    }

    #[test]
    fn trust_inline_policy() {
        assert!(Trust::CodeFact.allows_raw_inline());
        assert!(!Trust::ToolOutputUntrusted.allows_raw_inline());
        assert!(!Trust::TranscriptUntrusted.allows_raw_inline());
    }

    proptest::proptest! {
        /// Any valid file handle round-trips through Display/FromStr.
        #[test]
        fn file_handle_roundtrips(path in "[a-z][a-z0-9_/]{0,20}", lo in 1u32..1000, extra in 0u32..1000) {
            let hash = "c".repeat(64);
            let range = Some(LineRange { lo, hi: lo + extra });
            let h = Handle::File { path: path.clone(), sha256: hash, range };
            let parsed: Handle = h.to_string().parse().unwrap();
            proptest::prop_assert_eq!(h, parsed);
        }

        /// The parser never panics on arbitrary input.
        #[test]
        fn parse_never_panics(s in ".{0,80}") {
            let _ = s.parse::<Handle>();
        }
    }
}
