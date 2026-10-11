//! Recall serving policy: per-turn budgets, sensitive-path gates, trust
//! enforcement, and audited exact retrieval.
//!
//! The serve gate is policy-first: the default response is a
//! manifest, not content; exact content requires a `reason`, passes policy, and
//! is audited. Budgets bound over-retrieval; sensitive paths force a reason;
//! untrusted content is never served raw. Every granted exact
//! retrieval yields an [`AuditDecision::Allow`] carrying the audit record the
//! caller must emit through `rift hooks memd`.
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

mod budget;
mod capability;
mod engine;
mod levers;
mod policy_edit;
mod serve_policy;

pub use budget::{TurnBudget, DEFAULT_MAX_TOKENS_PER_TURN, DEFAULT_MAX_TOOL_CALLS};
pub use capability::{resolve, CapReason, Effective, FamilyCapability};
pub use engine::{
    normalize_rel, AuditDecision, Downgrade, EmptyReason, NonEmptyReason, Optimization,
    PolicyEngine, SERVE_POLICY_VERSION,
};
pub use levers::{
    AgentModes, DocChannel, DocPolicy, Formats, InterceptionPolicy, Mode, SearchPolicy,
    DEFAULT_ANSWER_BUDGET_TOKENS, DEFAULT_BASIS_TTL_DAYS, DEFAULT_CONVERT_BUDGET_MS,
    DEFAULT_DOC_TTL_DAYS, DEFAULT_FIRST_CONTACT_MIN_BYTES, DEFAULT_FIRST_CONTACT_MIN_LINES,
    DEFAULT_GLOB_TOP_N, DEFAULT_GREP_HEAD_LIMIT, DEFAULT_MAX_INLINE_HASH_BYTES,
    DEFAULT_MAX_INPUT_BYTES, DEFAULT_MAX_SLICE_BYTES, DEFAULT_MIN_SLICE_LINES,
    DEFAULT_PDF_MIN_CHARS_PER_PAGE, DEFAULT_RECOVERY_BUDGET, DEFAULT_TABLE_ROWS,
};
pub use policy_edit::{
    patch_injection_turn_budget, patch_lever_mode, LeverTable, ServePolicyEditError,
};
pub use serve_policy::{
    load_serve_policy, ServePolicy, ServePolicyRevision, ServePolicySnapshot, ServePolicySource,
    DEFAULT_INJECTION_TURN_BUDGET, SERVE_POLICY_TEMPLATE,
};
