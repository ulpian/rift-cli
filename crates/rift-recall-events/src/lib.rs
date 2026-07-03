//! `memd.*` serving events and the `rift hooks memd` handoff.
//!
//! Every serving decision emits a canonical event carrying
//! `estimated_tokens_returned` / `estimated_tokens_avoided`, so over-retrieval is
//! visible and Rift Cloud can attribute *measured* tokens avoided. The Rust side
//! does **not** write the hash chain: it hands the event to the Go capture CLI
//! via `rift hooks memd <verb>` with JSON on stdin (the plugin-protocol
//! convention), which redacts, chains, and stores it like any other event.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use sha2::{Digest, Sha256};

/// A serving decision event.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemdEvent {
    /// A manifest (no content) was served.
    ManifestServed {
        /// Tokens the manifest itself cost.
        estimated_tokens_returned: u32,
        /// Tokens avoided vs a full re-read.
        estimated_tokens_avoided: u32,
    },
    /// A validity-checked summary was served.
    SummaryServed {
        /// The summary handle.
        handle: String,
        /// Tokens the summary cost.
        estimated_tokens_returned: u32,
        /// Tokens avoided vs the full artifact.
        estimated_tokens_avoided: u32,
    },
    /// Exact content was retrieved (policy-gated, reason-required, audited).
    ExactRetrieved {
        /// The retrieved handle.
        handle: String,
        /// The reason supplied (mandatory).
        reason: String,
        /// Tokens the exact content cost.
        estimated_tokens_returned: u32,
    },
    /// A context pack was built.
    PackBuilt {
        /// Number of sections.
        sections: u32,
        /// Total pack tokens.
        total_tokens: u32,
        /// Tokens avoided vs full re-reads.
        estimated_tokens_avoided: u32,
    },
    /// A summary/handle was invalidated by a source change.
    Invalidated {
        /// The invalidated handle.
        handle: String,
    },
    /// A serve was refused (denied path/model, missing reason, untrusted-raw,
    /// or transcript-disabled). The `reason` is a neutral, non-leaking tag — it
    /// names *why*, never the withheld content. Audited so every serve decision
    /// (granted or refused) has exactly one terminal event (W10).
    Refused {
        /// The target handle/path the agent asked for.
        target: String,
        /// The neutral refusal reason tag (e.g. `policy_denied_exact`).
        reason: String,
    },
}

impl MemdEvent {
    /// The hook verb for this event (`memd.<verb>`).
    #[must_use]
    pub fn verb(&self) -> &'static str {
        match self {
            Self::ManifestServed { .. } => "manifest_served",
            Self::SummaryServed { .. } => "summary_served",
            Self::ExactRetrieved { .. } => "exact_retrieved",
            Self::PackBuilt { .. } => "pack_built",
            Self::Invalidated { .. } => "invalidated",
            Self::Refused { .. } => "refused",
        }
    }

    /// Tokens avoided by this decision (0 for exact/invalidated).
    #[must_use]
    pub fn tokens_avoided(&self) -> u32 {
        match self {
            Self::ManifestServed {
                estimated_tokens_avoided,
                ..
            }
            | Self::SummaryServed {
                estimated_tokens_avoided,
                ..
            }
            | Self::PackBuilt {
                estimated_tokens_avoided,
                ..
            } => *estimated_tokens_avoided,
            Self::ExactRetrieved { .. } | Self::Invalidated { .. } | Self::Refused { .. } => 0,
        }
    }

    /// Build the JSON envelope to pipe to `rift hooks memd <verb>` on stdin.
    /// Carries a content-addressed idempotency key so a retried handoff is a
    /// no-op downstream.
    #[must_use]
    pub fn to_hook_json(&self) -> String {
        let payload = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        let payload_bytes = serde_json::to_vec(&payload).unwrap_or_default();
        let mut h = Sha256::new();
        h.update(&payload_bytes);
        let mut key = String::with_capacity(64);
        use core::fmt::Write as _;
        for b in h.finalize() {
            let _ = write!(key, "{b:02x}");
        }
        let envelope = serde_json::json!({
            "schema": 1,
            "verb": format!("memd.{}", self.verb()),
            "idempotency_key": key,
            "payload": payload,
        });
        serde_json::to_string(&envelope).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbs_and_tokens_avoided() {
        let m = MemdEvent::ManifestServed {
            estimated_tokens_returned: 50,
            estimated_tokens_avoided: 900,
        };
        assert_eq!(m.verb(), "manifest_served");
        assert_eq!(m.tokens_avoided(), 900);
        let e = MemdEvent::ExactRetrieved {
            handle: "file:a@sha256:x".into(),
            reason: "debug".into(),
            estimated_tokens_returned: 200,
        };
        assert_eq!(e.verb(), "exact_retrieved");
        assert_eq!(e.tokens_avoided(), 0);
    }

    #[test]
    fn hook_json_is_well_formed_and_namespaced() {
        let m = MemdEvent::SummaryServed {
            handle: "summary:k".into(),
            estimated_tokens_returned: 30,
            estimated_tokens_avoided: 400,
        };
        let json = m.to_hook_json();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["verb"], "memd.summary_served");
        assert_eq!(v["payload"]["estimated_tokens_avoided"], 400);
        assert!(v["idempotency_key"].as_str().unwrap().len() == 64);
    }

    #[test]
    fn exact_event_carries_reason() {
        let e = MemdEvent::ExactRetrieved {
            handle: "file:a@sha256:x".into(),
            reason: "investigating regression".into(),
            estimated_tokens_returned: 10,
        };
        let json = e.to_hook_json();
        assert!(json.contains("investigating regression"));
    }

    #[test]
    fn idempotency_key_is_stable_for_same_event() {
        let a = MemdEvent::Invalidated {
            handle: "summary:k".into(),
        };
        let b = MemdEvent::Invalidated {
            handle: "summary:k".into(),
        };
        assert_eq!(a.to_hook_json(), b.to_hook_json());
    }

    #[test]
    fn refused_event_is_namespaced_and_leaks_no_content() {
        let e = MemdEvent::Refused {
            target: "file:secrets/.env@sha256:ab".into(),
            reason: "policy_denied_exact".into(),
        };
        assert_eq!(e.verb(), "refused");
        assert_eq!(e.tokens_avoided(), 0);
        let v: serde_json::Value = serde_json::from_str(&e.to_hook_json()).unwrap();
        assert_eq!(v["verb"], "memd.refused");
        // the reason names *why*, never the withheld bytes
        assert_eq!(v["payload"]["reason"], "policy_denied_exact");
    }
}
