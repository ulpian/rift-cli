//! The type wall around durable/sync writes, as the security model requires.
//!
//! Every surface that can persist to the checkpoint branch or push over sync
//! accepts **only** [`RedactedBytes`] — never `&[u8]`/`Vec<u8>`/`String`/
//! [`SecretBytes`]. Because [`RedactedBytes`] can be minted *only* by the
//! redaction pipeline (`rift-core` makes its buffer private; the sole constructor
//! is `RedactedBytes::seal`, called once in `rift-shadow-redact`), **no code path
//! can persist or sync un-redacted bytes — the compiler rejects it**.
//!
//! The raw, local-only shadow branch (which legitimately stores un-redacted bytes
//! on the developer's own machine) is written by a *separate, clearly-named*
//! [`LocalOnlyShadowWrite`] surface that speaks [`SecretBytes`] and is forbidden
//! from touching the permanent branch or sync. The two surfaces are disjoint by
//! type: a reviewer-proof boundary, not a naming convention.

use rift_core::{RedactedBytes, Result, SecretBytes};

/// The kind of object written to the checkpoint branch (informational; selects
/// the shard artifact name on the write side).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    /// `metadata.json`.
    Metadata,
    /// `attribution.json`.
    Attribution,
    /// `cost_estimate.json`.
    Cost,
    /// `redaction_report.json`.
    RedactionReport,
    /// `merkle_leaf.txt`.
    MerkleLeaf,
    /// An opaque content-addressed blob.
    Blob,
}

/// The durable-write surface (the redacted `rift/checkpoints/v1` branch).
///
/// Accepts **only** `&RedactedBytes`. There is deliberately no `&[u8]` overload
/// and no `*_raw` method — raw bytes do not type-check here.
///
/// The gate is non-bypassable: you cannot call this with raw bytes because you
/// cannot obtain a [`RedactedBytes`] without running redaction.
///
/// ```compile_fail
/// use rift_store::durable::{DurableWrite, ObjectKind};
/// struct W;
/// impl DurableWrite for W {
///     fn put_checkpoint_object(
///         &self,
///         _kind: ObjectKind,
///         _body: &rift_core::RedactedBytes,
///     ) -> rift_core::Result<()> {
///         Ok(())
///     }
/// }
/// let raw: &[u8] = b"unredacted";
/// // `&[u8]` is not `&RedactedBytes` — this does not compile.
/// let _ = W.put_checkpoint_object(ObjectKind::Blob, raw);
/// ```
pub trait DurableWrite {
    /// Write a content-addressed object to the checkpoint branch.
    ///
    /// # Errors
    /// Implementation-defined write errors (I/O, integrity).
    fn put_checkpoint_object(&self, kind: ObjectKind, body: &RedactedBytes) -> Result<()>;
}

/// The sync-write surface for a future, separately-secured cloud plane. Same
/// gate as today: only
/// [`RedactedBytes`] crosses it.
pub trait SyncWrite {
    /// Push a content-addressed object to the sync backend.
    ///
    /// # Errors
    /// Implementation-defined transport errors.
    fn push_object(&self, body: &RedactedBytes) -> Result<()>;
}

/// The raw, local-only shadow surface — the **only** place [`SecretBytes`] may be
/// written, and never to the permanent branch or sync. Implemented on a disjoint
/// target from [`DurableWrite`]/[`SyncWrite`].
pub trait LocalOnlyShadowWrite {
    /// Write raw bytes to the local-only shadow ref.
    ///
    /// # Errors
    /// Implementation-defined write errors.
    fn put_shadow_raw(&self, body: &SecretBytes) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Durable {
        written: std::cell::Cell<usize>,
        last: std::cell::RefCell<Vec<u8>>,
    }
    impl DurableWrite for Durable {
        fn put_checkpoint_object(&self, _kind: ObjectKind, body: &RedactedBytes) -> Result<()> {
            // The only thing a durable writer can do is *read* cleared bytes.
            self.last.replace(body.as_slice().to_vec());
            self.written.set(self.written.get() + 1);
            Ok(())
        }
    }

    /// Exercise the durable surface with a `RedactedBytes` minted by the **real**
    /// pipeline — the only production path that can produce one — proving a value
    /// that crossed the gate is what the durable surface accepts.
    #[test]
    fn durable_surface_accepts_pipeline_redacted_bytes() {
        let out = rift_shadow_redact::redact(&SecretBytes::from(
            "ok prose with a secret AKIAIOSFODNN7EXAMPLE inside",
        ))
        .unwrap();
        let w = Durable {
            written: std::cell::Cell::new(0),
            last: std::cell::RefCell::new(Vec::new()),
        };
        w.put_checkpoint_object(ObjectKind::Metadata, &out.redacted)
            .unwrap();
        assert_eq!(w.written.get(), 1);
        // the durable surface received cleared bytes (secret masked upstream)
        let written = String::from_utf8(w.last.borrow().clone()).unwrap();
        assert!(!written.contains("AKIAIOSFODNN7EXAMPLE"));
    }
}
