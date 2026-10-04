//! Setting the recall projection aside before a rebuild re-derives it.
//!
//! A rebuild re-derives every row a checkpoint shard carries, and nothing
//! else. The rows no shard carries — a serve a person made at a terminal, a
//! serve booked before the shards froze serves at all — exist only in the
//! projection being replaced. So the rebuild preserves that projection as a
//! dated sibling instead of deleting it, and `rift backfill --recall-from`
//! reads the sibling back. A module of its own because preservation takes the
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

trait FileOperations {
    fn hard_link(&self, source: &Path, target: &Path) -> std::io::Result<()>;
    fn remove_file(&self, path: &Path) -> std::io::Result<()>;
}

struct LocalFiles;

impl FileOperations for LocalFiles {
    fn hard_link(&self, source: &Path, target: &Path) -> std::io::Result<()> {
        std::fs::hard_link(source, target)
    }

    fn remove_file(&self, path: &Path) -> std::io::Result<()> {
        std::fs::remove_file(path)
    }
}

/// Preserve a complete DB/WAL/SHM set before removing any original path.
/// Exclusive ownership stays with the caller through opening and ingesting
/// the replacement, so no writer can commit into the preserved projection.
///
/// # Errors
/// A failed recovery names the complete preserved projection; rebuilding
/// stops rather than proceeding beside a partially restored source.
pub(crate) fn set_aside(
    guard: &rift_store::ProjectionReplacementGuard,
    now: Timestamp,
) -> Result<Option<PathBuf>> {
    set_aside_with(guard, now, &LocalFiles)
}

fn set_aside_with(
    guard: &rift_store::ProjectionReplacementGuard,
    now: Timestamp,
    files: &impl FileOperations,
) -> Result<Option<PathBuf>> {
    let db = guard.path();
    let present: Vec<&str> = std::iter::once("")
        .chain(SIDECARS)
        .filter(|suffix| sibling(db, suffix).exists())
        .collect();
    if present.is_empty() {
        return Ok(None);
    }
    if !present.contains(&"") {
        return Err(RiftError::storage(format!("projection {} has orphan SQLite sidecars; preserve them and repair the source before rebuilding", db.display())));
    }
    let base = format!("{}{SET_ASIDE_INFIX}{}", db.display(), stamp(now));
    let mut counter = 1_u32;
    loop {
        let target = if counter == 1 {
            PathBuf::from(&base)
        } else {
            PathBuf::from(format!("{base}.{counter}"))
        };
        if link_complete_copy(db, &target, &present, files)? {
            remove_sources(db, &target, &present, files)?;
            return Ok(Some(target));
        }
        counter = counter
            .checked_add(1)
            .ok_or_else(|| RiftError::storage("set-aside name counter exhausted"))?;
    }
}

fn link_complete_copy(
    db: &Path,
    target: &Path,
    present: &[&str],
    files: &impl FileOperations,
) -> Result<bool> {
    if std::iter::once("")
        .chain(SIDECARS)
        .any(|suffix| sibling(target, suffix).symlink_metadata().is_ok())
    {
        return Ok(false);
    }
    let mut linked = Vec::new();
    for suffix in present {
        match files.hard_link(&sibling(db, suffix), &sibling(target, suffix)) {
            Ok(()) => linked.push(*suffix),
            Err(error) => {
                cleanup_links(target, &linked, files)?;
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    return Ok(false);
                }
                return Err(RiftError::storage(format!(
                    "preserve {} as {}: {error}; original projection is unchanged",
                    db.display(),
                    target.display()
                )));
            }
        }
    }
    // A sidecar absent from the source must not appear beside the copy
    // while its DB was being linked. It could be another record's WAL.
    if std::iter::once("").chain(SIDECARS).any(|suffix| {
        !present.contains(&suffix) && sibling(target, suffix).symlink_metadata().is_ok()
    }) {
        cleanup_links(target, &linked, files)?;
        return Ok(false);
    }
    Ok(true)
}

fn cleanup_links(target: &Path, linked: &[&str], files: &impl FileOperations) -> Result<()> {
    for suffix in linked {
        files.remove_file(&sibling(target, suffix)).map_err(|error| RiftError::storage(format!(
            "original projection remains intact; remove an incomplete set-aside link at {} after inspecting it: {error}", sibling(target, suffix).display()
        )))?;
    }
    Ok(())
}

