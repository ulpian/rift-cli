//! Immutability primitives.
//!
//! These are markers, not enforcement — enforcement lives in `rift-store`
//! (SQL triggers + an insert-only DAO + a registry-driven property test). The
//! markers let the type system document which values are append-only facts.

/// Marker for values/records that must never be updated or deleted in place.
///
/// Corrections to an `Immutable` value are modelled as *new* values that
/// supersede prior ones, never as mutations of the original.
pub trait Immutable {}

/// An append-only entity that maps to a specific canonical storage table.
///
/// `rift-store` uses [`CanonicalEntity::TABLE`] to wire entities to their
/// trigger-guarded tables.
pub trait CanonicalEntity: Immutable {
    /// The canonical table name this entity is stored in.
    const TABLE: &'static str;
}
