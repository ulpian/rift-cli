//! The machine-global Canopy UI-state store (P4/P5): typed, append-only writers
//! and a fold reader over `canopy_project_event`.
//!
//! Canopy stays pure (`rift-core` only); *this* bin module owns the SQLite side of
//! the dashboard's registry. Every mutation the reducer emits as a
//! [`PersistOp`] becomes a new event row — add / remove / relabel / reorder are
//! all **inserts** (never `UPDATE`/`DELETE`), so the log is immutable by
//! construction and the current registry is the pure fold
//! [`rift_canopy::current_registry`] over it. Reads and the fold are engine-blind;
//! only this module touches `rift-store`.

use std::path::Path;

use rift_canopy::{
    current_registry, current_toggles, toggle_view, PersistOp, Project, ProjectEntry, ProjectEvent,
    ProjectOp, ProjectStatus, ToggleEvent, ToggleView,
};
use rift_core::{MonotonicUlidSource, ProjectId, Result, RiftError, UlidSource};
use rift_registry::ProjectRegistry;
use rift_store::{canopy_migrations, canopy_ui_db_under, CanonicalStore, SqliteStore, Value};

use crate::verify::repo_id_for;

/// The column order for every `canopy_toggle_event` insert.
const TOGGLE_COLS: [&str; 6] = [
    "event_id",
    "project_id",
    "feature",
    "desired",
    "supersedes",
    "created_at",
];

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
pub(crate) struct CanopyStore {
    store: SqliteStore,
    ulids: MonotonicUlidSource,
}

impl CanopyStore {
    /// Open (creating + migrating if absent) the machine-global `ui-state.db`
    /// under cache `root`.
    ///
    /// # Errors
    /// [`RiftError`] if the database cannot be created, opened, or migrated.
    pub(crate) fn open(root: &Path) -> Result<Self> {
        let store = rift_store::open_migrated(canopy_ui_db_under(root), &canopy_migrations())?;
        Ok(Self {
            store,
            ulids: MonotonicUlidSource::new(),
        })
    }

    /// The current registry, folded from the event log and projected onto Canopy's
    /// [`Project`] view (status starts `Idle`; the live bus refines it).
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the log cannot be read.
    pub(crate) fn projects(&self) -> Result<Vec<Project>> {
        let entries = self.registry()?;
        Ok(entries
            .into_iter()
            .map(|e| Project {
                name: e.label,
                path: e.path,
                status: ProjectStatus::Idle,
            })
            .collect())
    }

    /// Apply one append-only persistence op and return the refreshed project list.
    /// Add/remove/relabel/reorder each append an event; a no-op target (unknown
    /// path, or a reorder off the end) leaves the log untouched. `SetToggle` is
    /// handled by the column-2 step (P5 S2) and is a no-op here.
    ///
    /// # Errors
    /// [`RiftError`] if the path is invalid (add) or the store rejects a write.
    pub(crate) fn apply(&self, op: &PersistOp, now_ms: i64) -> Result<Vec<Project>> {
        match op {
            PersistOp::AddProject { path } => self.add(path, now_ms)?,
            PersistOp::RemoveProject { path } => self.remove(path, now_ms)?,
            PersistOp::RelabelProject { path, label } => self.relabel(path, label, now_ms)?,
            PersistOp::ReorderProject { path, up } => self.reorder(path, *up, now_ms)?,
            PersistOp::SetToggle {
                path,
                feature,
                desired,
            } => self.set_toggle(path, feature, *desired, now_ms)?,
        }
        self.projects()
    }

    /// The current feature-toggle view for a project (empty if the project is
    /// unknown). The read side of [`PersistOp::SetToggle`] — consumed by the live
    /// dashboard's detail builder (the `tui` path).
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the toggle log cannot be read.
    #[cfg_attr(not(feature = "tui"), allow(dead_code))]
    pub(crate) fn toggles(&self, path: &str) -> Result<ToggleView> {
        let Some(project_id) = self.project_id_for(path)? else {
            return Ok(ToggleView::default());
        };
        Ok(toggle_view(&current_toggles(
            &self.toggle_events(&project_id)?,
        )))
    }

