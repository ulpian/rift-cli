//! Rift shared core.
//!
//! Small, low-dependency foundation every other crate builds on. It defines:
//!
//! * the shared [`RiftError`] type and [`Result`] alias (`error`);
//! * the strongly-typed identifier system — content-addressed hashes
//!   ([`Sha256Id`], [`Blake3Id`]), the time-ordered [`Ulid`] with an injectable
//!   [`UlidSource`], and the semantic newtypes ([`SessionId`], [`CheckpointId`],
//!   …) ([`id`]);
//! * the per-era checkpoint-id derivation rules, as pure byte functions
//!   ([`checkpoint_id`]);
//! * the [`Timestamp`] type for bitemporal facts ([`time`]);
//! * local-only developer [`identity`] (hashed, private, no cloud);
//! * the immutability primitives ([`immutable`]) the storage layer uses to
//!   enforce append-only semantics.
//!
//! The rules these types encode are the append-only storage discipline and the
//! security posture in the repository's `SECURITY.md`.
#![forbid(unsafe_code)]
// Tests legitimately use unwrap/expect/panic; keep library code clean of them.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod boundary_gap;
pub mod checkpoint_id;
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
    capture_session_row_id, Blake3Id, CallId, CheckpointId, CounterUlidSource, FamilyIds,
    FileRevisionId, MonotonicUlidSource, ProjectId, RepoId, SessionId, Sha256Id, SummaryId,
    ToolCallId, TurnId, Ulid, UlidSource,
};
pub use identity::{
    bind_session, binding_basis, binding_evidence, snapshot as identity_snapshot, BindingBasis,
    BindingEvidence, Confidence, DeveloperId, FixtureIdentity, IdentitySnapshot, IdentitySource,
    MachineId, SessionBinding,
};
pub use immutable::{CanonicalEntity, Immutable};
pub use redacted::{RedactedBytes, RedactionProvenance, SecretBytes};
pub use time::Timestamp;
