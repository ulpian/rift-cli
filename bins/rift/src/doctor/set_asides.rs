//! The recall projections `rift-memd rebuild` set aside, listed beside the
//! live one.
//!
//! A rebuild renames the projection it replaces rather than delete it, since
//! the serves no checkpoint carries exist only there. The renamed files are
//! records, not run state: nothing collects them, so a person has to be able
//! to find them, see what they cost on disk, and learn what reads them back.
//! A module of its own because it reads a name format another binary writes.

use std::path::Path;

use crate::family;

/// What follows the projection's own file name in a set-aside sibling's.
///
/// `rift-memd`'s rebuild writes the same infix
/// (`bins/rift-memd/src/set_aside.rs`); the two binaries share no code, so
/// it is written in both.
const SET_ASIDE_INFIX: &str = ".set-aside-";

/// One set-aside projection.
struct SetAside {
    /// The file's name, beside the live projection.
    name: String,
    /// The projection file's size.
    bytes: u64,
    /// When the rebuild set it aside, read off the name's stamp.
    at_ms: Option<i64>,
}

/// The set-aside recall projections beside the live one, by name, size and
/// age. Read-only: no store is opened and nothing is removed.
pub(super) fn doctor_recall_set_asides(repo: &Path) -> (String, serde_json::Value) {
    let Ok(db) = family::recall_db(repo) else {
        return (String::new(), serde_json::json!({ "surveyed": false }));
    };
    set_asides_report(&db, crate::hooks::now_ms())
}

/// The leg's line and object for the projection at `db`, aged against `now_ms`.
fn set_asides_report(db: &Path, now_ms: i64) -> (String, serde_json::Value) {
    let found = set_asides_beside(db);
    if found.is_empty() {
        return (
            "\n  recall set-asides: none".to_owned(),
            serde_json::json!({ "surveyed": true, "set_asides": [] }),
        );
    }
    let mut human = format!(
        "\n  recall set-asides: {} beside {} — records, which `rift clean` never \
         removes; `rift backfill --recall-from <file>` restores the serves one holds",
        found.len(),
        db.display(),
    );
    for aside in &found {
        human.push_str(&format!(
            "\n    {}  {} byte(s)  {}",
            aside.name,
            aside.bytes,
            age_phrase(aside.at_ms, now_ms),
        ));
    }
    let listed: Vec<serde_json::Value> = found
        .iter()
        .map(|aside| {
            serde_json::json!({
                "name": aside.name,
                "bytes": aside.bytes,
                "set_aside_at_ms": aside.at_ms,
            })
        })
        .collect();
    (
        human,
        serde_json::json!({ "surveyed": true, "set_asides": listed }),
    )
}

