//! Lifetime exclusion for file-backed projections and their replacement.
//!
//! The stable marker survives replacement. Live opens refuse hardlink aliases;
//! Windows source leases also exclude foreign handles during replacement.
//! Locks are nonblocking so a status reader never waits behind a rebuild.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use rift_core::{Result, RiftError};

/// Exclusive ownership of one canonical projection path through replacement.
/// The owned path prevents a guard for one database from opening another.
pub struct ProjectionReplacementGuard {
    lease: ProjectionLease,
}

pub(crate) struct ProjectionLease {
    path: PathBuf,
    _marker: Option<File>,
    source: SourceLease,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SourceIdentity {
    volume: u64,
    file: u64,
}

struct SourceLease {
    identity: Option<SourceIdentity>,
    #[cfg(windows)]
    _file: Option<File>,
}

#[derive(Clone, Copy)]
enum SourceAccess {
    Shared,
    Replacement,
}

/// An explicitly selected historical projection for a consistent import.
/// This capability declares caller intent; it does not prove physical
/// immutability. Live family/destination identities must be refused before
/// opening SQLite, and imports must pin one read transaction across tables.
pub struct HistoricalProjectionSnapshot {
    path: PathBuf,
}

impl HistoricalProjectionSnapshot {
    /// Select an existing historical source for an import.
    /// The caller selects a preserved artifact, refuses live destinations,
    /// and keeps one read transaction across the complete import. READ_ONLY
    /// includes WAL pages; this never sets SQLite's `immutable=1` option.
    ///
    /// # Errors
    /// Returns filesystem errors for a missing or inaccessible source.
    pub fn user_selected(path: impl AsRef<Path>) -> Result<Self> {
        let path = std::fs::canonicalize(path)?;
        if !path.is_file() {
            return Err(RiftError::storage(
                "historical projection source is not a file",
            ));
        }
        Ok(Self { path })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Refuse importing a live destination, including filesystem aliases.
    ///
    /// # Errors
    /// Returns an error when source and destination share their identity.
    pub fn refuse_live_destination(&self, destination: &Path) -> Result<()> {
        let source = file_identity(&self.path)?;
        if source.is_some() && source == file_identity(destination)? {
            return Err(RiftError::storage("historical import source aliases a live projection; select a distinct historical snapshot"));
        }
        Ok(())
    }

    pub(crate) fn lease(&self) -> Result<Option<ProjectionLease>> {
        match existing_marker(&self.path)? {
            Some(marker) => {
                lock_shared(&marker, &self.path)?;
                let source = source_file(&self.path, SourceAccess::Shared)?;
                Ok(Some(ProjectionLease {
                    path: self.path.clone(),
                    _marker: Some(marker),
                    source,
                }))
            }
            None => Ok(None),
        }
    }
}

impl ProjectionReplacementGuard {
    /// Refuse active handles before any projection file is relocated.
    ///
    /// # Errors
    /// Busy, unsupported source aliases, and filesystem errors are returned
    /// without changing the source database or its sidecars.
    pub fn acquire(path: &Path) -> Result<Self> {
        Self::acquire_inspected(path, |_| ()).map(|(guard, ())| guard)
    }

    /// The canonical path every operation under this guard must use.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.lease.path
    }

    pub(crate) fn acquire_inspected<T>(
        path: &Path,
        inspect: impl FnOnce(&Path) -> T,
    ) -> Result<(Self, T)> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let canonical = canonical_path(path, 0)?;
        let marker = open_marker(&canonical)?;
        lock_exclusive(&marker, &canonical)?;
        // Windows' replacement lease denies READ from SQLite's separate
        // handle. Inspect while the stable marker is exclusive but before
        // that lease; then close SQLite and revalidate before any mutation.
        let inspected_source = source_file(&canonical, SourceAccess::Shared)?;
        if inspected_source.identity.is_none()
            && ["-wal", "-shm"].iter().any(|suffix| {
                let mut sidecar = canonical.as_os_str().to_owned();
                sidecar.push(suffix);
                Path::new(&sidecar).symlink_metadata().is_ok()
            })
        {
            return Err(RiftError::storage(
                "projection has orphan SQLite sidecars; preserve them before rebuilding",
            ));
        }
        let before = inspected_source.identity;
        let value = inspect(&canonical);
        #[cfg(windows)]
        drop(inspected_source);
        let source = source_file(&canonical, SourceAccess::Replacement)?;
        if canonical_path(path, 0)? != canonical || source.identity != before {
            return Err(RiftError::storage(
                "projection source changed during guarded inspection",
            ));
        }
        Ok((
            Self {
                lease: ProjectionLease {
                    path: canonical,
                    _marker: Some(marker),
                    source,
                },
            },
            value,
        ))
    }

