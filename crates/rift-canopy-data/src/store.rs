//! The machine-global Canopy UI-state store (P4/P5): typed, append-only writers
//! and a fold reader over `canopy_project_event`.
//!
//! Canopy stays pure (`rift-core` only); *this* crate owns the SQLite side of
//! the dashboard's registry (ADR-0009). Every mutation the reducer emits as a
//! [`PersistOp`] becomes a new event row — add / remove / relabel / reorder are
//! all **inserts** (never `UPDATE`/`DELETE`), so the log is immutable by
//! construction and the current registry is the pure fold
//! [`rift_canopy::current_registry`] over it. Reads and the fold are engine-blind;
//! only this module touches `rift-store`.

use std::path::Path;

use rift_canopy::{
    current_registry, current_toggles, toggle_view, EntryKind, PersistOp, Project, ProjectEntry,
    ProjectEvent, ProjectOp, ProjectStatus, ToggleEvent, ToggleView,
};
use rift_core::{MonotonicUlidSource, ProjectId, Result, RiftError, UlidSource};
use rift_git::repo_id_for;
use rift_store::{canopy_migrations, canopy_ui_db_under, CanonicalStore, SqliteStore, Value};

/// The column order for every `canopy_toggle_event` insert.
const TOGGLE_COLS: [&str; 6] = [
    "event_id",
    "project_id",
    "feature",
    "desired",
    "supersedes",
    "created_at",
];

