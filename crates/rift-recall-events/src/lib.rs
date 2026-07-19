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

/// An opaque MCP-connection identity. One `rift-memd serve --mcp` process owns
/// exactly one value; it is deliberately unrelated to capture-session ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct McpThreadId(rift_core::Blake3Id);

impl McpThreadId {
    /// Derive a process identity from injected entropy. Production supplies OS
    /// randomness; tests supply fixed bytes.
    #[must_use]
    pub fn from_seed(seed: &[u8]) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(b"rift:memd:mcp-thread:v1\0");
        h.update(seed);
        Self(rift_core::Blake3Id::from_bytes(*h.finalize().as_bytes()))
    }

    /// The stable lowercase representation stored in `recall.db`.
    #[must_use]
    pub fn to_hex(self) -> String {
        self.0.to_hex()
    }
}

impl core::fmt::Display for McpThreadId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

/// One decoded MCP invocation within an [`McpThreadId`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct McpCallId(rift_core::Blake3Id);

impl McpCallId {
    /// Derive a unique, replay-stable call id from the process id and its
    /// monotonic decoded-call ordinal.
    #[must_use]
    pub fn derive(thread: McpThreadId, ordinal: u64) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(b"rift:memd:mcp-call:v1\0");
        h.update(thread.0.as_bytes());
        h.update(&ordinal.to_be_bytes());
        Self(rift_core::Blake3Id::from_bytes(*h.finalize().as_bytes()))
    }

    /// The stable lowercase representation used as the audit idempotency key.
    #[must_use]
    pub fn to_hex(self) -> String {
        self.0.to_hex()
    }
}

impl core::fmt::Display for McpCallId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

/// Neutral identity attached to one decoded MCP invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpCallContext {
    /// Per-process connection identity.
    pub thread_id: McpThreadId,
    /// Per-invocation identity and downstream idempotency key.
    pub call_id: McpCallId,
    /// Decoded MCP wire tool name (never its arguments).
    pub tool_name: String,
}