    pub(crate) fn into_lease(mut self) -> Result<ProjectionLease> {
        // A Windows relocated file may be delete-pending until its old
        // source handle closes. The exclusive marker stays held across it.
        #[cfg(windows)]
        {
            self.lease.source._file.take();
            prepare_source(&self.lease.path)?;
        }
        self.lease.source = source_file(&self.lease.path, SourceAccess::Shared)?;
        Ok(self.lease)
    }
}

impl ProjectionLease {
    pub(crate) fn writable(path: &Path) -> Result<Self> {
        let lease = Self::existing_writable(path)?;
        #[cfg(windows)]
        let lease = if lease.source.identity.is_none() {
            prepare_source(&lease.path)?;
            Self {
                source: source_file(&lease.path, SourceAccess::Shared)?,
                ..lease
            }
        } else {
            lease
        };
        Ok(lease)
    }

    pub(crate) fn existing_writable(path: &Path) -> Result<Self> {
        let path = canonical_path(path, 0)?;
        let marker = open_marker(&path)?;
        lock_shared(&marker, &path)?;
        let source = source_file(&path, SourceAccess::Shared)?;
        Ok(Self {
            path,
            _marker: Some(marker),
            source,
        })
    }

    pub(crate) fn read_only(path: &Path) -> Result<Self> {
        let path = canonical_path(path, 0)?;
        let Some(marker) = existing_marker(&path)? else {
            // Distinguish a legitimate absent projection from an
            // existing legacy source without its concurrency marker.
            std::fs::metadata(&path)?;
            return Err(RiftError::ProjectionBusy {
                projection: path.display().to_string(),
                reason: "live projection has no concurrency marker; initialize this live projection through its writable workflow (rift-memd metrics for recall; rift init for capture), then retry; use explicit backfill for a historical source".into(),
            });
        };
        lock_shared(&marker, &path)?;
        let source = source_file(&path, SourceAccess::Shared)?;
        Ok(Self {
            path,
            _marker: Some(marker),
            source,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn has_source(&self) -> bool {
        self.source.identity.is_some()
    }
}

fn canonical_path(path: &Path, depth: u8) -> Result<PathBuf> {
    if depth == 16 {
        return Err(RiftError::storage(
            "projection symlink chain exceeds the supported depth",
        ));
    }
    match std::fs::canonicalize(path) {
        Ok(path) => return Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        let target = std::fs::read_link(path)?;
        let target = if target.is_absolute() {
            target
        } else {
            parent(path).join(target)
        };
        return canonical_path(&target, depth + 1);
    }
    let name = path
        .file_name()
        .ok_or_else(|| RiftError::storage("projection path has no filename"))?;
    Ok(std::fs::canonicalize(parent(path))?.join(name))
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn marker_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

fn open_marker(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(marker_path(path))
        .map_err(|e| source_error(path, e))
}

fn existing_marker(path: &Path) -> Result<Option<File>> {
    match File::open(marker_path(path)) {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(source_error(path, e)),
    }
}

#[cfg(windows)]
fn prepare_source(path: &Path) -> Result<()> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path).map_err(|e| source_error(path, e))?;
    Ok(())
}

fn source_file(path: &Path, access: SourceAccess) -> Result<SourceLease> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        let sharing = match access {
            SourceAccess::Shared => 3,
            SourceAccess::Replacement => 4,
        };
        let file = match OpenOptions::new().read(true).share_mode(sharing).open(path) {
            Ok(file) => Some(file),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(source_error(path, e)),
        };
        let identity = match &file {
            Some(file) => {
                let info = winapi_util::file::information(file)?;
                checked_identity(
                    info.number_of_links(),
                    info.volume_serial_number(),
                    info.file_index(),
                )?
            }
            None => None,
        };
        Ok(SourceLease {
            identity,
            _file: file,
        })
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let _ = access;
        let identity = match std::fs::metadata(path) {
            Ok(meta) => checked_identity(meta.nlink(), meta.dev(), meta.ino())?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        Ok(SourceLease { identity })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, access);
        Err(RiftError::storage(
            "projection source identity is unsupported on this platform",
        ))
    }
}

