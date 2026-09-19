//! The machine-global Canopy UI-state store: typed, append-only writers
//! and a fold reader over `canopy_project_event`.
//!
//! Canopy stays pure (`rift-core` only); *this* crate owns the SQLite side of
//! the dashboard's registry (the data-plane split). Every mutation the reducer emits as a
//! [`PersistOp`] becomes a new event row — add / remove / relabel / reorder are
//! all **inserts** (never `UPDATE`/`DELETE`), so the log is immutable by
//! construction and the current registry is the pure fold
//! [`rift_canopy::current_registry`] over it. Reads and the fold are engine-blind;
//! only this module tree touches `rift-store`.
//!
//! One module per relation the store appends to, because a relation is the
//! unit that has to stay coherent: its one writer, its column list, its
//! readers and the tests that pin its fold live together. This module keeps
//! the error type, the handle, the dispatch of a [`PersistOp`] and the
//! helpers every relation shares.

mod catalogue;
mod layouts;
mod membership;
mod profiles;
mod startup;
mod themes;
mod views;

#[cfg(test)]
mod fixtures;

use std::path::Path;

use rift_canopy::{PersistOp, Project};
use rift_core::{MonotonicUlidSource, Result, RiftError};
use rift_store::{canopy_migrations, canopy_ui_db_under, SqliteStore, Value};

use self::views::view_with_worktrees;

/// Why a Canopy registry mutation was refused.
///
/// One variant, and it is a *type* rather than a message because the refusal is
/// a rule and not a failure: the registry holds one row per repository family
/// (the family view), so a second path to a family that already has a row is turned
/// away — with the path that holds it, which is the one thing the reader needs.
///
/// It reaches callers as a [`RiftError`]: the reducer's persist seam is typed on
/// `rift_core::Result`, and an `Ok` no-op would fire the dashboard's **success**
/// toast for a registration that did not happen. Converting here means the
/// sentence is written once and the alert and the CLI print the same words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CanopyError {
    /// Another checkout of this repository family already holds the family's
    /// row; the path that holds it is carried.
    AlreadyRegisteredAs(String),
    /// A registration failed at a named step — the path the operator asked
    /// for, the operation that refused it, and the OS's or store's own words —
    /// so the alert says *which* folder and *what* went wrong, never a bare
    /// `io error` the reader cannot place. The `canonicalize` step carries the
    /// path as typed; every later step carries the canonical form.
    Register {
        /// The path being registered, as the failing step saw it.
        path: String,
        /// The step that failed (`canonicalize`, `check`, `read the registry`, …).
        step: &'static str,
        /// The failure's own text (identifiers and reasons only, never content).
        error: String,
    },
}

impl core::fmt::Display for CanopyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::AlreadyRegisteredAs(root) => write!(
                f,
                "already registered as {root} — one row per repository; \
                 a linked worktree is listed under it, never registered on its own"
            ),
            Self::Register { path, step, error } => {
                write!(f, "could not register {path} — {step}: {error}")
            }
        }
    }
}

impl From<CanopyError> for RiftError {
    /// Rendered bare (`RiftError::Other`), because this string is shown to a
    /// person in an alert and a CLI line: a category prefix in front of a
    /// finished sentence is noise.
    fn from(error: CanopyError) -> Self {
        Self::Other(error.to_string())
    }
}

/// The column order for every `canopy_profile_event` insert.
const PROFILE_COLS: [&str; 6] = [
    "event_id",
    "profile_id",
    "op",
    "name",
    "supersedes",
    "created_at",
];

/// The column order for every `canopy_profile_project_event` insert.
const PROFILE_PROJECT_COLS: [&str; 7] = [
    "event_id",
    "profile_id",
    "project_id",
    "op",
    "order_key",
    "supersedes",
    "created_at",
];

/// The column order for every `canopy_profile_layout_event` insert.
const PROFILE_LAYOUT_COLS: [&str; 6] = [
    "event_id",
    "profile_id",
    "project_id",
    "layout",
    "supersedes",
    "created_at",
];

/// The column order for every `canopy_profile_theme_event` insert.
const PROFILE_THEME_COLS: [&str; 6] = [
    "event_id",
    "profile_id",
    "slot",
    "colour",
    "supersedes",
    "created_at",
];

/// The column order for every `canopy_profile_startup_event` insert.
const STARTUP_COLS: [&str; 4] = ["event_id", "profile_id", "supersedes", "created_at"];

/// The column order for every `canopy_project_event` insert (one source of truth).
const EVENT_COLS: [&str; 9] = [
    "event_id",
    "project_id",
    "op",
    "repo_id",
    "path",
    "label",
    "order_key",
    "supersedes",
    "created_at",
];

/// A handle to the append-only Canopy UI-state store, plus the ULID source that
/// stamps fresh `event_id`/`project_id`s.
pub struct CanopyStore {
    store: SqliteStore,
    ulids: MonotonicUlidSource,
}

impl CanopyStore {
    /// Open (creating + migrating if absent) the machine-global `ui-state.db`
    /// under cache `root`.
    ///
    /// # Errors
    /// [`RiftError`] if the database cannot be created, opened, or migrated.
    pub fn open(root: &Path) -> Result<Self> {
        let store = rift_store::open_migrated(canopy_ui_db_under(root), &canopy_migrations())?;
        Ok(Self {
            store,
            ulids: MonotonicUlidSource::new(),
        })
    }