fn remove_sources(
    db: &Path,
    target: &Path,
    present: &[&str],
    files: &impl FileOperations,
) -> Result<()> {
    let mut removed = Vec::new();
    for suffix in present {
        if let Err(error) = files.remove_file(&sibling(db, suffix)) {
            return restore_sources(db, target, present, &removed, files, &error);
        }
        removed.push(*suffix);
    }
    Ok(())
}

fn restore_sources(
    db: &Path,
    target: &Path,
    present: &[&str],
    removed: &[&str],
    files: &impl FileOperations,
    unlink_error: &std::io::Error,
) -> Result<()> {
    let mut failed = false;
    for suffix in removed {
        if files
            .hard_link(&sibling(target, suffix), &sibling(db, suffix))
            .is_err()
        {
            failed = true;
        }
    }
    if failed {
        return Err(RiftError::storage(format!(
            "could not restore {} after unlink failure ({unlink_error}); complete projection preserved at {}; restore the complete original from the preserved triple, or quarantine its incomplete paths and recreate a distinct live projection before recovery with rift backfill --recall-from {} before retrying", db.display(), target.display(), target.display()
        )));
    }
    cleanup_links(target, present, files)?;
    Err(RiftError::storage(format!("could not unlink {}: {unlink_error}; original projection restored, retry after fixing the filesystem error", db.display())))
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

    fn set_aside(db: &Path, now: Timestamp) -> Result<Option<PathBuf>> {
        let guard = rift_store::ProjectionReplacementGuard::acquire(db)?;
        super::set_aside(&guard, now)
    }

    #[tokio::test]
    async fn an_active_wal_writer_excludes_setting_the_projection_aside() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("recall.db");
        let store = rift_store::SqliteStore::open(&db).unwrap();
        let conn = rift_store::Store::conn(&store).await.unwrap();
        conn.execute_batch("PRAGMA journal_mode = WAL; CREATE TABLE lease_probe (value INTEGER)")
            .await
            .unwrap();
        assert!(matches!(
            set_aside(&db, NOON),
            Err(RiftError::ProjectionBusy { .. })
        ));
        conn.execute("INSERT INTO lease_probe VALUES (7)", &[])
            .await
            .unwrap();
        assert!(db.is_file());
        let reader = rift_store::SqliteStore::open_read_only(&db).unwrap();
        assert_eq!(
            rift_store::CanonicalStore::query(&reader, "SELECT value FROM lease_probe", &[])
                .await
                .unwrap()[0]
                .get_i64(0)
                .unwrap(),
            7
        );
        drop(reader);
        drop(conn);
        drop(store);
        assert!(set_aside(&db, NOON).unwrap().unwrap().is_file());
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
            tmp.path()
                .canonicalize()
                .unwrap()
                .join("recall.db.set-aside-20261001T120000Z")
        );
        assert_eq!(
            names_in(tmp.path()),
            [
                "recall.db.lock",
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
        assert_eq!(names_in(tmp.path()), ["recall.db.lock"]);
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
    struct FaultFiles {
        link_calls: std::cell::Cell<usize>,
        remove_calls: std::cell::Cell<usize>,
        failed_links: Vec<usize>,
        failed_removes: Vec<usize>,
    }

    impl FaultFiles {
        fn new(failed_links: Vec<usize>, failed_removes: Vec<usize>) -> Self {
            Self {
                link_calls: std::cell::Cell::new(0),
                remove_calls: std::cell::Cell::new(0),
                failed_links,
                failed_removes,
            }
        }
    }

    impl FileOperations for FaultFiles {
        fn hard_link(&self, source: &Path, target: &Path) -> std::io::Result<()> {
            let call = self.link_calls.get() + 1;
            self.link_calls.set(call);
            if self.failed_links.contains(&call) {
                return Err(std::io::Error::other("injected link failure"));
            }
            std::fs::hard_link(source, target)
        }

        fn remove_file(&self, path: &Path) -> std::io::Result<()> {
            let call = self.remove_calls.get() + 1;
            self.remove_calls.set(call);
            if self.failed_removes.contains(&call) {
                return Err(std::io::Error::other("injected unlink failure"));
            }
            std::fs::remove_file(path)
        }
    }

    fn source_triple(dir: &Path) -> PathBuf {
        let db = dir.join("recall.db");
        for (suffix, body) in [("", "db"), ("-wal", "wal"), ("-shm", "shm")] {
            std::fs::write(sibling(&db, suffix), body).unwrap();
        }
        db
    }

    fn assert_complete(db: &Path) {
        for (suffix, body) in [("", "db"), ("-wal", "wal"), ("-shm", "shm")] {
            assert_eq!(std::fs::read_to_string(sibling(db, suffix)).unwrap(), body);
        }
    }

    #[test]
    fn every_target_link_failure_leaves_the_complete_original_triple() {
        for failed_link in 1..=3 {
            let dir = tempfile::tempdir().unwrap();
            let db = source_triple(dir.path());
            let guard = rift_store::ProjectionReplacementGuard::acquire(&db).unwrap();
            let files = FaultFiles::new(vec![failed_link], Vec::new());
            assert!(set_aside_with(&guard, NOON, &files).is_err());
            drop(guard);
            assert_complete(&db);
            assert_eq!(names_in(dir.path()).len(), 4);
        }
    }

    #[test]
    fn every_source_unlink_failure_restores_the_complete_original_triple() {
        for failed_remove in 1..=3 {
            let dir = tempfile::tempdir().unwrap();
            let db = source_triple(dir.path());
            let guard = rift_store::ProjectionReplacementGuard::acquire(&db).unwrap();
            let files = FaultFiles::new(Vec::new(), vec![failed_remove]);
            assert!(set_aside_with(&guard, NOON, &files).is_err());
            drop(guard);
            assert_complete(&db);
            assert_eq!(names_in(dir.path()).len(), 4);
        }
    }

    #[test]
    fn every_failed_restore_keeps_the_complete_target_and_names_its_full_path() {
        for failed_remove in 2..=3 {
            for failed_restore in 1..failed_remove {
                let dir = tempfile::tempdir().unwrap();
                let db = source_triple(dir.path());
                let guard = rift_store::ProjectionReplacementGuard::acquire(&db).unwrap();
                let files = FaultFiles::new(vec![3 + failed_restore], vec![failed_remove]);
                let error = set_aside_with(&guard, NOON, &files)
                    .unwrap_err()
                    .to_string();
                let target = sibling(guard.path(), ".set-aside-20261001T120000Z");
                drop(guard);
                assert_complete(&target);
                assert!(error.contains(&target.display().to_string()), "{error}");
                assert!(error.contains("rift backfill --recall-from"), "{error}");
            }
        }
    }

    #[test]
    fn every_cleanup_failure_preserves_a_complete_original_and_an_actionable_error() {
        for failed_cleanup in 2..=4 {
            let dir = tempfile::tempdir().unwrap();
            let db = source_triple(dir.path());
            let guard = rift_store::ProjectionReplacementGuard::acquire(&db).unwrap();
            let files = FaultFiles::new(Vec::new(), vec![1, failed_cleanup]);
            let error = set_aside_with(&guard, NOON, &files)
                .unwrap_err()
                .to_string();
            drop(guard);
            assert_complete(&db);
            assert!(
                error.contains("original projection remains intact"),
                "{error}"
            );
            assert!(error.contains("set-aside-20261001T120000Z"), "{error}");
        }
    }

    #[test]
    fn every_partial_link_cleanup_failure_preserves_the_original_triple() {
        for failed_link in 2..=3 {
            for failed_cleanup in 1..failed_link {
                let dir = tempfile::tempdir().unwrap();
                let db = source_triple(dir.path());
                let guard = rift_store::ProjectionReplacementGuard::acquire(&db).unwrap();
                let files = FaultFiles::new(vec![failed_link], vec![failed_cleanup]);
                let error = set_aside_with(&guard, NOON, &files)
                    .unwrap_err()
                    .to_string();
                let source = guard.path().to_path_buf();
                drop(guard);
                assert_complete(&db);
                assert!(
                    error.contains("original projection remains intact"),
                    "{error}"
                );
                assert!(error.contains(&source.display().to_string()), "{error}");
            }
        }
    }

    struct LinkCollision {
        calls: std::cell::Cell<usize>,
        collide_at: usize,
    }

    impl FileOperations for LinkCollision {
        fn hard_link(&self, source: &Path, target: &Path) -> std::io::Result<()> {
            let call = self.calls.get() + 1;
            self.calls.set(call);
            if call == self.collide_at {
                std::fs::write(target, b"foreign")?;
            }
            std::fs::hard_link(source, target)
        }

        fn remove_file(&self, path: &Path) -> std::io::Result<()> {
            std::fs::remove_file(path)
        }
    }

    #[test]
    fn collisions_at_every_target_member_retry_without_overwriting_foreign_bytes() {
        for (call, suffix) in [(1, ""), (2, "-wal"), (3, "-shm")] {
            let dir = tempfile::tempdir().unwrap();
            let db = source_triple(dir.path());
            let guard = rift_store::ProjectionReplacementGuard::acquire(&db).unwrap();
            let files = LinkCollision {
                calls: std::cell::Cell::new(0),
                collide_at: call,
            };
            let target = set_aside_with(&guard, NOON, &files).unwrap().unwrap();
            assert!(target.ends_with("recall.db.set-aside-20261001T120000Z.2"));
            let foreign = sibling(
                guard.path(),
                &format!(".set-aside-20261001T120000Z{suffix}"),
            );
            drop(guard);
            assert_complete(&target);
            assert_eq!(std::fs::read(&foreign).unwrap(), b"foreign");
        }
    }

    struct RestoreCollision {
        source: PathBuf,
        removes: std::cell::Cell<usize>,
    }

    impl FileOperations for RestoreCollision {
        fn hard_link(&self, source: &Path, target: &Path) -> std::io::Result<()> {
            std::fs::hard_link(source, target)
        }

        fn remove_file(&self, path: &Path) -> std::io::Result<()> {
            let call = self.removes.get() + 1;
            self.removes.set(call);
            if call == 2 {
                std::fs::write(&self.source, b"foreign source")?;
                return Err(std::io::Error::other("injected source unlink failure"));
            }
            std::fs::remove_file(path)
        }
    }

    #[test]
    fn rollback_never_clobbers_a_new_source_and_retains_the_complete_preserved_triple() {
        let dir = tempfile::tempdir().unwrap();
        let db = source_triple(dir.path());
        let guard = rift_store::ProjectionReplacementGuard::acquire(&db).unwrap();
        let files = RestoreCollision {
            source: db.clone(),
            removes: std::cell::Cell::new(0),
        };
        let error = set_aside_with(&guard, NOON, &files)
            .unwrap_err()
            .to_string();
        let target = sibling(guard.path(), ".set-aside-20261001T120000Z");
        drop(guard);
        assert_complete(&target);
        assert_eq!(std::fs::read(&db).unwrap(), b"foreign source");
        assert!(error.contains(&target.display().to_string()), "{error}");
        assert!(error.contains("rift backfill --recall-from"), "{error}");
        assert_eq!(files.removes.get(), 2, "complete target is not cleaned up");
    }

    struct LateCollision {
        suffix: &'static str,
        created: std::cell::Cell<bool>,
    }

    impl FileOperations for LateCollision {
        fn hard_link(&self, source: &Path, target: &Path) -> std::io::Result<()> {
            let result = std::fs::hard_link(source, target);
            if !self.created.replace(true) {
                std::fs::write(sibling(target, self.suffix), b"foreign")?;
            }
            result
        }

        fn remove_file(&self, path: &Path) -> std::io::Result<()> {
            std::fs::remove_file(path)
        }
    }

    #[test]
    fn sidecars_absent_from_the_source_still_make_a_candidate_taken() {
        for suffix in SIDECARS {
            let dir = tempfile::tempdir().unwrap();
            let db = dir.path().join("recall.db");
            std::fs::write(&db, b"db").unwrap();
            let guard = rift_store::ProjectionReplacementGuard::acquire(&db).unwrap();
            let occupied = sibling(
                guard.path(),
                &format!(".set-aside-20261001T120000Z{suffix}"),
            );
            std::fs::write(&occupied, b"foreign").unwrap();
            let target = super::set_aside(&guard, NOON).unwrap().unwrap();
            drop(guard);
            assert!(target.ends_with("recall.db.set-aside-20261001T120000Z.2"));
            assert_eq!(std::fs::read(&occupied).unwrap(), b"foreign");
            assert_eq!(std::fs::read(&target).unwrap(), b"db");
        }
    }

    #[test]
    fn a_sidecar_created_while_its_source_member_is_absent_is_never_paired_with_the_copy() {
        for suffix in SIDECARS {
            let dir = tempfile::tempdir().unwrap();
            let db = dir.path().join("recall.db");
            std::fs::write(&db, b"db").unwrap();
            let guard = rift_store::ProjectionReplacementGuard::acquire(&db).unwrap();
            let files = LateCollision {
                suffix,
                created: std::cell::Cell::new(false),
            };
            let target = set_aside_with(&guard, NOON, &files).unwrap().unwrap();
            let foreign = sibling(
                guard.path(),
                &format!(".set-aside-20261001T120000Z{suffix}"),
            );
            drop(guard);
            assert_eq!(std::fs::read(&foreign).unwrap(), b"foreign");
            assert!(target.ends_with("recall.db.set-aside-20261001T120000Z.2"));
            assert_eq!(std::fs::read(&target).unwrap(), b"db");
        }
    }
}
