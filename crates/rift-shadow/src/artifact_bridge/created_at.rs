//! The checkpoint's own creation time, read from the shard's header.
//!
//! Kept apart because the timestamp parser and its round-trip tests are the
//! one place the projection's `checkpoint.created_at` meets the writer's clock.

use rift_recall_types::checkpoint::CheckpointMetadata;

use super::CREATED_AT_UNKNOWN;

/// The checkpoint's own creation time, or [`CREATED_AT_UNKNOWN`].
///
/// Native shards record `created_at` (RFC3339 with millis) from the capture
/// event's own time — `condense` threads the hook's `now_ms` into
/// `metadata_json`, so the shard's claim about its own time is capture data,
/// never the clock of whoever happened to run the rebuild. Every shard
/// archived before that writer landed carries no time field, and for
/// those — and for any unparseable claim — this is the sentinel, which is the
/// honest answer.
pub(super) fn created_at_for(metadata: &CheckpointMetadata) -> i64 {
    metadata
        .created_at
        .as_deref()
        .and_then(parse_rfc3339_ms)
        .unwrap_or(CREATED_AT_UNKNOWN)
}

/// Parse the small RFC3339 subset a capture tool actually writes
/// (`YYYY-MM-DDTHH:MM:SS[.fff…]Z`) into epoch ms. `None` on anything else — a
/// half-understood timestamp is worse than an admitted unknown.
///
/// The fraction is READ, not skipped (truncated past millis, the
/// `rift-recall-substrate` parser's semantics), because the native writer now
/// emits `.mmm` ([`crate::backend`]'s `rfc3339_utc_ms`) and a parser that
/// dropped it would floor every native checkpoint to the second — exactly the
/// precision the squash tie-break needs when a rewrite replays several
/// checkpoints inside one second. For the same reason, bytes between the
/// seconds field and the `Z` that are NOT a digit fraction now refuse instead
/// of being silently ignored: the earlier scan accepted
/// `…T03:04:05GARBAGEZ` as a whole-second instant, which is the
/// half-understood shape this function's own contract forbids.
fn parse_rfc3339_ms(text: &str) -> Option<i64> {
    let at = |i: usize| -> Option<u8> { text.as_bytes().get(i).copied() };
    if text.len() < 20
        || at(4) != Some(b'-')
        || at(7) != Some(b'-')
        || at(10) != Some(b'T')
        || !text.ends_with('Z')
    {
        return None;
    }
    let millis = match at(19) {
        Some(b'Z') if text.len() == 20 => 0,
        Some(b'.') => {
            let digits = text.get(20..text.len().checked_sub(1)?)?;
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            digits
                .bytes()
                .take(3)
                .fold(0i64, |ms, b| ms * 10 + i64::from(b - b'0'))
                * match digits.len() {
                    1 => 100,
                    2 => 10,
                    _ => 1,
                }
        }
        _ => return None,
    };
    let num = |a: usize, z: usize| -> Option<i64> { text.get(a..z)?.parse::<i64>().ok() };
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // Days from civil (Howard Hinnant's algorithm) over the proleptic Gregorian
    // calendar — one field does not justify a date dependency.
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = if shifted >= 0 { shifted } else { shifted - 399 } / 400;
    let year_of_era = shifted - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some((days * 86_400 + hour * 3600 + minute * 60 + second) * 1000 + millis)
}

#[cfg(test)]
mod chain_source_tests {
    use super::*;

    #[test]
    fn an_unknown_time_is_an_impossible_time_not_the_epoch() {
        // The whole point: `0` is a real instant, so it cannot mean "unknown".
        assert_eq!(CREATED_AT_UNKNOWN, -1);
        // A capture time is epoch-ms from a real clock, so it is non-negative;
        // the sentinel is outside that range by construction, not by luck.
        assert!(
            !(0..=i64::MAX).contains(&CREATED_AT_UNKNOWN),
            "the sentinel must be unreachable by any real capture time"
        );
        let none = CheckpointMetadata::default();
        assert_eq!(created_at_for(&none), CREATED_AT_UNKNOWN);
    }

