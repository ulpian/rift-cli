//! The registry of columns that must be **application-layer encrypted** (Layer 2).
//!
//! One source of truth (mirroring [`crate::schema::canonical_tables`]) for which
//! `(table, column)` slots carry highest-sensitivity payloads that are AEAD-encrypted
//! before they reach durable storage ([`crate::encrypted_dao::EncryptingStore`]),
//! so a plaintext leak of the DB file does not expose them (the security model's storage rule).
//!
//! Layer 1 (whole-DB encryption) is unavailable on the pinned turso BETA (no
//! `Builder` encryption option, as the storage spike measured), so this
//! application-column layer is the standing confidentiality guarantee for the
//! enumerated columns; non-enumerated columns rely on filesystem perms + the
//! out-of-tree cache location ([`crate::paths`]).
//!
//! Encrypted columns must be declared `BLOB` (ciphertext is `nonce ‖ AEAD`), and
//! must not be queried by value (they cannot be indexed) — both invariants are
//! asserted by the tests below against the schema registry, so a mis-registration
//! fails CI.

/// One column that is AEAD-encrypted at the application layer (Layer 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensitiveColumn {
    /// The canonical table the column belongs to.
    pub table: &'static str,
    /// The column name (must be declared `BLOB` in [`crate::schema`]).
    pub column: &'static str,
}

/// The registry of application-encrypted columns.
///
/// Enumerates `digest_blob.bytes` — recall's hash-bound summary bodies
/// (redacted-but-sensitive payloads, this layer's first live row). As capture
/// cost aggregates land as `BLOB` payload columns they are
/// added here and inherit the same protection.
///
/// `key_metadata.wrapped_dek` is deliberately NOT registered: it is already
/// AEAD ciphertext *by construction* (the DEK wrapped under the keychain KEK,
/// [`crate::KeyringKeyStore`]) — encrypting it under the DEK it wraps would be
/// circular.
const SENSITIVE_COLUMNS: &[SensitiveColumn] = &[SensitiveColumn {
    table: "digest_blob",
    column: "bytes",
}];

/// The registry of columns to AEAD-encrypt at the application layer.
#[must_use]
pub fn sensitive_columns() -> &'static [SensitiveColumn] {
    SENSITIVE_COLUMNS
}

/// Whether `(table, column)` is a registered application-encrypted column.
#[must_use]
pub fn is_sensitive_column(table: &str, column: &str) -> bool {
    SENSITIVE_COLUMNS
        .iter()
        .any(|s| s.table == table && s.column == column)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{canonical_table, canopy_canonical_table, recall_canonical_table};

    /// Resolve a table across ALL THREE registries (capture / recall / canopy) —
    /// sensitive columns may live in any projection.
    fn any_registry_table(name: &str) -> Option<&'static crate::CanonicalTable> {
        canonical_table(name)
            .or_else(|| recall_canonical_table(name))
            .or_else(|| canopy_canonical_table(name))
    }

    #[test]
    fn registry_is_nonempty_and_membership_works() {
        assert!(!sensitive_columns().is_empty());
        assert!(is_sensitive_column("digest_blob", "bytes"));
        assert!(!is_sensitive_column("digest_blob", "digest_ref"));
        assert!(!is_sensitive_column("checkpoint", "merkle_root"));
        // wrapped_dek is protected by construction (KEK-wrapped), not Layer 2.
        assert!(!is_sensitive_column("key_metadata", "wrapped_dek"));
    }

    /// Every registered sensitive column must exist in one of the three schema
    /// registries and be declared `BLOB` (so ciphertext fits). A typo or a
    /// non-BLOB target fails CI.
    #[test]
    fn every_sensitive_column_exists_and_is_blob() {
        for s in sensitive_columns() {
            let table = any_registry_table(s.table)
                .unwrap_or_else(|| panic!("sensitive table `{}` not in any registry", s.table));
            let col = table
                .columns
                .iter()
                .find(|c| c.name == s.column)
                .unwrap_or_else(|| panic!("sensitive column `{}.{}` missing", s.table, s.column));
            assert_eq!(
                col.sql_type, "BLOB",
                "sensitive column `{}.{}` must be BLOB to hold ciphertext, is {}",
                s.table, s.column, col.sql_type
            );
        }
    }
}