    /// One-time migration from the legacy mutable TOML registry: if the event log
    /// is empty and `reg` has projects, fold each in as an `added` event
    /// (idempotent by canonical path). A no-op once the log is populated.
    ///
    /// # Errors
    /// [`RiftError`] if the log cannot be read or an event cannot be written.
    pub(crate) fn migrate_from_toml(&self, reg: &ProjectRegistry, now_ms: i64) -> Result<()> {
        if !self.registry()?.is_empty() {
            return Ok(()); // already on the append-only store
        }
        for p in reg.projects() {
            let path = p.path.to_string_lossy();
            // Best-effort: skip an entry whose path no longer resolves.
            if let Ok(repo) = repo_id_for(&p.path) {
                self.append_added(&path, &p.name, repo.as_str(), now_ms)?;
            }
        }
        Ok(())
    }

    // --- writers ------------------------------------------------------------

    fn add(&self, path: &str, now_ms: i64) -> Result<()> {
        let canon = std::fs::canonicalize(path)?;
        if !canon.is_dir() {
            return Err(RiftError::validity(format!(
                "{} is not a directory",
                canon.display()
            )));
        }
        let canon_str = canon.to_string_lossy().into_owned();
        // Idempotent per canonical path: a present project is not re-added.
        if self.registry()?.iter().any(|e| e.path == canon_str) {
            return Ok(());
        }
        let repo = repo_id_for(&canon)?;
        let label = canon
            .file_name()
            .map_or_else(|| canon_str.clone(), |n| n.to_string_lossy().into_owned());
        self.append_added(&canon_str, &label, repo.as_str(), now_ms)
    }

    fn remove(&self, path: &str, now_ms: i64) -> Result<()> {
        let events = self.events()?;
        let Some(entry) = current_registry(&events)
            .into_iter()
            .find(|e| e.path == path)
        else {
            return Ok(()); // unknown path → nothing to hide
        };
        let supersedes = latest_event_id(&events, &entry.project_id);
        self.insert(
            &entry.project_id,
            "removed",
            &[
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                opt_text(supersedes),
                Value::Int(now_ms),
            ],
        )
    }

    fn relabel(&self, path: &str, label: &str, now_ms: i64) -> Result<()> {
        let events = self.events()?;
        let Some(entry) = current_registry(&events)
            .into_iter()
            .find(|e| e.path == path)
        else {
            return Ok(());
        };
        let supersedes = latest_event_id(&events, &entry.project_id);
        self.insert(
            &entry.project_id,
            "relabeled",
            &[
                Value::Null,
                Value::Null,
                Value::text(label),
                Value::Null,
                opt_text(supersedes),
                Value::Int(now_ms),
            ],
        )
    }

    fn reorder(&self, path: &str, up: bool, now_ms: i64) -> Result<()> {
        let events = self.events()?;
        let registry = current_registry(&events);
        let Some(i) = registry.iter().position(|e| e.path == path) else {
            return Ok(());
        };
        // Off-the-end moves are no-ops (top can't go up; bottom can't go down).
        let Some(j) = neighbor(i, up, registry.len()) else {
            return Ok(());
        };
        let (Some(a), Some(b)) = (registry.get(i), registry.get(j)) else {
            return Ok(()); // unreachable: position()/neighbor() are in-bounds
        };
        // Swap their effective order keys: two superseding `reordered` events.
        self.insert_reorder(
            &a.project_id,
            b.order_key,
            latest_event_id(&events, &a.project_id),
            now_ms,
        )?;
        self.insert_reorder(
            &b.project_id,
            a.order_key,
            latest_event_id(&events, &b.project_id),
            now_ms,
        )
    }

    /// Append an `added` event for a fresh project id.
    fn append_added(&self, path: &str, label: &str, repo_id: &str, now_ms: i64) -> Result<()> {
        let project_id = ProjectId::new(self.ulids.next_ulid()).to_string();
        self.insert_row(
            &project_id,
            "added",
            &[
                Value::text(repo_id),
                Value::text(path),
                Value::text(label),
                Value::Null,
                Value::Null,
                Value::Int(now_ms),
            ],
        )
    }

