//! The spellings every block shares: how a label is padded, how a share is
//! written, and the one computation behind every share printed anywhere.

/// A share as the surfaces spell it, or the report's own word for a question
/// with no answer yet (`n/a`, as every other block here already writes it).
pub(crate) fn fmt_share(share: Option<f64>) -> String {
    share.map_or_else(|| "n/a".to_owned(), |s| format!("{s:.1}%"))
}

/// `avoided ÷ (avoided + read)` as a percentage, or nothing when the
/// denominator is empty: a share of nothing is unanswerable, not zero.
///
/// One computation, one place: the report, the `savings` verb, the recall
/// status table and the dashboard all reach the same function, so the figure
/// the owner judges cannot differ between the surface it is read on.
pub(super) fn share_of(avoided: u64, read: u64) -> Option<f64> {
    rift_recall_ledger::savings::share(avoided, read)
}

/// Left-pad a label to `width` COLUMNS, not bytes.
///
/// Some labels in these tables carry characters outside ASCII (`≥`, an en
/// dash), and `{:<n}` counts bytes: a table padded that way is crooked exactly
/// where the interesting rows are. Wide characters are not a case these tables
/// have, so counting characters is the whole of it.
pub(super) fn pad(label: &str, width: usize) -> String {
    let mut out = label.to_owned();
    for _ in label.chars().count()..width {
        out.push(' ');
    }
    out
}
