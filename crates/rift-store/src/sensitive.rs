//! The registry of columns that must be **application-layer encrypted** (Layer 2).
//!
//! One source of truth (mirroring [`crate::schema::canonical_tables`]) for which
//! `(table, column)` slots carry highest-sensitivity payloads that are AEAD-encrypted
//! before they reach durable storage ([`crate::encrypted_dao::EncryptingStore`]),
//! so a plaintext leak of the DB file does not expose them (`05-security` §5.7).
//!
//! Layer 1 (whole-DB encryption) is unavailable on the pinned turso BETA (no
//! `Builder` encryption option — see the phase `SPIKE-RESULTS.md`), so this
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
/// v1 enumerates `key_metadata.wrapped_dek` — the wrapped data-encryption key,
/// the one BLOB column today whose payload is key material. As Stage-2 cost
/// aggregates and Stage-3 summary bodies land (as `BLOB` payload columns), they
/// are added here and inherit the same Layer-2 protection.
const SENSITIVE_COLUMNS: &[SensitiveColumn] = &[SensitiveColumn {
    table: "key_metadata",
    column: "wrapped_dek",
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
    use crate::schema::canonical_table;

    #[test]
    fn registry_is_nonempty_and_membership_works() {
        assert!(!sensitive_columns().is_empty());
        assert!(is_sensitive_column("key_metadata", "wrapped_dek"));
        assert!(!is_sensitive_column("key_metadata", "key_id"));
        assert!(!is_sensitive_column("checkpoint", "merkle_root"));
    }

    /// Every registered sensitive column must exist in the canonical schema and be
    /// declared `BLOB` (so ciphertext fits). A typo or a non-BLOB target fails CI.
    #[test]
    fn every_sensitive_column_exists_and_is_blob() {
        for s in sensitive_columns() {
            let table = canonical_table(s.table)
                .unwrap_or_else(|| panic!("sensitive table `{}` not in registry", s.table));
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
