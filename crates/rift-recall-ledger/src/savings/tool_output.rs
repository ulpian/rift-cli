//! The tool-output classes over one window: results the hook held back from
//! a repeat, and results it folded, each read from the audit relation by its
//! own verb.
//!
//! An observing row books no token figure, so it is counted and never summed
//! into anything: it is what the class *would* save. An acting row books what
//! it avoided, and that figure is already inside the share's numerator, which
//! is the audit relation's whole total; this read only lets a surface say
//! which part of the share the class is. Summing an observed figure into the
//! share would publish a saving nobody made.

use rift_core::Result;
use rift_store::{CanonicalStore, Value};

/// The held class's verb.
pub const VERB_TOOL_OUTPUT_HELD: &str = "memd.tool_output_held";
/// The folded class's verb.
pub const VERB_TOOL_OUTPUT_FOLDED: &str = "memd.tool_output_folded";

/// One tool-output class's counts and figures over one window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ToolOutputClass {
    /// Rows that only observed: the class would have acted, and did not.
    pub observed: u64,
    /// The lines the observed rows' handles name.
    pub observed_lines: u64,
    /// Rows that acted.
    pub acted: u64,
    /// What the acting rows avoided; already inside the share.
    pub avoided: u64,
    /// What the acting rows returned in place of the output.
    pub returned: u64,
}

/// Both tool-output classes over one window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ToolOutputTotals {
    /// Results held back from a repeat.
    pub held: ToolOutputClass,
    /// Results folded to their shape.
    pub folded: ToolOutputClass,
}

/// The two tool-output classes from the audit relation, over the window
/// starting at `since_ms` (the whole record when `None`).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn tool_output_totals(
    recall: &impl CanonicalStore,
    since_ms: Option<i64>,
) -> Result<ToolOutputTotals> {
    let (clause, mut params) = since_ms.map_or_else(
        || (String::new(), Vec::new()),
        |ms| (" AND recorded_at >= ?3".to_owned(), vec![Value::Int(ms)]),
    );
    params.splice(
        0..0,
        [
            Value::text(VERB_TOOL_OUTPUT_HELD),
            Value::text(VERB_TOOL_OUTPUT_FOLDED),
        ],
    );
    let rows = recall
        .query(
            &format!(
                "SELECT verb, handle, tokens_avoided, tokens_returned \
                 FROM memd_audit_event WHERE verb IN (?1, ?2){clause}"
            ),
            &params,
        )
        .await?;
    let mut out = ToolOutputTotals::default();
    for r in &rows {
        let n = |i: usize| u64::try_from(r.get_i64(i).unwrap_or(0)).unwrap_or(0);
        let class = match r.get_text(0)?.as_str() {
            VERB_TOOL_OUTPUT_HELD => &mut out.held,
            _ => &mut out.folded,
        };
        let avoided = n(2);
        if avoided == 0 {
            let lines = r.get_opt_text(1)?.as_deref().map_or(0, handle_lines);
            class.observed = class.observed.saturating_add(1);
            class.observed_lines = class.observed_lines.saturating_add(lines);
            continue;
        }
        class.acted = class.acted.saturating_add(1);
        class.avoided = class.avoided.saturating_add(avoided);
        class.returned = class.returned.saturating_add(n(3));
    }
    Ok(out)
}

/// The lines a `…:L<lo>-<hi>` handle names, or zero for a handle that names
/// no range: an unparsable suffix is counted as nothing rather than guessed.
fn handle_lines(handle: &str) -> u64 {
    let Some((_, range)) = handle.rsplit_once(":L") else {
        return 0;
    };
    let Some((lo, hi)) = range.split_once('-') else {
        return 0;
    };
    match (lo.parse::<u64>(), hi.parse::<u64>()) {
        (Ok(lo), Ok(hi)) if hi >= lo => hi - lo + 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    fn stores() -> (rift_store::SqliteStore, rift_store::SqliteStore) {
        let recall = rift_store::SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&recall, &rift_store::recall_migrations()).unwrap();
        let projection = rift_store::SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&projection, &rift_store::default_migrations()).unwrap();
        (recall, projection)
    }

    async fn row(recall: &rift_store::SqliteStore, id: &str, verb: &str, avoided: i64, at: i64) {
        let handle = "file:src/a.rs@sha256:ab:L10-29";
        let columns = [
            "audit_id",
            "verb",
            "handle",
            "reason",
            "tokens_returned",
            "tokens_avoided",
            "recorded_at",
            "schema_version",
        ];
        let returned = if avoided > 0 { 40 } else { 0 };
        let values = [
            Value::text(id),
            Value::text(verb),
            Value::text(handle),
            Value::Null,
            Value::Int(returned),
            Value::Int(avoided),
            Value::Int(at),
            Value::Int(1),
        ];
        recall
            .insert("memd_audit_event", &columns, &values)
            .await
            .unwrap();
    }

    /// ★ An observing row is counted with the lines it names and moves no
    /// headline figure; an acting row is counted with what it avoided, and
    /// that figure is the headline's, so the share moves by exactly it.
    #[test]
    fn observed_rows_leave_the_headline_and_an_acted_row_moves_it() {
        block(async {
            let (recall, projection) = stores();
            let before = crate::savings::compose(&recall, &projection, None)
                .await
                .unwrap();
            row(&recall, "h1", VERB_TOOL_OUTPUT_HELD, 0, 1_000).await;
            row(&recall, "f1", VERB_TOOL_OUTPUT_FOLDED, 0, 1_000).await;
            let observed = crate::savings::compose(&recall, &projection, None)
                .await
                .unwrap();
            assert_eq!(observed.tokens_avoided, before.tokens_avoided);
            assert_eq!(observed.share(), before.share());
            row(&recall, "h2", VERB_TOOL_OUTPUT_HELD, 900, 3_000).await;
            let acted = crate::savings::compose(&recall, &projection, None)
                .await
                .unwrap();
            assert_eq!(acted.tokens_avoided, before.tokens_avoided + 900);
            let totals = tool_output_totals(&recall, None).await.unwrap();
            let held = ToolOutputClass {
                observed: 1,
                observed_lines: 20,
                acted: 1,
                avoided: 900,
                returned: 40,
            };
            assert_eq!(totals.held, held);
            assert_eq!((totals.folded.observed, totals.folded.acted), (1, 0));
            let late = tool_output_totals(&recall, Some(2_000)).await.unwrap();
            assert_eq!((late.held.observed, late.held.acted), (0, 1));
            assert_eq!(late.folded, ToolOutputClass::default());
        });
    }

    #[test]
    fn a_handle_without_a_line_range_names_no_lines() {
        assert_eq!(handle_lines("file:a.rs@sha256:ab:L3-3"), 1);
        assert_eq!(handle_lines("file:a.rs@sha256:ab"), 0);
        assert_eq!(handle_lines("file:a.rs@sha256:ab:L9-3"), 0);
    }
}