/// The column order for every `canopy_layout_event` insert (ADR-0035).
const LAYOUT_COLS: [&str; 5] = [
    "event_id",
    "project_id",
    "layout",
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

    /// The current registry, folded from the event log and projected onto
    /// Canopy's [`Project`] view (status starts `Idle`; the live bus refines
    /// it), with each repository's **linked worktrees** listed as child rows
    /// directly under it (M4-07, user story 11). Worktree rows are derived
    /// from git — never persisted — and carry the minimal honest semantics:
    /// the directory name as the label, [`EntryKind::Worktree`], and a status
    /// derived for their own path like any row (a worktree that was never
    /// `rift init`ed honestly reads NotInitialized). A worktree that is
    /// itself a registered project keeps its own registry row instead.
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the log cannot be read.
    pub fn projects(&self) -> Result<Vec<Project>> {
        let entries = self.registry()?;
        let registered: std::collections::BTreeSet<String> =
            entries.iter().map(|e| e.path.clone()).collect();
        let mut out = Vec::new();
        for e in entries {
            let repo_path = std::path::PathBuf::from(&e.path);
            out.push(Project {
                name: e.label,
                path: e.path,
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            });
            // Children: this repository's linked worktrees (best-effort — a
            // non-git or unreadable repo simply has none).
            let Ok(paths) = rift_git::discover(&repo_path) else {
                continue;
            };
            for wt in rift_git::list_worktrees(&paths) {
                let wt_path = wt.work_tree.to_string_lossy().into_owned();
                if registered.contains(&wt_path) {
                    continue; // it has its own registry row
                }
                out.push(Project {
                    name: wt.name,
                    path: wt_path,
                    status: ProjectStatus::Idle,
                    kind: EntryKind::Worktree,
                });
            }
        }
        Ok(out)
    }

    /// Apply one append-only persistence op and return the refreshed project list.
    /// Add/remove/relabel/reorder each append an event; a no-op target (unknown
    /// path, or a reorder off the end) leaves the log untouched. `SetToggle` is
    /// handled by the column-2 step (P5 S2) and is a no-op here.
    ///
    /// # Errors
    /// [`RiftError`] if the path is invalid (add) or the store rejects a write.
    pub fn apply(&self, op: &PersistOp, now_ms: i64) -> Result<Vec<Project>> {
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
            PersistOp::SetLayout { path, layout } => {
                self.set_layout(path, layout.as_deref(), now_ms)?;
            }
        }
        self.projects()
    }

    /// The current feature-toggle view for a project (empty if the project is
    /// unknown). The read side of [`PersistOp::SetToggle`] — consumed by the live
    /// dashboard's detail builder.
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the toggle log cannot be read.
    pub fn toggles(&self, path: &str) -> Result<ToggleView> {
        let Some(project_id) = self.project_id_for(path)? else {
            return Ok(ToggleView::default());
        };
        Ok(toggle_view(&current_toggles(
            &self.toggle_events(&project_id)?,
        )))
    }

    /// One-time import from a legacy registry's `(name, path)` entries: if the
    /// event log is empty, fold each in as an `added` event. A no-op once the
    /// log is populated. Registry-format-free so this crate takes no registry
    /// dependency — the bin flattens its TOML registry into pairs.
    ///
    /// # Errors
    /// [`RiftError`] if the log cannot be read or an event cannot be written.
    pub fn import_legacy(
        &self,
        entries: &[(String, std::path::PathBuf)],
        now_ms: i64,
    ) -> Result<()> {
        if !self.registry()?.is_empty() {
            return Ok(()); // already on the append-only store
        }
        for (name, path) in entries {
            let path_str = path.to_string_lossy();
            // Best-effort: skip an entry whose path no longer resolves.
            if let Ok(repo) = repo_id_for(path) {
                let project_id = ProjectId::new(self.ulids.next_ulid()).to_string();
                self.append(
                    &project_id,
                    &EventRow::added(repo.as_str(), &path_str, name, now_ms),
                )?;
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
        // A fresh project id for the first `added` event.
        let project_id = ProjectId::new(self.ulids.next_ulid()).to_string();
        self.append(
            &project_id,
            &EventRow::added(repo.as_str(), &canon_str, &label, now_ms),
        )
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
        self.append(&entry.project_id, &EventRow::removed(supersedes, now_ms))
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
        self.append(
            &entry.project_id,
            &EventRow::relabeled(label, supersedes, now_ms),
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
        self.append(
            &a.project_id,
            &EventRow::reordered(b.order_key, latest_event_id(&events, &a.project_id), now_ms),
        )?;
        self.append(
            &b.project_id,
            &EventRow::reordered(a.order_key, latest_event_id(&events, &b.project_id), now_ms),
        )
    }

    /// Append one typed event row for `project_id` — the **only** writer of
    /// `canopy_project_event`, so the column order is encoded exactly once,
    /// against [`EVENT_COLS`].
    fn append(&self, project_id: &str, row: &EventRow<'_>) -> Result<()> {
        let event_id = self.ulids.next_ulid().to_hex();
        let values = [
            Value::text(event_id),
            Value::text(project_id),
            Value::text(row.op),
            opt_text_ref(row.repo_id),
            opt_text_ref(row.path),
            opt_text_ref(row.label),
            row.order_key.map_or(Value::Null, Value::Int),
            opt_text(row.supersedes.clone()),
            Value::Int(row.created_at),
        ];
        let cols: Vec<&str> = EVENT_COLS.to_vec();
        runtime()?.block_on(self.store.insert("canopy_project_event", &cols, &values))
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
        let rows = runtime()?.block_on(self.store.query(sql, &[]))?;
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
        let row = ToggleRow {
            feature,
            desired,
            supersedes,
            created_at: now_ms,
        };
        let event_id = self.ulids.next_ulid().to_hex();
        let values = [
            Value::text(event_id),
            Value::text(&project_id),
            Value::text(row.feature),
            Value::Int(i64::from(row.desired)),
            opt_text(row.supersedes),
            Value::Int(row.created_at),
        ];
        let cols: Vec<&str> = TOGGLE_COLS.to_vec();
        runtime()?.block_on(self.store.insert("canopy_toggle_event", &cols, &values))
    }

    /// Read a project's toggle events into engine-neutral [`ToggleEvent`]s.
    fn toggle_events(&self, project_id: &str) -> Result<Vec<ToggleEvent>> {
        let sql = "SELECT event_id, feature, desired, created_at \
                   FROM canopy_toggle_event WHERE project_id = ? ORDER BY created_at, event_id";
        let rows = runtime()?.block_on(self.store.query(sql, &[Value::text(project_id)]))?;
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

    /// Append a per-project layout event (ADR-0035): `layout` is the encoded
    /// arrangement, `None` a reset — a superseding event either way. A no-op for
    /// an unknown path (like [`Self::set_toggle`]).
    fn set_layout(&self, path: &str, layout: Option<&str>, now_ms: i64) -> Result<()> {
        let Some(project_id) = self.project_id_for(path)? else {
            return Ok(());
        };
        let supersedes = self.latest_layout_event_id(&project_id)?;
        let event_id = self.ulids.next_ulid().to_hex();
        let values = [
            Value::text(event_id),
            Value::text(&project_id),
            opt_text_ref(layout),
            opt_text(supersedes),
            Value::Int(now_ms),
        ];
        let cols: Vec<&str> = LAYOUT_COLS.to_vec();
        runtime()?.block_on(self.store.insert("canopy_layout_event", &cols, &values))
    }

    /// The event_id of the latest layout event for a project (for `supersedes`).
    fn latest_layout_event_id(&self, project_id: &str) -> Result<Option<String>> {
        let sql = "SELECT event_id FROM canopy_layout_event WHERE project_id = ? \
                   ORDER BY created_at DESC, event_id DESC LIMIT 1";
        let rows = runtime()?.block_on(self.store.query(sql, &[Value::text(project_id)]))?;
        match rows.first() {
            Some(r) => Ok(Some(r.get_text(0)?)),
            None => Ok(None),
        }
    }

    /// The current saved layout for a project (ADR-0035): the latest event's
    /// arrangement, or `None` when the project is unknown, has no layout event, or
    /// its latest event is a reset — the caller then uses `LayoutModel::default`.
    /// A malformed/forward-version blob decodes to `None` (degrade to default).
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the layout log cannot be read.
    pub fn layout(&self, path: &str) -> Result<Option<rift_canopy::LayoutModel>> {
        let Some(project_id) = self.project_id_for(path)? else {
            return Ok(None);
        };
        let sql = "SELECT layout FROM canopy_layout_event WHERE project_id = ? \
                   ORDER BY created_at DESC, event_id DESC LIMIT 1";
        let rows = runtime()?.block_on(self.store.query(sql, &[Value::text(&project_id)]))?;
        let Some(r) = rows.first() else {
            return Ok(None);
        };
        match r.get_opt_text(0)? {
            Some(encoded) => Ok(rift_canopy::LayoutModel::decode(&encoded)),
            None => Ok(None), // a reset event → the default
        }
    }
}

/// A typed `canopy_project_event` row — the compile-checked shape of every
/// project-event insert. Constructed only through the per-op constructors, so a
/// column can never be filled positionally with the wrong value; the single
/// mapping onto [`EVENT_COLS`] lives in [`CanopyStore::append`].
struct EventRow<'a> {
    op: &'static str,
    repo_id: Option<&'a str>,
    path: Option<&'a str>,
    label: Option<&'a str>,
    order_key: Option<i64>,
    supersedes: Option<String>,
    created_at: i64,
}

impl<'a> EventRow<'a> {
    /// An `added` registration (fresh project).
    fn added(repo_id: &'a str, path: &'a str, label: &'a str, created_at: i64) -> Self {
        Self {
            op: "added",
            repo_id: Some(repo_id),
            path: Some(path),
            label: Some(label),
            order_key: None,
            supersedes: None,
            created_at,
        }
    }

    /// A superseding `removed` (hides the project; history kept).
    fn removed(supersedes: Option<String>, created_at: i64) -> Self {
        Self {
            op: "removed",
            repo_id: None,
            path: None,
            label: None,
            order_key: None,
            supersedes,
            created_at,
        }
    }

    /// A superseding `relabeled` (new display label).
    fn relabeled(label: &'a str, supersedes: Option<String>, created_at: i64) -> Self {
        Self {
            op: "relabeled",
            repo_id: None,
            path: None,
            label: Some(label),
            order_key: None,
            supersedes,
            created_at,
        }
    }

    /// A superseding `reordered` (new explicit order key).
    fn reordered(order_key: i64, supersedes: Option<String>, created_at: i64) -> Self {
        Self {
            op: "reordered",
            repo_id: None,
            path: None,
            label: None,
            order_key: Some(order_key),
            supersedes,
            created_at,
        }
    }
}

/// A typed `canopy_toggle_event` row (the columns after `event_id`/`project_id`),
/// mapped onto [`TOGGLE_COLS`] in [`CanopyStore::set_toggle`] only.
struct ToggleRow<'a> {
    feature: &'a str,
    desired: bool,
    supersedes: Option<String>,
    created_at: i64,
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

/// Nullable text value from an `Option<&str>`.
fn opt_text_ref(v: Option<&str>) -> Value {
    v.map_or(Value::Null, Value::text)
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

    /// Hand-build the exact `git worktree add` file layout under `repo`.
    fn add_worktree_fixture(repo: &Path, name: &str) -> String {
        let wt = repo.parent().unwrap().join(name);
        std::fs::create_dir_all(&wt).unwrap();
        let admin = repo.join(".git").join("worktrees").join(name);
        std::fs::create_dir_all(&admin).unwrap();
        let dot_git = wt.join(".git");
        std::fs::write(&dot_git, format!("gitdir: {}\n", admin.display())).unwrap();
        std::fs::write(admin.join("gitdir"), format!("{}\n", dot_git.display())).unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        wt.to_string_lossy().into_owned()
    }

    #[test]
    fn linked_worktrees_list_as_child_rows_under_their_repo() {
        let (dir, cs) = store();
        let repo = make_dir(dir.path(), "repo");
        std::fs::create_dir_all(std::path::Path::new(&repo).join(".git")).unwrap();
        let wt_path = add_worktree_fixture(std::path::Path::new(&repo), "feature-wt");
        cs.apply(&PersistOp::AddProject { path: repo.clone() }, 1)
            .unwrap();

        let projects = cs.projects().unwrap();
        assert_eq!(projects.len(), 2, "repo + its worktree child");
        assert_eq!(projects[0].path, repo);
        assert_eq!(projects[0].kind, EntryKind::Repo);
        assert_eq!(projects[1].name, "feature-wt");
        assert_eq!(projects[1].path, wt_path);
        assert_eq!(
            projects[1].kind,
            EntryKind::Worktree,
            "the child row carries the reserved Worktree kind"
        );
    }

    #[test]
    fn repo_without_worktrees_renders_exactly_as_today() {
        let (dir, cs) = store();
        let repo = make_dir(dir.path(), "plain");
        std::fs::create_dir_all(std::path::Path::new(&repo).join(".git")).unwrap();
        cs.apply(&PersistOp::AddProject { path: repo.clone() }, 1)
            .unwrap();
        let projects = cs.projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].kind, EntryKind::Repo);
    }

    #[test]
    fn a_registered_worktree_keeps_its_own_row_not_a_duplicate_child() {
        let (dir, cs) = store();
        let repo = make_dir(dir.path(), "repo");
        std::fs::create_dir_all(std::path::Path::new(&repo).join(".git")).unwrap();
        let wt_path = add_worktree_fixture(std::path::Path::new(&repo), "wt-own");
        cs.apply(&PersistOp::AddProject { path: repo.clone() }, 1)
            .unwrap();
        cs.apply(
            &PersistOp::AddProject {
                path: wt_path.clone(),
            },
            2,
        )
        .unwrap();

        let projects = cs.projects().unwrap();
        let wt_rows: Vec<_> = projects.iter().filter(|p| p.path == wt_path).collect();
        assert_eq!(
            wt_rows.len(),
            1,
            "no duplicate row for a registered worktree"
        );
        assert_eq!(
            wt_rows[0].kind,
            EntryKind::Repo,
            "its own registry row wins over the derived child"
        );
    }

    #[test]
    fn import_legacy_folds_in_once() {
        let (dir, cs) = store();
        let a = make_dir(dir.path(), "a");
        let b = make_dir(dir.path(), "b");
        let entries = vec![
            ("a".to_string(), std::path::PathBuf::from(&a)),
            ("b".to_string(), std::path::PathBuf::from(&b)),
        ];

        cs.import_legacy(&entries, 1).unwrap();
        assert_eq!(cs.projects().unwrap().len(), 2);
        let events_after_first = cs.events().unwrap().len();

        // A second import is a no-op (the log is already populated).
        cs.import_legacy(&entries, 2).unwrap();
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
    fn set_layout_appends_and_reads_back_the_latest_with_reset() {
        use rift_canopy::LayoutModel;
        let (dir, cs) = store();
        let path = make_dir(dir.path(), "lay");
        cs.apply(&PersistOp::AddProject { path: path.clone() }, 1)
            .unwrap();
        // No layout event yet → the default (loader returns None).
        assert_eq!(cs.layout(&path).unwrap(), None);

        // Save a rearranged layout (swap the first two columns).
        let mut m = LayoutModel::default();
        m.columns.swap(0, 1);
        cs.apply(
            &PersistOp::SetLayout {
                path: path.clone(),
                layout: Some(m.encode()),
            },
            2,
        )
        .unwrap();
        assert_eq!(cs.layout(&path).unwrap(), Some(m));

        // A reset is a NEW superseding event → the reader returns None (default).
        cs.apply(
            &PersistOp::SetLayout {
                path: path.clone(),
                layout: None,
            },
            3,
        )
        .unwrap();
        assert_eq!(
            cs.layout(&path).unwrap(),
            None,
            "reset supersedes to default"
        );

        // Both events are kept (append-only).
        let pid = cs.project_id_for(&path).unwrap().unwrap();
        assert!(cs.latest_layout_event_id(&pid).unwrap().is_some());
    }

    #[test]
    fn layout_for_unknown_path_is_a_noop() {
        let (dir, cs) = store();
        let ghost = dir.path().join("ghost").to_string_lossy().into_owned();
        cs.apply(
            &PersistOp::SetLayout {
                path: ghost.clone(),
                layout: Some("v1;20=rail".into()),
            },
            1,
        )
        .unwrap();
        assert_eq!(cs.layout(&ghost).unwrap(), None);
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