/// The set-aside projection files beside `db`, oldest name first; their
/// `-wal`/`-shm` sidecars belong to them and are not listed apart.
fn set_asides_beside(db: &Path) -> Vec<SetAside> {
    let (Some(dir), Some(live)) = (db.parent(), db.file_name()) else {
        return Vec::new();
    };
    let prefix = format!("{}{SET_ASIDE_INFIX}", live.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<SetAside> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter_map(|path| set_aside_at(&path, &prefix))
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// The set-aside projection at `path`, when its name is one.
fn set_aside_at(path: &Path, prefix: &str) -> Option<SetAside> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let stamp = name.strip_prefix(prefix)?;
    if stamp.ends_with("-wal") || stamp.ends_with("-shm") {
        return None;
    }
    let bytes = std::fs::metadata(path).ok()?.len();
    let stamp = stamp.split_once('.').map_or(stamp, |(stamp, _)| stamp);
    Some(SetAside {
        at_ms: stamp_millis(stamp),
        name,
        bytes,
    })
}

/// How long ago a projection was set aside, in the largest whole unit.
fn age_phrase(set_aside_at_ms: Option<i64>, now_ms: i64) -> String {
    let Some(at) = set_aside_at_ms else {
        return "set aside at an unknown time".to_owned();
    };
    let minutes = (now_ms - at).max(0) / 60_000;
    match minutes {
        0 => "set aside under a minute ago".to_owned(),
        1..=59 => format!("set aside {minutes} minute(s) ago"),
        60..=1439 => format!("set aside {} hour(s) ago", minutes / 60),
        _ => format!("set aside {} day(s) ago", minutes / 1440),
    }
}

/// The instant a `YYYYMMDDTHHMMSSZ` stamp names, in epoch milliseconds.
fn stamp_millis(stamp: &str) -> Option<i64> {
    let digits = |range: std::ops::Range<usize>| -> Option<i64> {
        let text = stamp.get(range)?;
        if !text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        text.parse().ok()
    };
    if stamp.len() != 16 || stamp.get(8..9) != Some("T") || stamp.get(15..) != Some("Z") {
        return None;
    }
    let (year, month, day) = (digits(0..4)?, digits(4..6)?, digits(6..8)?);
    let (hour, minute, second) = (digits(9..11)?, digits(11..13)?, digits(13..15)?);
    let days = days_from_civil(year, month, day);
    Some(((days * 86_400) + hour * 3600 + minute * 60 + second) * 1000)
}

/// Days from 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`), so reading a stamp needs no date library.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-01T12:00:00Z.
    const NOON: i64 = 1_790_856_000_000;

    #[test]
    fn a_stamp_reads_back_as_the_instant_it_names() {
        assert_eq!(stamp_millis("20261001T120000Z"), Some(NOON));
        assert_eq!(stamp_millis("20000229T235959Z"), Some(951_868_799_000));
        assert_eq!(stamp_millis("19700101T000000Z"), Some(0));
        for not_a_stamp in ["2026-10-01T12:00Z", "20261001T120000", "2026100XT120000Z"] {
            assert_eq!(stamp_millis(not_a_stamp), None, "{not_a_stamp}");
        }
    }

    #[test]
    fn the_doctor_lists_each_set_aside_by_name_size_and_age_and_says_what_restores_it() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("recall.db");
        std::fs::write(&db, b"live").unwrap();
        for (name, body) in [
            (
                "recall.db.set-aside-20260928T120000Z",
                "three days".as_bytes(),
            ),
            ("recall.db.set-aside-20260928T120000Z-wal", b"its journal"),
            ("recall.db.set-aside-20261001T090000Z.2", b"hours"),
            (
                "projection.db.set-aside-20261001T090000Z",
                b"another projection",
            ),
        ] {
            std::fs::write(tmp.path().join(name), body).unwrap();
        }

        let (human, json) = set_asides_report(&db, NOON);

        assert!(human.contains("recall set-asides: 2 beside"), "{human}");
        assert!(human.contains("`rift clean` never removes"), "{human}");
        assert!(human.contains("rift backfill --recall-from"), "{human}");
        assert!(
            human.contains(
                "recall.db.set-aside-20260928T120000Z  10 byte(s)  set aside 3 day(s) ago"
            ),
            "{human}"
        );
        assert!(
            human.contains(
                "recall.db.set-aside-20261001T090000Z.2  5 byte(s)  set aside 3 hour(s) ago"
            ),
            "{human}"
        );
        assert!(
            !human.contains("-wal"),
            "a sidecar is not a projection: {human}"
        );
        assert_eq!(json["set_asides"].as_array().unwrap().len(), 2);
        assert_eq!(
            json["set_asides"][1]["set_aside_at_ms"],
            NOON - 3 * 3_600_000
        );
    }

    #[test]
    fn no_set_aside_reads_as_none() {
        let tmp = tempfile::tempdir().unwrap();
        let (human, json) = set_asides_report(&tmp.path().join("recall.db"), NOON);
        assert_eq!(human, "\n  recall set-asides: none");
        assert_eq!(json["set_asides"], serde_json::json!([]));
    }
}
