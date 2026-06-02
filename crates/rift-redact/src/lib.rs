//! A pluggable redaction pipeline for Rift.
//!
//! Stages ([`RedactStage`]) each scan text and report [`Finding`]s; the
//! [`Pipeline`] merges them and produces masked output. [`target`] applies a
//! pipeline to `entire`'s captured files (working-tree copies only).

pub mod pipeline;
pub mod stages;
pub mod target;

pub use pipeline::{Finding, Masker, Pipeline, RedactStage, Span};
pub use stages::custom_pack::RulePack;
pub use target::{checkpoint_text_targets, redact_file, RedactionOutcome};