    fn insert_reorder(
        &self,
        project_id: &str,
        order_key: i64,
        supersedes: Option<String>,
        now_ms: i64,
    ) -> Result<()> {
        self.insert(
            project_id,
            "reordered",
            &[
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Int(order_key),
                opt_text(supersedes),
                Value::Int(now_ms),
            ],
        )
    }

    /// Insert one event for an *existing* project id (`tail` is the six columns
    /// after `event_id`, `project_id`, `op`).
    fn insert(&self, project_id: &str, op: &str, tail: &[Value]) -> Result<()> {
        self.insert_row(project_id, op, tail)
    }

    fn insert_row(&self, project_id: &str, op: &str, tail: &[Value]) -> Result<()> {
        let event_id = self.ulids.next_ulid().to_hex();
        let mut values = Vec::with_capacity(EVENT_COLS.len());
        values.push(Value::text(event_id));
        values.push(Value::text(project_id));
        values.push(Value::text(op));
        values.extend_from_slice(tail);
        let cols: Vec<&str> = EVENT_COLS.to_vec();
        rt()?.block_on(self.store.insert("canopy_project_event", &cols, &values))
    }

    // --- readers ------------------------------------------------------------

    /// The current registry entries (the fold over the event log).
    fn registry(&self) -> Result<Vec<ProjectEntry>> {
        Ok(current_registry(&self.events()?))
    }

    /// Read every event row into the engine-neutral [`ProjectEvent`], ordered by
    /// transaction time then event id (the fold re-sorts defensively regardless).
    fn events(&self) -> Result<Vec<ProjectEvent>> {
        let sql = "SELECT event_id, project_id, op, repo_id, path, label, order_key, created_at \
                   FROM canopy_project_event ORDER BY created_at, event_id";
        let rows = rt()?.block_on(self.store.query(sql, &[]))?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let event_id = r.get_text(0)?;
            let project_id = r.get_text(1)?;
            let op = r.get_text(2)?;
            let repo_id = r.get_opt_text(3)?;
            let path = r.get_opt_text(4)?;
            let label = r.get_opt_text(5)?;
            let order_key = r.get_opt_i64(6)?;
            let created_at = r.get_i64(7)?;
            let op = decode_op(&op, repo_id, path, label, order_key)?;
            out.push(ProjectEvent {
                event_id,
                project_id,
                op,
                created_at,
            });
        }
        Ok(out)
    }

    /// Resolve a canonical path to its stable `project_id` (via the registry fold),
    /// or `None` if no present project has that path.
    fn project_id_for(&self, path: &str) -> Result<Option<String>> {
        Ok(self
            .registry()?
            .into_iter()
            .find(|e| e.path == path)
            .map(|e| e.project_id))
    }

    /// Append a `canopy_toggle_event` recording the desired state for one
    /// `(project, feature)`. A no-op for an unknown path.
    fn set_toggle(&self, path: &str, feature: &str, desired: bool, now_ms: i64) -> Result<()> {
        let Some(project_id) = self.project_id_for(path)? else {
            return Ok(());
        };
        let supersedes = self
            .toggle_events(&project_id)?
            .into_iter()
            .rfind(|e| e.feature == feature)
            .map(|e| e.event_id);
        let event_id = self.ulids.next_ulid().to_hex();
        let values = [
            Value::text(event_id),
            Value::text(&project_id),
            Value::text(feature),
            Value::Int(i64::from(desired)),
            opt_text(supersedes),
            Value::Int(now_ms),
        ];
        let cols: Vec<&str> = TOGGLE_COLS.to_vec();
        rt()?.block_on(self.store.insert("canopy_toggle_event", &cols, &values))
    }

    /// Read a project's toggle events into engine-neutral [`ToggleEvent`]s.
    fn toggle_events(&self, project_id: &str) -> Result<Vec<ToggleEvent>> {
        let sql = "SELECT event_id, feature, desired, created_at \
                   FROM canopy_toggle_event WHERE project_id = ? ORDER BY created_at, event_id";
        let rows = rt()?.block_on(self.store.query(sql, &[Value::text(project_id)]))?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(ToggleEvent {
                event_id: r.get_text(0)?,
                feature: r.get_text(1)?,
                desired: r.get_i64(2)? != 0,
                created_at: r.get_i64(3)?,
            });
        }
        Ok(out)
    }
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