    #[test]
    fn a_shards_own_time_beats_the_clock_of_whoever_ran_the_rebuild() {
        let m = CheckpointMetadata {
            created_at: Some("2026-06-01T00:00:00Z".into()),
            ..CheckpointMetadata::default()
        };
        assert_eq!(created_at_for(&m), 1_780_272_000_000);
        // A round-trip anchor that is not the same arithmetic as the parser.
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_ms("2000-01-01T00:00:00Z"),
            Some(946_684_800_000)
        );
    }

    #[test]
    fn a_half_understood_timestamp_is_refused_rather_than_guessed() {
        for bad in [
            "",
            "2026-06-01",
            "2026-06-01T00:00:00",  // no zone
            "2026-13-01T00:00:00Z", // month 13
            "2026-06-32T00:00:00Z", // day 32
            "2026-06-01T24:00:00Z", // hour 24
            "not-a-timestamp-at-all!!!",
            // Shapes the pre-fraction scan half-understood: it checked five
            // positions and `ends_with('Z')`, so everything between the
            // seconds field and the `Z` was silently ignored. Now they refuse.
            "2026-06-01T00:00:05GARBAGEZ",
            "2026-06-01T00:00:05.Z",     // a dot with no digits
            "2026-06-01T00:00:05.12a3Z", // a non-digit inside the fraction
        ] {
            assert_eq!(parse_rfc3339_ms(bad), None, "{bad}");
            let m = CheckpointMetadata {
                created_at: Some(bad.to_owned()),
                ..CheckpointMetadata::default()
            };
            assert_eq!(created_at_for(&m), CREATED_AT_UNKNOWN, "{bad}");
        }
    }

    #[test]
    fn the_fraction_is_read_not_skipped() {
        // The native writer emits `.mmm`; a parser that dropped it would floor
        // every native checkpoint to the second — losing exactly the
        // resolution the squash tie-break needs when a rewrite replays several
        // checkpoints inside one second.
        let base = 1_780_272_000_000; // 2026-06-01T00:00:00Z, pinned above
        assert_eq!(
            parse_rfc3339_ms("2026-06-01T00:00:00.123Z"),
            Some(base + 123)
        );
        // Short fractions scale, long ones truncate past millis — the
        // `rift-recall-substrate` parser's semantics, kept identical so the
        // two lenient views of one field can never disagree on a value.
        assert_eq!(parse_rfc3339_ms("2026-06-01T00:00:00.1Z"), Some(base + 100));
        assert_eq!(
            parse_rfc3339_ms("2026-06-01T00:00:00.12Z"),
            Some(base + 120)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-06-01T00:00:00.1259Z"),
            Some(base + 125)
        );
    }

    #[test]
    fn the_writer_and_the_reader_agree_to_the_millisecond() {
        // `metadata.json`'s `created_at` is written by `rfc3339_utc_ms` and
        // read back by `parse_rfc3339_ms`; the projection's
        // `checkpoint.created_at` equals the capture event's own recorded time
        // exactly when these two are inverses. Swept over instants chosen to
        // hit every carry: epoch+1ms, a leap-day, both edges of a millisecond
        // decade, end-of-year, and the fixture clocks the suites already pin.
        for ms in [
            1,
            999,
            1_000,
            86_399_999,        // 1970-01-01T23:59:59.999Z
            951_782_400_000,   // 2000-02-29T00:00:00Z (leap day)
            1_700_000_000_000, // the parity/backends fixture clock
            1_756_000_000_000, // FIXTURE_CREATED_AT_MS
            1_780_272_000_000, // 2026-06-01T00:00:00Z
            1_767_225_599_999, // 2025-12-31T23:59:59.999Z
            4_102_444_800_000, // 2100-01-01T00:00:00Z (century, non-leap)
        ] {
            let text = crate::backend::rfc3339_utc_ms(ms);
            assert_eq!(
                parse_rfc3339_ms(&text),
                Some(ms),
                "round-trip broke at {ms} (rendered {text})"
            );
        }
        // …and the rendering itself is pinned once, so a formatting drift is a
        // visible diff and not just a still-round-tripping mystery.
        assert_eq!(
            crate::backend::rfc3339_utc_ms(1_700_000_000_000),
            "2023-11-14T22:13:20.000Z"
        );
    }
}
