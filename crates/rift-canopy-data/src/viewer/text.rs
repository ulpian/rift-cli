//! Display primitives every card shares: bounded, terminal-safe text, the
//! compact UTC timestamp, and the fact and warning line builders.

use rift_canopy::{ViewerLine, ViewerLineKind, ViewerTimelineItem};
use rift_recall_substrate::parse;

const MAX_DISPLAY_CHARS: usize = 4_096;

pub(super) fn bounded(input: &str) -> String {
    parse::sanitize_terminal_text(input)
        .chars()
        .take(MAX_DISPLAY_CHARS)
        .collect()
}

pub(super) fn short(value: &str) -> String {
    value.chars().take(12).collect()
}

pub(super) fn compact_utc(ms: i64) -> Option<String> {
    if ms < 0 {
        return None;
    }
    let seconds = ms / 1_000;
    let days = seconds / 86_400;
    let day_seconds = seconds % 86_400;
    let (year, month, day) = civil_from_days(days)?;
    let hour = day_seconds / 3_600;
    let minute = day_seconds % 3_600 / 60;
    Some(format!(
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}Z"
    ))
}

/// A comma-joined list, or the given wording when there is nothing to
/// list — an empty field reads as a fact about absence, never as blank.
pub(super) fn list_or(values: &[String], empty: &str) -> String {
    if values.is_empty() {
        return empty.to_owned();
    }
    values.join(", ")
}

pub(super) fn fact_line(label: &str, value: &str) -> ViewerLine {
    ViewerLine::plain(
        ViewerLineKind::Text,
        format!("{}: {}", bounded(label), bounded(value)),
    )
}

pub(super) fn warning_item(id: &str, title: &str, text: &str) -> ViewerTimelineItem {
    ViewerTimelineItem {
        id: id.to_owned(),
        title: title.to_owned(),
        timestamp: None,
        summary: vec![ViewerLine::plain(ViewerLineKind::Warning, bounded(text))],
        body: Vec::new(),
        expanded_body: None,
        collapsed: false,
        full_session_only: false,
        tone: rift_canopy::ViewerItemTone::Neutral,
    }
}

fn civil_from_days(days: i64) -> Option<(i64, i64, i64)> {
    let shifted = days.checked_add(719_468)?;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted.checked_sub(146_096)?
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    Some((year, month, day))
}

#[cfg(test)]
mod tests {
    use rift_store::CREATED_AT_UNKNOWN;

    use super::*;

    #[test]
    fn compact_date_rejects_unknown_and_formats_utc() {
        assert_eq!(compact_utc(CREATED_AT_UNKNOWN), None);
        assert_eq!(compact_utc(0).as_deref(), Some("1970-01-01 00:00Z"));
    }

    #[test]
    fn civil_dates_hold_at_the_epoch_shift_and_refuse_overflow() {
        assert!(civil_from_days(-719_468).is_some());
        assert!(civil_from_days(-719_469).is_some());
        assert!(civil_from_days(i64::MAX).is_none());
        assert!(compact_utc(i64::MAX).is_some());
    }

    #[test]
    fn an_empty_list_reads_as_its_absence_wording() {
        assert_eq!(list_or(&[], "none recorded"), "none recorded");
        assert_eq!(
            list_or(&["a".to_owned(), "b".to_owned()], "none recorded"),
            "a, b"
        );
    }
}