/// Decode a stored `op` + its columns into a typed [`ProjectOp`]. An unknown op or
/// an `added` missing its path is a data error (never silently mis-folded).
fn decode_op(
    op: &str,
    repo_id: Option<String>,
    path: Option<String>,
    label: Option<String>,
    order_key: Option<i64>,
) -> Result<ProjectOp> {
    match op {
        "added" => {
            let path = path.ok_or_else(|| RiftError::storage("added event missing path"))?;
            let label = label.unwrap_or_else(|| path.clone());
            Ok(ProjectOp::Added {
                repo_id,
                path,
                label,
            })
        }
        "removed" => Ok(ProjectOp::Removed),
        "relabeled" => Ok(ProjectOp::Relabeled {
            label: label.unwrap_or_default(),
        }),
        "reordered" => {
            let order_key =
                order_key.ok_or_else(|| RiftError::storage("reordered event missing order_key"))?;
            Ok(ProjectOp::Reordered { order_key })
        }
        other => Err(RiftError::storage(format!("unknown project op {other:?}"))),
    }
}

/// The most recent `event_id` recorded for `project_id` (events arrive sorted), or
/// `None` — the `supersedes` back-pointer for the next correction.
fn latest_event_id(events: &[ProjectEvent], project_id: &str) -> Option<String> {
    events
        .iter()
        .rev()
        .find(|e| e.project_id == project_id)
        .map(|e| e.event_id.clone())
}

/// Nullable text value from an `Option<String>`.
fn opt_text(v: Option<String>) -> Value {
    v.map_or(Value::Null, Value::text)
}