/// A decoded invocation's terminal result. Internal failures after decoding
/// are counted without exposing error text; failures before decoding never
/// construct one of these.
#[derive(Clone, Copy, Debug)]
pub enum McpTerminal<'a> {
    /// The policy/serve layer produced a canonical `memd.*` decision.
    Event(&'a MemdEvent),
    /// Dispatch failed after the tool name and parameters decoded.
    InternalError,
}

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
        /// Total rendered-pack tokens (the pack compiler's own figure).
        total_tokens: u32,
        /// Tokens the serialized response cost (the accounting seam's measure,
        /// ADR-0049 — the full payload, not just the rendered pack).
        estimated_tokens_returned: u32,
        /// Tokens avoided vs full re-reads.
        estimated_tokens_avoided: u32,
    },
    /// A summary/handle was invalidated by a source change.
    Invalidated {
        /// The invalidated handle.
        handle: String,
        /// Tokens the invalidation response itself cost (ADR-0049 — no
        /// response class is exempt from the accounting).
        estimated_tokens_returned: u32,
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
        /// Tokens the refusal response itself cost (ADR-0049 — measured and
        /// charged like any serve; never the withheld bytes).
        estimated_tokens_returned: u32,
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

    /// Tokens returned by this decision — the accounting seam's measure of the
    /// serialized response, on every variant (ADR-0049): a refusal or
    /// invalidation costs its own (small) payload, never the withheld bytes.
    #[must_use]
    pub fn tokens_returned(&self) -> u32 {
        match self {
            Self::ManifestServed {
                estimated_tokens_returned,
                ..
            }
            | Self::SummaryServed {
                estimated_tokens_returned,
                ..
            }
            | Self::ExactRetrieved {
                estimated_tokens_returned,
                ..
            }
            | Self::PackBuilt {
                estimated_tokens_returned,
                ..
            }
            | Self::Invalidated {
                estimated_tokens_returned,
                ..
            }
            | Self::Refused {
                estimated_tokens_returned,
                ..
            } => *estimated_tokens_returned,
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

impl McpTerminal<'_> {
    /// The neutral hook verb for this terminal.
    #[must_use]
    pub fn verb(self) -> &'static str {
        match self {
            Self::Event(event) => event.verb(),
            Self::InternalError => "internal_error",
        }
    }

    /// Build the schema-v2 terminal envelope. The call id, rather than outcome
    /// content, is the idempotency key: identical invocations remain distinct,
    /// while replaying this exact handoff remains a no-op.
    #[must_use]
    pub fn to_hook_json(&self, call: &McpCallContext, recorded_at: rift_core::Timestamp) -> String {
        let (verb, returned, avoided, payload) = match self {
            Self::Event(event) => (
                format!("memd.{}", event.verb()),
                event.tokens_returned(),
                event.tokens_avoided(),
                serde_json::to_value(event).unwrap_or(serde_json::Value::Null),
            ),
            Self::InternalError => (
                "memd.internal_error".to_string(),
                0,
                0,
                serde_json::Value::Null,
            ),
        };
        let call_id = call.call_id.to_hex();
        let envelope = serde_json::json!({
            "schema": 2,
            "verb": verb,
            "idempotency_key": call_id,
            "call": {
                "call_id": call_id,
                "thread_id": call.thread_id.to_hex(),
                "tool_name": call.tool_name,
                "recorded_at": recorded_at.as_millis(),
                "tokens_returned": returned,
                "tokens_avoided": avoided,
            },
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
            estimated_tokens_returned: 9,
        };
        let b = MemdEvent::Invalidated {
            handle: "summary:k".into(),
            estimated_tokens_returned: 9,
        };
        assert_eq!(a.to_hook_json(), b.to_hook_json());
    }

    #[test]
    fn call_identity_is_thread_scoped_and_ordinal_sensitive() {
        let a = McpThreadId::from_seed(b"process-a");
        let a_again = McpThreadId::from_seed(b"process-a");
        let b = McpThreadId::from_seed(b"process-b");
        assert_eq!(a, a_again);
        assert_ne!(a, b);
        assert_eq!(McpCallId::derive(a, 1), McpCallId::derive(a, 1));
        assert_ne!(McpCallId::derive(a, 1), McpCallId::derive(a, 2));
        assert_ne!(McpCallId::derive(a, 1), McpCallId::derive(b, 1));
    }

    #[test]
    fn v2_terminal_envelope_is_invocation_addressed_and_neutral() {
        let thread = McpThreadId::from_seed(b"process");
        let call = McpCallContext {
            thread_id: thread,
            call_id: McpCallId::derive(thread, 7),
            tool_name: "rift_read_summary".into(),
        };
        let event = MemdEvent::SummaryServed {
            handle: "summary:neutral".into(),
            estimated_tokens_returned: 12,
            estimated_tokens_avoided: 90,
        };
        let json =
            McpTerminal::Event(&event).to_hook_json(&call, rift_core::Timestamp::from_millis(123));
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["schema"], 2);
        assert_eq!(value["idempotency_key"], call.call_id.to_hex());
        assert_eq!(value["call"]["thread_id"], thread.to_hex());
        assert_eq!(value["call"]["tool_name"], "rift_read_summary");
        assert_eq!(value["call"]["recorded_at"], 123);
        assert_eq!(value["call"]["tokens_returned"], 12);
        assert_eq!(value["call"]["tokens_avoided"], 90);
        assert!(!json.contains("arguments"));
        assert!(!json.contains("transcript"));

        let internal =
            McpTerminal::InternalError.to_hook_json(&call, rift_core::Timestamp::from_millis(124));
        assert!(internal.contains("memd.internal_error"));
        assert!(!internal.contains("error message"));
    }

    #[test]
    fn pack_built_carries_the_returned_figure() {
        // ADR-0049: the accessor reads the seam's measure, never `total_tokens`,
        // and both hook envelopes (v1 payload, v2 call) carry it.
        let e = MemdEvent::PackBuilt {
            sections: 3,
            total_tokens: 120,
            estimated_tokens_returned: 480,
            estimated_tokens_avoided: 900,
        };
        assert_eq!(e.tokens_returned(), 480);
        let v: serde_json::Value = serde_json::from_str(&e.to_hook_json()).unwrap();
        assert_eq!(v["payload"]["estimated_tokens_returned"], 480);
        assert_eq!(v["payload"]["total_tokens"], 120);
        let thread = McpThreadId::from_seed(b"process");
        let call = McpCallContext {
            thread_id: thread,
            call_id: McpCallId::derive(thread, 1),
            tool_name: "rift_context_pack".into(),
        };
        let v2: serde_json::Value = serde_json::from_str(
            &McpTerminal::Event(&e).to_hook_json(&call, rift_core::Timestamp::from_millis(1)),
        )
        .unwrap();
        assert_eq!(v2["call"]["tokens_returned"], 480);
        assert_eq!(v2["call"]["tokens_avoided"], 900);
    }

    #[test]
    fn refused_event_is_namespaced_and_leaks_no_content() {
        let e = MemdEvent::Refused {
            target: "file:secrets/.env@sha256:ab".into(),
            reason: "policy_denied_exact".into(),
            estimated_tokens_returned: 40,
        };
        assert_eq!(e.verb(), "refused");
        assert_eq!(e.tokens_avoided(), 0);
        // ADR-0049: the refusal's own payload cost is audited, never the
        // withheld bytes.
        assert_eq!(e.tokens_returned(), 40);
        let v: serde_json::Value = serde_json::from_str(&e.to_hook_json()).unwrap();
        assert_eq!(v["verb"], "memd.refused");
        // the reason names *why*, never the withheld bytes
        assert_eq!(v["payload"]["reason"], "policy_denied_exact");
    }
}
