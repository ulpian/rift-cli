//! The cross-scope grant's own fact.
//!
//! A grant widens containment, so it is audited — but the audit relation it is
//! audited *into* has a contract: `memd_audit_event.reason` is a **neutral tag**
//! (frozen v4), a value from a closed vocabulary Rift itself writes. The reason
//! a person types for a grant is neither: it is untrusted free text arriving
//! from argv.
//!
//! So the two halves are stored in the two places their contracts allow. The
//! served event carries the fixed tag `cross-branch-grant` in `reason`, which is
//! what makes "which serves were widened" a query over the frozen relation. The
//! text lands here, in `serve_grant_event`, bounded and joined back by
//! `audit_id` — **stored and never re-served**. Nothing reads a `reason_text`
//! back into a response; the relation exists so a human auditing the ledger can
//! read why, not so a serve can quote it.

use rift_core::Result;
use rift_store::{CanonicalStore, Value};

use crate::graph::sha256_id;

/// The neutral tag a widened serve carries in the frozen audit relation. A
/// fixed constant, not a rendering of anything the caller supplied — which is
/// exactly what keeps a neutral-tag column neutral.
pub const CROSS_BRANCH_GRANT_TAG: &str = "cross-branch-grant";

/// The grant axis this writer records for a cross-branch grant.
pub const GRANT_KIND_BRANCHES: &str = "branches";

/// The most reason text one grant row stores. Enforced **here**, at the writer,
/// because this is the last place before an append-only table nothing can
/// correct — a surface that also refuses longer input earlier is welcome to,
/// but the bound cannot depend on it having done so.
pub const MAX_GRANT_REASON_BYTES: usize = 200;

/// The stored form of a grant reason: trimmed, and cut to
/// [`MAX_GRANT_REASON_BYTES`] on a character boundary.
#[must_use]
pub fn bounded_reason(reason: &str) -> &str {
    let reason = reason.trim();
    if reason.len() <= MAX_GRANT_REASON_BYTES {
        return reason;
    }
    let mut end = MAX_GRANT_REASON_BYTES;
    while end > 0 && !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason.get(..end).unwrap_or("")
}

/// Record the free text behind one cross-scope grant, keyed to the serve it
/// widened. Returns the content-addressed `grant_id`.
///
/// Append-only and idempotent: the id is derived from `(audit_id, kind, text)`,
/// so re-delivering one grant is one row.
///
/// # Errors
/// Propagates store write errors.
pub async fn record_serve_grant(
    store: &impl CanonicalStore,
    audit_id: &str,
    kind: &str,
    reason_text: &str,
    recorded_at: i64,
) -> Result<String> {
    let text = bounded_reason(reason_text);
    let grant_id = sha256_id(
        "rift.grant.v1",
        &[audit_id.as_bytes(), kind.as_bytes(), text.as_bytes()],
    );
    store
        .insert_or_ignore(
            "serve_grant_event",
            &[
                "grant_id",
                "audit_id",
                "kind",
                "reason_text",
                "recorded_at",
                "schema_version",
            ],
            &[
                Value::text(grant_id.as_str()),
                Value::text(audit_id),
                Value::text(kind),
                Value::text(text),
                Value::Int(recorded_at),
                Value::Int(1),
            ],
        )
        .await?;
    Ok(grant_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_store::{recall_migrations, SqliteStore};

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(f)
    }

    fn recall_store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&s, &recall_migrations()).unwrap();
        s
    }

    /// The reason is bounded at the writer, on a character boundary — a cut
    /// through a multi-byte character would store bytes that are not text.
    #[test]
    fn a_reason_is_bounded_at_the_writer_on_a_character_boundary() {
        assert_eq!(bounded_reason("  review  "), "review");
        let wide = "é".repeat(200); // 400 bytes
        let cut = bounded_reason(&wide);
        assert!(cut.len() <= MAX_GRANT_REASON_BYTES);
        assert_eq!(
            cut.chars().count(),
            100,
            "cut between characters, not through one"
        );
        assert!(std::str::from_utf8(cut.as_bytes()).is_ok());
    }

    /// One grant is one row however many times it is delivered, and the text is
    /// joined back to the serve it widened.
    #[test]
    fn a_grant_is_one_row_keyed_to_the_serve_it_widened() {
        let store = recall_store();
        let id = block(record_serve_grant(
            &store,
            "audit-1",
            GRANT_KIND_BRANCHES,
            "reviewing the sibling",
            42,
        ))
        .unwrap();
        block(record_serve_grant(
            &store,
            "audit-1",
            GRANT_KIND_BRANCHES,
            "reviewing the sibling",
            99,
        ))
        .unwrap();

        let rows = block(store.query(
            "SELECT grant_id, audit_id, kind, reason_text, recorded_at FROM serve_grant_event",
            &[],
        ))
        .unwrap();
        assert_eq!(rows.len(), 1, "a re-delivered grant is one row");
        assert_eq!(rows[0].get_text(0).unwrap(), id);
        assert_eq!(rows[0].get_text(1).unwrap(), "audit-1");
        assert_eq!(rows[0].get_text(2).unwrap(), "branches");
        assert_eq!(rows[0].get_text(3).unwrap(), "reviewing the sibling");
        assert_eq!(rows[0].get_i64(4).unwrap(), 42);
    }
}