/// A short-lived current-thread runtime for the async store seam.
fn rt() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(RiftError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, CanopyStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = CanopyStore::open(dir.path()).unwrap();
        (dir, store)
    }

    /// Create `name` as a real directory under `root` and return its canonical path.
    fn make_dir(root: &Path, name: &str) -> String {
        let p = root.join(name);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::canonicalize(&p)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn add_lists_project_with_basename_label() {
        let (dir, cs) = store();
        let path = make_dir(dir.path(), "alpha");
        let projects = cs
            .apply(&PersistOp::AddProject { path: path.clone() }, 1)
            .unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, path);
        assert_eq!(projects[0].name, "alpha");
    }

    #[test]
    fn add_is_idempotent_per_path() {
        let (dir, cs) = store();
        let path = make_dir(dir.path(), "alpha");
        cs.apply(&PersistOp::AddProject { path: path.clone() }, 1)
            .unwrap();
        let again = cs.apply(&PersistOp::AddProject { path }, 2).unwrap();
        assert_eq!(again.len(), 1, "re-adding the same path is a no-op");
        // Only one `added` row exists (idempotent at the writer).
        assert_eq!(cs.events().unwrap().len(), 1);
    }

    #[test]
    fn add_rejects_non_directory() {
        let (dir, cs) = store();
        let missing = dir
            .path()
            .join("does-not-exist")
            .to_string_lossy()
            .into_owned();
        assert!(cs
            .apply(&PersistOp::AddProject { path: missing }, 1)
            .is_err());
    }

    #[test]
    fn remove_appends_event_and_hides_but_keeps_history() {
        let (dir, cs) = store();
        let path = make_dir(dir.path(), "alpha");
        cs.apply(&PersistOp::AddProject { path: path.clone() }, 1)
            .unwrap();
        let after = cs.apply(&PersistOp::RemoveProject { path }, 2).unwrap();
        assert!(after.is_empty(), "removed project is hidden");
        // Append-only: the history (added + removed) is preserved, nothing deleted.
        assert_eq!(cs.events().unwrap().len(), 2);
    }

    #[test]
    fn relabel_changes_the_display_name() {
        let (dir, cs) = store();
        let path = make_dir(dir.path(), "alpha");
        cs.apply(&PersistOp::AddProject { path: path.clone() }, 1)
            .unwrap();
        let after = cs
            .apply(
                &PersistOp::RelabelProject {
                    path,
                    label: "Alpha!".into(),
                },
                2,
            )
            .unwrap();
        assert_eq!(after[0].name, "Alpha!");
    }

    #[test]
    fn reorder_down_then_up_swaps_and_restores() {
        let (dir, cs) = store();
        let a = make_dir(dir.path(), "a");
        let b = make_dir(dir.path(), "b");
        cs.apply(&PersistOp::AddProject { path: a.clone() }, 1)
            .unwrap();
        cs.apply(&PersistOp::AddProject { path: b.clone() }, 2)
            .unwrap();
        // Registration order: a, b.
        assert_eq!(cs.projects().unwrap()[0].path, a);

        // Move a down → b, a.
        let after = cs
            .apply(
                &PersistOp::ReorderProject {
                    path: a.clone(),
                    up: false,
                },
                3,
            )
            .unwrap();
        assert_eq!(after[0].path, b);
        assert_eq!(after[1].path, a);

        // Move a back up → a, b.
        let after = cs
            .apply(
                &PersistOp::ReorderProject {
                    path: a.clone(),
                    up: true,
                },
                4,
            )
            .unwrap();
        assert_eq!(after[0].path, a);
        assert_eq!(after[1].path, b);
    }

    #[test]
    fn reorder_off_the_end_is_a_noop() {
        let (dir, cs) = store();
        let a = make_dir(dir.path(), "a");
        cs.apply(&PersistOp::AddProject { path: a.clone() }, 1)
            .unwrap();
        let n = cs.events().unwrap().len();
        // Top item can't move up.
        cs.apply(&PersistOp::ReorderProject { path: a, up: true }, 2)
            .unwrap();
        assert_eq!(
            cs.events().unwrap().len(),
            n,
            "no event appended off the end"
        );
    }

    #[test]
    fn migrate_from_toml_folds_in_once() {
        let (dir, cs) = store();
        let a = make_dir(dir.path(), "a");
        let b = make_dir(dir.path(), "b");
        let mut reg = ProjectRegistry::default();
        reg.add("a", std::path::PathBuf::from(&a));
        reg.add("b", std::path::PathBuf::from(&b));

        cs.migrate_from_toml(&reg, 1).unwrap();
        assert_eq!(cs.projects().unwrap().len(), 2);
        let events_after_first = cs.events().unwrap().len();

        // A second migration is a no-op (the log is already populated).
        cs.migrate_from_toml(&reg, 2).unwrap();
        assert_eq!(cs.events().unwrap().len(), events_after_first);
    }

    #[test]
    fn flip_appends_toggle_event_and_reads_back() {
        use rift_canopy::Feature;
        let (dir, cs) = store();
        let path = make_dir(dir.path(), "t");
        cs.apply(&PersistOp::AddProject { path: path.clone() }, 1)
            .unwrap();

        // Enable fff.
        cs.apply(
            &PersistOp::SetToggle {
                path: path.clone(),
                feature: "fff".into(),
                desired: true,
            },
            2,
        )
        .unwrap();
        assert!(cs.toggles(&path).unwrap().is_on(Feature::Fff));

        // Disabling is a NEW superseding event (history preserved, not an UPDATE).
        cs.apply(
            &PersistOp::SetToggle {
                path: path.clone(),
                feature: "fff".into(),
                desired: false,
            },
            3,
        )
        .unwrap();
        assert!(!cs.toggles(&path).unwrap().is_on(Feature::Fff));
        let repo = repo_id_for(std::path::Path::new(&path)).unwrap();
        let pid = current_registry(&cs.events().unwrap())
            .into_iter()
            .find(|e| e.repo_id.as_deref() == Some(repo.as_str()))
            .unwrap()
            .project_id;
        assert_eq!(
            cs.toggle_events(&pid).unwrap().len(),
            2,
            "both toggle events are kept (append-only)"
        );
    }

    #[test]
    fn toggle_for_unknown_path_is_a_noop() {
        let (dir, cs) = store();
        // Never added → no project_id → the write is silently skipped.
        cs.apply(
            &PersistOp::SetToggle {
                path: dir.path().join("ghost").to_string_lossy().into_owned(),
                feature: "fff".into(),
                desired: true,
            },
            1,
        )
        .unwrap();
        assert_eq!(cs.projects().unwrap().len(), 0);
    }
}
