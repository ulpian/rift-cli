//! Setting the recall projection aside before a rebuild re-derives it.
//!
//! A rebuild re-derives every row a checkpoint shard carries, and nothing
//! else. The rows no shard carries — a serve a person made at a terminal, a
//! serve booked before the shards froze serves at all — exist only in the
//! projection being replaced. So the rebuild renames that projection to a
//! dated sibling instead of deleting it, and `rift backfill --recall-from`
//! reads the sibling back. A module of its own because the rename takes the
//! clock, and the clock is what a test injects.
//!
//! The name is `<db>.set-aside-<YYYYMMDDTHHMMSSZ>`, read by a person and
//! sorted by `ls`. A sidecar keeps its SQLite suffix *after* the new name
//! (`<db>.set-aside-<stamp>-wal`), so SQLite pairs the set-aside file with its
//! own journal when it is opened. The alternative the name was weighed
//! against, `<db>-wal.set-aside-<stamp>`, reads the same but leaves the journal
//! unpaired, and a reader of the set-aside file would then miss every page
//! still in it.
//!
//! The `rift` binary lists these files under the same spelling; neither binary
//! depends on the other, so the infix is written in both, and each says so.

use std::path::{Path, PathBuf};

use rift_core::{Result, RiftError, Timestamp};

/// What follows the projection's own file name in a set-aside sibling's.
///
/// `rift doctor` matches the same infix in `bins/rift/src/doctor/stores.rs`.
const SET_ASIDE_INFIX: &str = ".set-aside-";

/// The SQLite sidecars that travel with the projection.
const SIDECARS: [&str; 2] = ["-wal", "-shm"];

/// Rename the projection at `db`, and each sidecar beside it, to siblings
/// stamped with `now`, and return the projection's new path; `None` when
/// there was nothing to set aside.
///
/// One stamp names all three files, so they stay one projection on disk. A
/// name already taken (a second rebuild inside the same second) gains a
/// counter, `.2`, `.3`, …; a set-aside file is a record and is never
/// overwritten.
///
/// # Errors
/// [`RiftError::Storage`] when a rename fails; the rebuild stops there rather
/// than derive a new projection beside a half-moved one.
pub(crate) fn set_aside(db: &Path, now: Timestamp) -> Result<Option<PathBuf>> {
    let present: Vec<&str> = std::iter::once("")
        .chain(SIDECARS)
        .filter(|suffix| sibling(db, suffix).exists())
        .collect();
    if present.is_empty() {
        return Ok(None);
    }
    let target = free_name(db, &stamp(now));
    for suffix in present {
        let from = sibling(db, suffix);
        let to = sibling(&target, suffix);
        std::fs::rename(&from, &to).map_err(|e| {
            RiftError::storage(format!(
                "set aside {} as {}: {e}",
                from.display(),
                to.display()
            ))
        })?;
    }
    Ok(Some(target))
}

/// `db`'s set-aside name under `stamp`, with the first counter that names no
/// file of the three.
fn free_name(db: &Path, stamp: &str) -> PathBuf {
    let base = format!("{}{SET_ASIDE_INFIX}{stamp}", db.display());
    let taken = |candidate: &Path| {
        std::iter::once("")
            .chain(SIDECARS)
            .any(|suffix| sibling(candidate, suffix).exists())
    };
    let mut candidate = PathBuf::from(&base);
    let mut n = 2u32;
    while taken(&candidate) {
        candidate = PathBuf::from(format!("{base}.{n}"));
        n += 1;
    }
    candidate
}

/// `path` with `suffix` appended to its last component.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// `now` as `YYYYMMDDTHHMMSSZ` in UTC: compact, sortable, and read without a
/// tool.
fn stamp(now: Timestamp) -> String {
    let secs = now.as_millis().div_euclid(1000);
    let (days, of_day) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        of_day / 3600,
        of_day % 3600 / 60,
        of_day % 60
    )
}

/// The proleptic Gregorian date `days` after 1970-01-01 (Howard Hinnant's
/// `civil_from_days`), so the stamp needs no date library.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-01T12:00:00Z.
    const NOON: Timestamp = Timestamp::from_millis(1_790_856_000_000);

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn the_stamp_is_the_injected_instant_in_utc() {
        assert_eq!(stamp(NOON), "20261001T120000Z");
        assert_eq!(
            stamp(Timestamp::from_millis(951_868_799_000)),
            "20000229T235959Z"
        );
        assert_eq!(stamp(Timestamp::from_millis(0)), "19700101T000000Z");
    }

    #[test]
    fn the_projection_and_its_sidecars_move_under_one_stamp_with_their_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("recall.db");
        for (suffix, body) in [("", "main"), ("-wal", "wal"), ("-shm", "shm")] {
            std::fs::write(sibling(&db, suffix), body).unwrap();
        }

        let moved = set_aside(&db, NOON).unwrap().unwrap();

        assert_eq!(
            moved,
            tmp.path().join("recall.db.set-aside-20261001T120000Z")
        );
        assert_eq!(
            names_in(tmp.path()),
            [
                "recall.db.set-aside-20261001T120000Z",
                "recall.db.set-aside-20261001T120000Z-shm",
                "recall.db.set-aside-20261001T120000Z-wal",
            ]
        );
        assert_eq!(std::fs::read_to_string(&moved).unwrap(), "main");
        assert_eq!(
            std::fs::read_to_string(sibling(&moved, "-wal")).unwrap(),
            "wal"
        );
    }

    #[test]
    fn nothing_to_set_aside_moves_nothing_and_names_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("recall.db");
        assert_eq!(set_aside(&db, NOON).unwrap(), None);
        assert!(names_in(tmp.path()).is_empty());
    }

    #[test]
    fn a_taken_name_gains_a_counter_and_no_record_is_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("recall.db");
        let mut moved = Vec::new();
        for body in ["first", "second", "third"] {
            std::fs::write(&db, body).unwrap();
            moved.push(set_aside(&db, NOON).unwrap().unwrap());
        }
        let names: Vec<String> = moved
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            [
                "recall.db.set-aside-20261001T120000Z",
                "recall.db.set-aside-20261001T120000Z.2",
                "recall.db.set-aside-20261001T120000Z.3",
            ]
        );
        let bodies: Vec<String> = moved
            .iter()
            .map(|p| std::fs::read_to_string(p).unwrap())
            .collect();
        assert_eq!(bodies, ["first", "second", "third"]);
    }

    #[test]
    fn a_name_taken_only_by_a_sidecar_is_still_taken() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("recall.db");
        std::fs::write(
            tmp.path().join("recall.db.set-aside-20261001T120000Z-wal"),
            "older",
        )
        .unwrap();
        std::fs::write(&db, "live").unwrap();
        let moved = set_aside(&db, NOON).unwrap().unwrap();
        assert!(moved.ends_with("recall.db.set-aside-20261001T120000Z.2"));
    }
}
