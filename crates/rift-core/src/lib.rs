//! Rift shared core.
//!
//! Small, low-dependency foundation every other crate builds on. It defines:
//!
//! * the shared [`RiftError`] type and [`Result`] alias ([`error`]);
//! * the strongly-typed identifier system — content-addressed hashes
//!   ([`Sha256Id`], [`Blake3Id`]), the time-ordered [`Ulid`] with an injectable
//!   [`UlidSource`], and the semantic newtypes ([`SessionId`], [`CheckpointId`],
//!   …) ([`id`]);
//! * the [`Timestamp`] type for bitemporal facts ([`time`]);
//! * local-only developer [`identity`] (hashed, private, no cloud);
//! * the immutability primitives ([`immutable`]) the storage layer uses to
//!   enforce append-only semantics.
//!
//! See `build-plan/00-overview/04-database-design-4nf-5nf-immutability.md` and
//! `…/05-security-model-and-threat-model.md` for the rules these types encode.
#![forbid(unsafe_code)]
// Tests legitimately use unwrap/expect/panic; keep library code clean of them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
// Style-only pedantic lints we deliberately accept (not correctness/security).
#![allow(clippy::module_name_repetitions, clippy::doc_markdown)]

mod error;
pub mod event;
pub mod id;
pub mod identity;
pub mod immutable;
pub mod redacted;
pub mod time;

pub use error::{Result, RiftError};
pub use event::{AgentId, CanonicalEvent, EventKind, Trust};
pub use id::{
    Blake3Id, CallId, CheckpointId, CounterUlidSource, FileRevisionId, MonotonicUlidSource, RepoId,
    SessionId, Sha256Id, SummaryId, ToolCallId, TurnId, Ulid, UlidSource,
};
pub use identity::{
    BindingBasis, Confidence, DeveloperId, FixtureIdentity, IdentitySnapshot, IdentitySource,
    MachineId, SessionBinding,
};
pub use immutable::{CanonicalEntity, Immutable};
pub use redacted::{RedactedBytes, RedactionProvenance, SecretBytes};
pub use time::Timestamp;