#[cfg(any(unix, windows))]
fn checked_identity(links: u64, volume: u64, file: u64) -> Result<Option<SourceIdentity>> {
    if links != 1 {
        return Err(RiftError::storage("projection source has multiple hard links; remove aliases before live opening or replacement"));
    }
    Ok(Some(SourceIdentity { volume, file }))
}

fn file_identity(path: &Path) -> Result<Option<SourceIdentity>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        match std::fs::metadata(path) {
            Ok(meta) => Ok(Some(SourceIdentity {
                volume: meta.dev(),
                file: meta.ino(),
            })),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        match OpenOptions::new().read(true).share_mode(7).open(path) {
            Ok(file) => {
                let info = winapi_util::file::information(&file)?;
                Ok(Some(SourceIdentity {
                    volume: info.volume_serial_number(),
                    file: info.file_index(),
                }))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(source_error(path, e)),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(RiftError::storage(
            "historical source identity is unsupported on this platform",
        ))
    }
}

fn lock_shared(file: &File, path: &Path) -> Result<()> {
    file.try_lock_shared().map_err(|e| lock_error(path, e))
}

fn lock_exclusive(file: &File, path: &Path) -> Result<()> {
    file.try_lock().map_err(|e| lock_error(path, e))
}

fn lock_error(path: &Path, error: TryLockError) -> RiftError {
    match error {
        TryLockError::WouldBlock => busy(path),
        TryLockError::Error(error) => source_error(path, error),
    }
}

fn source_error(path: &Path, error: io::Error) -> RiftError {
    #[cfg(windows)]
    if error.raw_os_error() == Some(32) {
        return busy(path);
    }
    let _ = path;
    error.into()
}

fn busy(path: &Path) -> RiftError {
    RiftError::ProjectionBusy {
        projection: path.display().to_string(),
        reason:
            "an active connection or replacement holds this projection; retry after it finishes"
                .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CanonicalStore, SqliteStore, Store, Value};

    fn is_busy<T>(result: &Result<T>) -> bool {
        matches!(result, Err(RiftError::ProjectionBusy { .. }))
    }

    #[test]
    fn replacement_excludes_all_store_open_flavors_without_creating_a_source() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("projection.db");
        let guard = ProjectionReplacementGuard::acquire(&db).unwrap();
        assert!(is_busy(&SqliteStore::open(&db)));
        assert!(is_busy(&SqliteStore::open_read_only(&db)));
        assert!(is_busy(&SqliteStore::open_existing(&db)));
        assert!(!db.exists());
        drop(guard);
        assert!(SqliteStore::open_existing(&db).unwrap().is_none());
    }

    #[test]
    fn missing_parent_and_source_are_not_created_by_existing_or_read_only_opens() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("absent/projection.db");
        assert!(SqliteStore::open_existing(&db).unwrap().is_none());
        assert!(SqliteStore::open_read_only(&db).is_err());
        assert!(!dir.path().join("absent").exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn transferred_guard_opens_its_owned_path_and_retains_exclusion_through_connections() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("projection.db");
        let guard = ProjectionReplacementGuard::acquire(&db).unwrap();
        assert_eq!(
            guard.path(),
            dir.path().canonicalize().unwrap().join("projection.db")
        );
        let store = crate::open_replacement_migrated(guard, &crate::default_migrations()).unwrap();
        let conn = Store::conn(&store).await.unwrap();
        drop(store);
        assert!(is_busy(&SqliteStore::open(&db)));
        conn.execute_batch(
            "CREATE TABLE lease_probe (n INTEGER); INSERT INTO lease_probe VALUES (7)",
        )
        .await
        .unwrap();
        drop(conn);
        assert!(SqliteStore::open(&db).is_ok());
        assert!(!dir.path().join("unguarded.db").exists());
    }

    #[tokio::test]
    async fn a_transaction_retains_the_guard_after_the_store_owner_drops() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("projection.db");
        let store = SqliteStore::open(&db).unwrap();
        let conn = Store::conn(&store).await.unwrap();
        conn.execute_batch("CREATE TABLE lease_probe (n INTEGER)")
            .await
            .unwrap();
        let tx = conn.begin(crate::TxMode::Deferred).await.unwrap();
        drop(store);
        assert!(is_busy(&ProjectionReplacementGuard::acquire(&db)));
        tx.execute("INSERT INTO lease_probe VALUES (?)", &[Value::Int(7)])
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert!(is_busy(&ProjectionReplacementGuard::acquire(&db)));
        drop(conn);
        assert!(ProjectionReplacementGuard::acquire(&db).is_ok());
    }

    #[test]
    fn a_live_reader_without_a_marker_refuses_without_creating_any_files() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("projection.db");
        std::fs::write(&db, b"original").unwrap();
        assert!(is_busy(&SqliteStore::open_read_only(&db)));
        assert_eq!(std::fs::read(&db).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn ordinary_concurrent_sqlite_and_historical_reads_preserve_wal_snapshot_consistency() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("projection.db");
        let writer = SqliteStore::open(&db).unwrap();
        let conn = Store::conn(&writer).await.unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE snapshot_probe (n INTEGER); INSERT INTO snapshot_probe VALUES (7)").await.unwrap();
        assert!(db.with_file_name("projection.db-wal").is_file());
        let snapshot = HistoricalProjectionSnapshot::user_selected(&db).unwrap();
        let reader = SqliteStore::open_historical_snapshot(snapshot).unwrap();
        let reading = Store::conn(&reader).await.unwrap();
        assert_eq!(
            reader
                .query("SELECT n FROM snapshot_probe", &[])
                .await
                .unwrap()[0]
                .get_i64(0)
                .unwrap(),
            7
        );
        let concurrent = SqliteStore::open(&db).unwrap();
        let writing = Store::conn(&concurrent).await.unwrap();
        writing
            .execute("INSERT INTO snapshot_probe VALUES (8)", &[])
            .await
            .unwrap();
        assert_eq!(
            reader
                .query("SELECT n FROM snapshot_probe", &[])
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(reading
            .execute("INSERT INTO snapshot_probe VALUES (9)", &[])
            .await
            .is_err());
        reading.execute_batch("ROLLBACK").await.unwrap();
        assert_eq!(
            reader
                .query("SELECT n FROM snapshot_probe", &[])
                .await
                .unwrap()
                .len(),
            2
        );
        drop(reading);
        drop(reader);
        // Closing ordinary readers must not release another SQLite writer's
        // transaction locks through an unrelated source descriptor.
        writing
            .execute_batch("BEGIN IMMEDIATE; INSERT INTO snapshot_probe VALUES (9)")
            .await
            .unwrap();
        let live_reader = SqliteStore::open_read_only(&db).unwrap();
        assert_eq!(
            live_reader
                .query("SELECT n FROM snapshot_probe", &[])
                .await
                .unwrap()
                .len(),
            2
        );
        drop(live_reader);
        writer
            .with_conn(|c| c.busy_timeout(std::time::Duration::ZERO))
            .unwrap();
        assert!(conn
            .execute("INSERT INTO snapshot_probe VALUES (10)", &[])
            .await
            .is_err());
        writing.execute_batch("COMMIT").await.unwrap();
        conn.execute("INSERT INTO snapshot_probe VALUES (10)", &[])
            .await
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn explicit_historical_source_is_readable_in_a_read_only_directory_without_a_marker() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("history.db");
        {
            let store = SqliteStore::open(&db).unwrap();
            store
                .with_conn(|c| {
                    c.execute_batch(
                        "CREATE TABLE history (n INTEGER); INSERT INTO history VALUES (7)",
                    )
                })
                .unwrap();
        }
        std::fs::remove_file(marker_path(&db)).unwrap();
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o444)).unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let selected = HistoricalProjectionSnapshot::user_selected(&db).unwrap();
        let store = SqliteStore::open_historical_snapshot(selected).unwrap();
        assert_eq!(
            store
                .with_conn(|c| c.query_row("SELECT n FROM history", [], |r| r.get::<_, i64>(0)))
                .unwrap(),
            7
        );
        drop(store);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert!(is_busy(&SqliteStore::open_read_only(&db)));
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn explicit_historical_selection_still_refuses_an_existing_exclusive_marker() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("history.db");
        drop(SqliteStore::open(&db).unwrap());
        let selected = HistoricalProjectionSnapshot::user_selected(&db).unwrap();
        let guard = ProjectionReplacementGuard::acquire(&db).unwrap();
        assert!(is_busy(&SqliteStore::open_historical_snapshot(selected)));
        drop(guard);
    }

    #[cfg(windows)]
    #[test]
    fn inspection_transition_refuses_a_foreign_handle_before_mutating_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("projection.db");
        std::fs::write(&db, b"original").unwrap();
        let foreign = std::cell::RefCell::new(None);
        let result = ProjectionReplacementGuard::acquire_inspected(&db, |_| {
            foreign.replace(Some(source_file(&db, SourceAccess::Shared).unwrap()));
        });
        assert!(is_busy(&result));
        assert_eq!(std::fs::read(&db).unwrap(), b"original");
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn a_source_alias_changed_during_inspection_refuses_both_sources_intact() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.db");
        let changed = dir.path().join("changed.db");
        let alias = dir.path().join("alias.db");
        std::fs::write(&original, b"original").unwrap();
        std::fs::write(&changed, b"changed").unwrap();
        file_alias_link(&original, &alias);
        let result = ProjectionReplacementGuard::acquire_inspected(&alias, |_| {
            std::fs::remove_file(&alias).unwrap();
            file_alias_link(&changed, &alias);
        });
        assert!(matches!(result, Err(RiftError::Storage(_))));
        assert_eq!(std::fs::read(&original).unwrap(), b"original");
        assert_eq!(std::fs::read(&changed).unwrap(), b"changed");
    }

    #[test]
    fn preexisting_hardlink_aliases_refuse_replacement_and_preserve_future_writes() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("projection.db");
        let alias = dir.path().join("alias.db");
        std::fs::write(&db, b"original").unwrap();
        std::fs::hard_link(&db, &alias).unwrap();
        assert!(matches!(
            ProjectionReplacementGuard::acquire(&db),
            Err(RiftError::Storage(_))
        ));
        std::fs::write(&alias, b"later").unwrap();
        assert_eq!(std::fs::read(&db).unwrap(), b"later");
        assert!(!dir.path().join("projection.db.set-aside").exists());
    }

    #[tokio::test]
    async fn real_sqlite_writes_through_a_hardlink_alias_cannot_escape_replacement_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("projection.db");
        let alias = dir.path().join("alias.db");
        let store = crate::open_capture_projection(&db).unwrap();
        std::fs::hard_link(&db, &alias).unwrap();
        assert!(SqliteStore::open(&alias).is_err());
        assert!(SqliteStore::open(&db).is_err());
        assert!(SqliteStore::open_read_only(&alias).is_err());
        let conn = Store::conn(&store).await.unwrap();
        conn.execute_batch(
            "CREATE TABLE alias_probe (n INTEGER); INSERT INTO alias_probe VALUES (7)",
        )
        .await
        .unwrap();
        assert!(ProjectionReplacementGuard::acquire(&db).is_err());
        assert!(ProjectionReplacementGuard::acquire(&alias).is_err());
        conn.execute("INSERT INTO alias_probe VALUES (9)", &[])
            .await
            .unwrap();
        let rows = store
            .query("SELECT n FROM alias_probe ORDER BY n", &[])
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].get_i64(0).unwrap(), 9);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn symlink_and_parent_aliases_share_the_canonical_marker_and_owned_path() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let parent_alias = dir.path().join("alias");
        directory_alias(&real, &parent_alias);
        let db = real.join("projection.db");
        let guard =
            ProjectionReplacementGuard::acquire(&parent_alias.join("projection.db")).unwrap();
        assert_eq!(
            guard.path(),
            real.canonicalize().unwrap().join("projection.db")
        );
        assert!(is_busy(&SqliteStore::open(&db)));
        drop(guard);
        let store = SqliteStore::open(&db).unwrap();
        let file_alias = dir.path().join("file.db");
        file_alias_link(&db, &file_alias);
        assert!(is_busy(&ProjectionReplacementGuard::acquire(&file_alias)));
        drop(store);
        assert_eq!(
            ProjectionReplacementGuard::acquire(&file_alias)
                .unwrap()
                .path(),
            db.canonicalize().unwrap()
        );
    }

    #[cfg(any(unix, windows))]
    fn directory_alias(source: &Path, target: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(source, target).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(source, target)
            .expect("Windows alias regression requires Developer Mode or symlink privilege");
    }

    #[cfg(any(unix, windows))]
    fn file_alias_link(source: &Path, target: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(source, target).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(source, target)
            .expect("Windows alias regression requires Developer Mode or symlink privilege");
    }
}