    /// Open an existing Canopy UI-state store with SQLite's enforced
    /// read-only flag. The file is never created or migrated.
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the existing file is unreadable or invalid.
    pub fn open_read_only(root: &Path) -> Result<Option<Self>> {
        let path = canopy_ui_db_under(root);
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(Self {
            store: SqliteStore::open_read_only(path)?,
            ulids: MonotonicUlidSource::new(),
        }))
    }

    /// The current **global catalogue**, folded from the event log and projected
    /// onto Canopy's [`Project`] view (status starts `Idle`; the live bus refines
    /// it), with each repository's **linked worktrees** listed as child rows
    /// directly under it. Profile-scoped callers want
    /// [`Self::active_projects`]; this is the registration source of truth.
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the log cannot be read.
    pub fn projects(&self) -> Result<Vec<Project>> {
        Ok(view_with_worktrees(self.registry()?))
    }

    /// Apply one append-only persistence op and return the refreshed project list
    /// for the **active profile's view** (falling back to the global catalogue on
    /// an unseeded store). Every op appends superseding events; a no-op target
    /// (unknown path, or a reorder off the end) leaves the log untouched.
    ///
    /// # Errors
    /// [`RiftError`] if the path is invalid (add), a profile guard refuses
    /// (remove-active / remove-last / invalid name), or the store rejects a write.
    pub fn apply(&self, op: &PersistOp, now_ms: i64) -> Result<Vec<Project>> {
        match op {
            PersistOp::AddProject { path } => {
                self.add_to_view(path, now_ms)?;
            }
            PersistOp::RemoveProject { path } => self.remove(path, now_ms)?,
            PersistOp::RelabelProject { path, label } => self.relabel(path, label, now_ms)?,
            PersistOp::ReorderProject { path, up } => self.reorder(path, *up, now_ms)?,
            PersistOp::SetLayout {
                profile,
                path,
                layout,
            } => {
                self.set_profile_layout(profile, path, layout.as_deref(), now_ms)?;
            }
            PersistOp::CreateProfile { name } => {
                self.create_profile(name, now_ms)?;
            }
            PersistOp::DuplicateProfile { source, name } => {
                self.duplicate_profile(source, name, now_ms)?;
            }
            PersistOp::RenameProfile { profile, name } => {
                self.rename_profile(profile, name, now_ms)?;
            }
            PersistOp::RemoveProfile { profile } => self.remove_profile(profile, now_ms)?,
            PersistOp::SelectProfile { profile } => self.select_profile(profile, now_ms)?,
            PersistOp::AddProfileProject { profile, path } => {
                self.add_profile_project(profile, path, now_ms)?;
            }
            PersistOp::RemoveProfileProject { profile, path } => {
                self.remove_profile_project(profile, path, now_ms)?;
            }
            PersistOp::ReorderProfileProject { profile, path, up } => {
                self.reorder_profile_project(profile, path, *up, now_ms)?;
            }
            PersistOp::SetProfileTheme {
                profile,
                slot,
                colour,
            } => self.set_profile_theme(profile, slot, colour.as_deref(), now_ms)?,
            PersistOp::SetStartupProfile { profile } => {
                self.set_startup_profile(profile.as_deref(), now_ms)?;
            }
        }
        self.active_projects()
    }
}

/// A short-lived current-thread runtime for the async store seam — the one
/// builder every canopy-plane call site uses (the bus's read paths included).
///
/// # Errors
/// [`RiftError`] if the runtime cannot be built.
pub fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(RiftError::from)
}

/// The index of `i`'s neighbor in the given `up` direction, or `None` at the end.
fn neighbor(i: usize, up: bool, len: usize) -> Option<usize> {
    if up {
        i.checked_sub(1)
    } else if i + 1 < len {
        Some(i + 1)
    } else {
        None
    }
}

/// Nullable text value from an `Option<String>`.
fn opt_text(v: Option<String>) -> Value {
    v.map_or(Value::Null, Value::text)
}

/// Nullable text value from an `Option<&str>`.
fn opt_text_ref(v: Option<&str>) -> Value {
    v.map_or(Value::Null, Value::text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::fixtures::make_dir;

    #[test]
    fn read_only_open_preserves_an_absent_ui_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = canopy_ui_db_under(dir.path());
        assert!(!path.exists());
        assert!(CanopyStore::open_read_only(dir.path()).unwrap().is_none());
        assert!(
            !path.exists(),
            "inspection must not create an absent database"
        );
    }

    #[test]
    fn read_only_open_leaves_an_existing_ui_database_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let project = make_dir(dir.path(), "project");
        {
            let store = CanopyStore::open(dir.path()).unwrap();
            let op = PersistOp::AddProject { path: project };
            store.apply(&op, 1).unwrap();
        }
        let path = canopy_ui_db_under(dir.path());
        let before = std::fs::read(&path).unwrap();
        let read_only = CanopyStore::open_read_only(dir.path()).unwrap().unwrap();
        assert_eq!(read_only.projects().unwrap().len(), 1);
        drop(read_only);
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
}
