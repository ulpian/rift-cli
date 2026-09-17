//! Column 1 — the project list.
//!
//! The persisted truth is the append-only `canopy_project_event` log in the
//! machine-global UI-state store (owned by the bin + `rift-store`). This module
//! holds only the **pure, engine-free** projection over that log: the event
//! model ([`ProjectEvent`]/[`ProjectOp`]) and the fold
//! ([`current_registry`]) that collapses the history into the current
//! [`ProjectEntry`] list. The bin reads rows, hands plain event structs to the
//! fold, and maps the result onto [`crate::Project`] — so the fold is
//! proptest-covered headlessly and Canopy never grows a storage dependency.

pub mod registry;
pub mod status;

pub use registry::{current_registry, EntryKind, ProjectEntry, ProjectEvent, ProjectOp};
pub use status::{
    derive as derive_status, LiveSessions, ProjectionState, SessionsRead, StatusInputs,
};
