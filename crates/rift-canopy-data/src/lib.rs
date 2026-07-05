//! rift-canopy-data — the dashboard's data plane (ADR-0009).
//!
//! Owns every dashboard read and every UI-fact persist: the live bus and its
//! worker, the watermark poller and change observation, the append-only
//! UI-facts store over the canopy registry tables, the status/detail builders,
//! and the graph feeds. `rift-canopy` (the render/reducer side) never imports
//! this crate — the edge points one way, so the dashboard stays storage-free
//! and this plane is testable against tempdir projections without the binary.
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

pub mod bus;
pub mod store;

pub use bus::{spawn, spawn_with_root, Bus};
pub use store::{runtime, CanopyStore};
