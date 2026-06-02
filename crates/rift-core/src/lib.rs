//! Shared domain for the Rift CLI.
//!
//! This crate knows how `entire` lays out its data on disk and on the
//! `entire/checkpoints/v1` orphan branch, and provides the read-side primitives
//! the rest of Rift builds on: the data model, source scanning, JSONL parsing,
//! a git reader abstraction, and Rift's own configuration.

pub mod checkpoint_store;
pub mod config;
pub mod git;
pub mod jsonl;
pub mod metrics;
pub mod model;
pub mod paths;
pub mod scan;

pub use metrics::{read_branch_metrics, BranchMetrics, CheckpointMetric};
pub use scan::{scan, ScanReport};
