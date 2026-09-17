//! The machine-global Canopy UI-state store: typed, append-only writers
//! and a fold reader over `canopy_project_event`.
//!
//! Canopy stays pure (`rift-core` only); *this* crate owns the SQLite side of
//! the dashboard's registry (the data-plane split). Every mutation the reducer emits as a
//! [`PersistOp`] becomes a new event row — add / remove / relabel / reorder are
//! all **inserts** (never `UPDATE`/`DELETE`), so the log is immutable by
//! construction and the current registry is the pure fold
//! [`rift_canopy::current_registry`] over it. Reads and the fold are engine-blind;
//! only this module touches `rift-store`.

use std::path::Path;

use rift_canopy::{
    active_profile_id, current_membership, current_profiles, current_registry, default_profile_id,
    normalize_profile_name, EntryKind, PersistOp, ProfileEntry, ProfileEvent, ProfileOp,
    ProfileProjectEvent, ProfileProjectOp, Project, ProjectEntry, ProjectEvent, ProjectOp,
    ProjectStatus, DEFAULT_PROFILE_NAME,
};
use rift_core::{MonotonicUlidSource, ProjectId, Result, RiftError, UlidSource};
use rift_git::repo_id_for;
use rift_store::{canopy_migrations, canopy_ui_db_under, CanonicalStore, SqliteStore, Value};

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

/// The [`CanopyError::Register`] for one failed step of a registration.
fn register_err(path: &str, step: &'static str, error: impl core::fmt::Display) -> RiftError {
    CanopyError::Register {
        path: path.to_owned(),
        step,
        error: error.to_string(),
    }
    .into()
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
        // The legacy file predates the family view and deduped by raw path, so it
        // can hold a repository AND one of its worktrees. Importing both would
        // recreate, on the very first launch, the duplicate the family view
        // removes — so the import takes one entry per family (the root where
        // the file names it, else the first entry, so the result is a pure
        // function of the file).
        for (name, path) in one_row_per_family(entries) {
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

    /// Register a project in the global catalogue (idempotent per canonical
    /// path) **and** include it in the active profile's view where that view is
    /// explicit (the profile plane: the unmaterialised default profile already shows
    /// the whole catalogue, so no membership event is written there).
    fn add_to_view(&self, path: &str, now_ms: i64) -> Result<()> {
        let project_id = self.add(path, now_ms)?;
        let pevents = self.profile_events()?;
        let Some(active) = active_profile_id(&pevents) else {
            return Ok(()); // unseeded store: pre-profile behavior
        };
        let is_default = default_profile_id(&pevents).as_deref() == Some(active.as_str());
        if is_default && self.membership_events(&active)?.is_empty() {
            return Ok(()); // the catalogue fallback already shows it
        }
        self.include_in_profile(&active, &project_id, None, now_ms)
    }

    /// Register a project in the global catalogue and return its stable id —
    /// the existing id when the canonical path is already present (idempotent),
    /// a fresh one after the first `added` event otherwise.
    fn add(&self, path: &str, now_ms: i64) -> Result<String> {
        // Every step names itself in its failure (`CanopyError::Register`):
        // the one message a person sees for a refused registration must say
        // which folder and which operation, or the next report is "it says
        // permission denied" with nothing to act on.
        let canon =
            std::fs::canonicalize(path).map_err(|e| register_err(path, "canonicalize", e))?;
        let canon_str = canon.to_string_lossy().into_owned();
        if !canon.is_dir() {
            return Err(register_err(&canon_str, "check", "not a directory"));
        }
        let registry = self
            .registry()
            .map_err(|e| register_err(&canon_str, "read the registry", e))?;
        // Idempotent per canonical path: a present project is not re-added —
        // whatever spelling its row was written with (a legacy import wrote
        // whatever the TOML said).
        if let Some(existing) = registry.iter().find(|e| comparable(&e.path) == canon) {
            return Ok(existing.project_id.clone());
        }
        // the family view — **one row per repository family**. A linked worktree is
        // a second path to one repository, not a second repository: registering
        // it recreated in the UI the double identity the family rule removed from
        // storage. Two arms, and the difference between them is the whole rule:
        //
        // * the family root arriving over a member's row **supersedes** it —
        //   `removed` then `added`, two events, no deletion (the log is
        //   append-only and a mistaken registration is history too);
        // * anything else is REFUSED. Not an `Ok` no-op: `apply_persist` fires
        //   the success toast on `Ok` and opens the alert on `Err`, so a silent
        //   no-op would tell the user their worktree had been registered.
        if let Some(family) = family_of(&canon) {
            if let Some(existing) = registry.iter().find(|e| family.holds(&e.path)) {
                if family.is_root(&canon) && !family.is_root(&comparable(&existing.path)) {
                    self.remove(&existing.path, now_ms)?;
                } else {
                    return Err(CanopyError::AlreadyRegisteredAs(existing.path.clone()).into());
                }
            }
        }
        let repo = repo_id_for(&canon).map_err(|e| register_err(&canon_str, "repository id", e))?;
        let label = canon
            .file_name()
            .map_or_else(|| canon_str.clone(), |n| n.to_string_lossy().into_owned());
        // A fresh project id for the first `added` event.
        let project_id = ProjectId::new(self.ulids.next_ulid()).to_string();
        self.append(
            &project_id,
            &EventRow::added(repo.as_str(), &canon_str, &label, now_ms),
        )
        .map_err(|e| register_err(&canon_str, "append to ui-state.db", e))?;
        Ok(project_id)
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

    /// The saved **legacy** per-project layout: the latest event's
    /// arrangement, or `None` when the project is unknown, has no layout event, or
    /// its latest event is a reset — the caller then uses `LayoutModel::default`.
    /// A malformed/forward-version blob decodes to `None` (degrade to default).
    /// The table is **frozen for writes** since the profile plane (all new arrangement
    /// writes are profile-scoped); this reader is the default profile's fallback.
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

    // --- the profile plane ----------------------------------------

    /// Seed the default profile on first launch (the `import_legacy` pattern):
    /// when the profile log is empty, append one `created` event named
    /// `default` plus one `selected` event — ordinary rows, so every fold stays
    /// a pure function of the log. A no-op once any profile event exists (a
    /// second launch appends nothing).
    ///
    /// # Errors
    /// [`RiftError`] if the log cannot be read or an event cannot be written.
    pub fn seed_default_profile(&self, now_ms: i64) -> Result<()> {
        if !self.profile_events()?.is_empty() {
            return Ok(());
        }
        let profile_id = self.ulids.next_ulid().to_hex();
        self.append_profile(
            &profile_id,
            &ProfileRow::created(DEFAULT_PROFILE_NAME, now_ms),
        )?;
        self.append_profile(&profile_id, &ProfileRow::selected(None, now_ms))
    }

    /// The live profiles plus the active profile's id: the fold over
    /// the profile log. The active id is `None` only on an unseeded store.
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the log cannot be read.
    pub fn profiles_view(&self) -> Result<(Option<String>, Vec<ProfileEntry>)> {
        let events = self.profile_events()?;
        Ok((active_profile_id(&events), current_profiles(&events)))
    }

    /// The active profile's effective project view — what the dashboard shows.
    /// Falls back to the global catalogue on an unseeded store (pre-profile
    /// behavior, so nothing breaks before the binary seeds).
    ///
    /// # Errors
    /// [`RiftError::Storage`] if a log cannot be read.
    pub fn active_projects(&self) -> Result<Vec<Project>> {
        let pevents = self.profile_events()?;
        match active_profile_id(&pevents) {
            Some(active) => Ok(view_with_worktrees(
                self.effective_entries(&active, &pevents)?,
            )),
            None => self.projects(),
        }
    }

    /// One profile's effective project view, projected like
    /// [`Self::projects`] (worktree children derived, statuses start `Idle`).
    ///
    /// # Errors
    /// [`RiftError::Storage`] if a log cannot be read.
    pub fn profile_projects(&self, profile: &str) -> Result<Vec<Project>> {
        let pevents = self.profile_events()?;
        Ok(view_with_worktrees(
            self.effective_entries(profile, &pevents)?,
        ))
    }

    /// The saved arrangement for `(profile, project)`: the latest
    /// profile-scoped event wins (`None` for a reset or a malformed blob); with
    /// **no** profile-scoped row, the default profile falls back to the legacy
    /// per-project fold ([`Self::layout`]) so every legacy arrangement survives —
    /// other profiles start canonical instead.
    ///
    /// # Errors
    /// [`RiftError::Storage`] if a log cannot be read.
    pub fn profile_layout(
        &self,
        profile: &str,
        path: &str,
    ) -> Result<Option<rift_canopy::LayoutModel>> {
        let Some(project_id) = self.project_id_for(path)? else {
            return Ok(None);
        };
        let sql = "SELECT layout FROM canopy_profile_layout_event \
                   WHERE profile_id = ? AND project_id = ? \
                   ORDER BY created_at DESC, event_id DESC LIMIT 1";
        let rows = runtime()?.block_on(
            self.store
                .query(sql, &[Value::text(profile), Value::text(&project_id)]),
        )?;
        if let Some(r) = rows.first() {
            return Ok(match r.get_opt_text(0)? {
                Some(encoded) => rift_canopy::LayoutModel::decode(&encoded),
                None => None, // an explicit reset supersedes any fallback
            });
        }
        // No profile-scoped row: only the default profile reads the legacy fold.
        let pevents = self.profile_events()?;
        if default_profile_id(&pevents).as_deref() == Some(profile) {
            return self.layout(path);
        }
        Ok(None)
    }

    /// Mint a new profile (a `created` event) and copy the creating (active)
    /// profile's **effective** project list as explicit membership events at
    /// creation time (the profile plane) — auditable, frozen at that moment, free to
    /// diverge after. Returns the new profile's id.
    ///
    /// # Errors
    /// [`RiftError::Validity`] when the name breaks the rules (trimmed,
    /// non-empty, ≤ 24 chars, unique among live profiles).
    pub fn create_profile(&self, name: &str, now_ms: i64) -> Result<String> {
        let pevents = self.profile_events()?;
        let live = current_profiles(&pevents);
        let Some(name) = normalize_profile_name(name, &live, None) else {
            return Err(RiftError::validity(format!(
                "invalid or duplicate profile name {name:?}"
            )));
        };
        let source = active_profile_id(&pevents);
        let profile_id = self.ulids.next_ulid().to_hex();
        self.append_profile(&profile_id, &ProfileRow::created(&name, now_ms))?;
        if let Some(source) = source {
            for e in self.effective_entries(&source, &pevents)? {
                self.include_in_profile(&profile_id, &e.project_id, Some(e.order_key), now_ms)?;
            }
        }
        Ok(profile_id)
    }

    /// Duplicate a live profile under a new name — a **full clone of its
    /// effective state at clone time**: the source's effective
    /// project list as explicit membership `added` events in source order,
    /// each member's effective arrangement (read through
    /// [`Self::profile_layout`], so a clone of the default profile captures
    /// its legacy-fold fallback), and each theme slot the source's fold
    /// resolves to `Some` — absence already means default, so no NULL resets
    /// are written. **Never writes `selected`**: cloning is not switching.
    /// Returns the new profile's id.
    ///
    /// # Errors
    /// [`RiftError::Validity`] for an unknown/removed source or a name that
    /// breaks the rules (trimmed, non-empty, ≤ 24 chars, unique among live).
    pub fn duplicate_profile(&self, source: &str, name: &str, now_ms: i64) -> Result<String> {
        let pevents = self.profile_events()?;
        let live = current_profiles(&pevents);
        if !live.iter().any(|p| p.profile_id == source) {
            return Err(RiftError::validity(format!("unknown profile {source:?}")));
        }
        let Some(name) = normalize_profile_name(name, &live, None) else {
            return Err(RiftError::validity(format!(
                "invalid or duplicate profile name {name:?}"
            )));
        };
        let entries = self.effective_entries(source, &pevents)?;
        let profile_id = self.ulids.next_ulid().to_hex();
        self.append_profile(&profile_id, &ProfileRow::created(&name, now_ms))?;
        for e in &entries {
            self.include_in_profile(&profile_id, &e.project_id, Some(e.order_key), now_ms)?;
        }
        for e in &entries {
            if let Some(m) = self.profile_layout(source, &e.path)? {
                self.set_profile_layout(&profile_id, &e.path, Some(&m.encode()), now_ms)?;
            }
        }
        let overrides = self.profile_theme(source)?;
        for slot in rift_canopy::ThemeSlot::ALL {
            if let Some(hex) = overrides.get(slot).and_then(rift_canopy::colour_to_hex) {
                self.set_profile_theme(&profile_id, slot.key(), Some(&hex), now_ms)?;
            }
        }
        Ok(profile_id)
    }

    /// Rename a live profile (a superseding `renamed` event).
    ///
    /// # Errors
    /// [`RiftError::Validity`] for an unknown profile or a name that breaks the
    /// rules (rename-to-its-own-name passes).
    pub fn rename_profile(&self, profile: &str, name: &str, now_ms: i64) -> Result<()> {
        let pevents = self.profile_events()?;
        let live = current_profiles(&pevents);
        if !live.iter().any(|p| p.profile_id == profile) {
            return Err(RiftError::validity(format!("unknown profile {profile:?}")));
        }
        let Some(name) = normalize_profile_name(name, &live, Some(profile)) else {
            return Err(RiftError::validity(format!(
                "invalid or duplicate profile name {name:?}"
            )));
        };
        let supersedes = latest_profile_event_id(&pevents, profile);
        self.append_profile(profile, &ProfileRow::renamed(&name, supersedes, now_ms))
    }

    /// Remove a profile (a superseding `removed` event, never a row deletion).
    /// The reducer refuses first; the store refuses **again** (the profile plane):
    /// the active profile and the last live profile cannot be removed. An
    /// already-removed/unknown profile is a no-op.
    ///
    /// # Errors
    /// [`RiftError::Validity`] when the removal guard refuses.
    pub fn remove_profile(&self, profile: &str, now_ms: i64) -> Result<()> {
        let pevents = self.profile_events()?;
        let live = current_profiles(&pevents);
        if !live.iter().any(|p| p.profile_id == profile) {
            return Ok(()); // unknown or already removed → nothing to hide
        }
        if active_profile_id(&pevents).as_deref() == Some(profile) {
            return Err(RiftError::validity(
                "cannot remove the active profile".to_string(),
            ));
        }
        if live.len() <= 1 {
            return Err(RiftError::validity(
                "cannot remove the last profile".to_string(),
            ));
        }
        let supersedes = latest_profile_event_id(&pevents, profile);
        self.append_profile(profile, &ProfileRow::removed(supersedes, now_ms))
    }

    /// Make a live profile the active one (a superseding `selected` event; the
    /// latest fold names the active profile).
    ///
    /// # Errors
    /// [`RiftError::Validity`] for an unknown or removed profile.
    pub fn select_profile(&self, profile: &str, now_ms: i64) -> Result<()> {
        let pevents = self.profile_events()?;
        if !current_profiles(&pevents)
            .iter()
            .any(|p| p.profile_id == profile)
        {
            return Err(RiftError::validity(format!("unknown profile {profile:?}")));
        }
        let supersedes = latest_profile_event_id(&pevents, profile);
        self.append_profile(profile, &ProfileRow::selected(supersedes, now_ms))
    }

    /// Include a catalogue project in a profile's view (this profile only). A
    /// no-op for an unknown path or a project already in the view; the default
    /// profile materialises its catalogue view first.
    fn add_profile_project(&self, profile: &str, path: &str, now_ms: i64) -> Result<()> {
        let Some(project_id) = self.project_id_for(path)? else {
            return Ok(());
        };
        self.materialize_default_if_needed(profile, now_ms)?;
        let membership = self.membership_events(profile)?;
        if current_membership(&membership)
            .iter()
            .any(|m| m.project_id == project_id)
        {
            return Ok(()); // already in this profile's view
        }
        self.include_in_profile(profile, &project_id, None, now_ms)
    }

    /// Hide a project from a profile's view (this profile only — the global
    /// catalogue and every other profile are untouched). A no-op for an unknown
    /// path or a project not in the view; the default profile materialises its
    /// catalogue view first (copy-on-first-divergence).
    fn remove_profile_project(&self, profile: &str, path: &str, now_ms: i64) -> Result<()> {
        let Some(project_id) = self.project_id_for(path)? else {
            return Ok(());
        };
        self.materialize_default_if_needed(profile, now_ms)?;
        let membership = self.membership_events(profile)?;
        if !current_membership(&membership)
            .iter()
            .any(|m| m.project_id == project_id)
        {
            return Ok(()); // not in this profile's view
        }
        let supersedes = latest_membership_event_id(&membership, &project_id);
        self.append_membership(
            profile,
            &project_id,
            &MembershipRow::removed(supersedes, now_ms),
        )
    }

    /// Move a project within a profile's view (two superseding `reordered`
    /// events swapping the neighbours' effective keys, like the catalogue
    /// reorder). Off-the-end moves and unknown paths are no-ops.
    fn reorder_profile_project(
        &self,
        profile: &str,
        path: &str,
        up: bool,
        now_ms: i64,
    ) -> Result<()> {
        let Some(project_id) = self.project_id_for(path)? else {
            return Ok(());
        };
        self.materialize_default_if_needed(profile, now_ms)?;
        let membership = self.membership_events(profile)?;
        let view = current_membership(&membership);
        let Some(i) = view.iter().position(|m| m.project_id == project_id) else {
            return Ok(());
        };
        let Some(j) = neighbor(i, up, view.len()) else {
            return Ok(());
        };
        let (Some(a), Some(b)) = (view.get(i), view.get(j)) else {
            return Ok(()); // unreachable: position()/neighbor() are in-bounds
        };
        self.append_membership(
            profile,
            &a.project_id,
            &MembershipRow::reordered(
                b.order_key,
                latest_membership_event_id(&membership, &a.project_id),
                now_ms,
            ),
        )?;
        self.append_membership(
            profile,
            &b.project_id,
            &MembershipRow::reordered(
                a.order_key,
                latest_membership_event_id(&membership, &b.project_id),
                now_ms,
            ),
        )
    }

    /// One profile's effective project entries: the membership fold joined to
    /// the catalogue — except the **default profile with no membership events**,
    /// which shows the full catalogue (the profile plane), so CLI registrations keep
    /// appearing until its first membership edit.
    fn effective_entries(
        &self,
        profile: &str,
        pevents: &[ProfileEvent],
    ) -> Result<Vec<ProjectEntry>> {
        let membership = self.membership_events(profile)?;
        let is_default = default_profile_id(pevents).as_deref() == Some(profile);
        if is_default && membership.is_empty() {
            return self.registry();
        }
        let catalogue = self.registry()?;
        let mut out = Vec::new();
        for m in current_membership(&membership) {
            // A project removed from the catalogue drops out of every view.
            if let Some(e) = catalogue.iter().find(|e| e.project_id == m.project_id) {
                let mut e = e.clone();
                e.order_key = m.order_key;
                out.push(e);
            }
        }
        Ok(out)
    }

    /// Materialise the default profile's implicit catalogue view as explicit
    /// membership events (copy-on-first-divergence, the profile plane). A no-op for
    /// non-default profiles or a default that already has membership events.
    fn materialize_default_if_needed(&self, profile: &str, now_ms: i64) -> Result<()> {
        let pevents = self.profile_events()?;
        if default_profile_id(&pevents).as_deref() != Some(profile) {
            return Ok(());
        }
        if !self.membership_events(profile)?.is_empty() {
            return Ok(());
        }
        for e in self.registry()? {
            self.include_in_profile(profile, &e.project_id, Some(e.order_key), now_ms)?;
        }
        Ok(())
    }

    /// Append one membership `added` event (the single include writer, so the
    /// copy paths and the edit paths cannot diverge).
    fn include_in_profile(
        &self,
        profile: &str,
        project_id: &str,
        order_key: Option<i64>,
        now_ms: i64,
    ) -> Result<()> {
        self.append_membership(
            profile,
            project_id,
            &MembershipRow::added(order_key, now_ms),
        )
    }

    /// Append one typed profile event row — the **only** writer of
    /// `canopy_profile_event`, mapped onto [`PROFILE_COLS`] exactly once.
    fn append_profile(&self, profile_id: &str, row: &ProfileRow<'_>) -> Result<()> {
        let event_id = self.ulids.next_ulid().to_hex();
        let values = [
            Value::text(event_id),
            Value::text(profile_id),
            Value::text(row.op),
            opt_text_ref(row.name),
            opt_text(row.supersedes.clone()),
            Value::Int(row.created_at),
        ];
        let cols: Vec<&str> = PROFILE_COLS.to_vec();
        runtime()?.block_on(self.store.insert("canopy_profile_event", &cols, &values))
    }

    /// Append one typed membership event row — the **only** writer of
    /// `canopy_profile_project_event`, mapped onto [`PROFILE_PROJECT_COLS`]
    /// exactly once.
    fn append_membership(
        &self,
        profile_id: &str,
        project_id: &str,
        row: &MembershipRow,
    ) -> Result<()> {
        let event_id = self.ulids.next_ulid().to_hex();
        let values = [
            Value::text(event_id),
            Value::text(profile_id),
            Value::text(project_id),
            Value::text(row.op),
            row.order_key.map_or(Value::Null, Value::Int),
            opt_text(row.supersedes.clone()),
            Value::Int(row.created_at),
        ];
        let cols: Vec<&str> = PROFILE_PROJECT_COLS.to_vec();
        runtime()?.block_on(
            self.store
                .insert("canopy_profile_project_event", &cols, &values),
        )
    }

    /// Append a per-(profile, project) layout event: `layout` is the
    /// encoded arrangement, `None` a reset — a superseding event either way.
    /// All new arrangement writes land here; the legacy per-project table is
    /// frozen for writes. A no-op for an unknown path.
    fn set_profile_layout(
        &self,
        profile: &str,
        path: &str,
        layout: Option<&str>,
        now_ms: i64,
    ) -> Result<()> {
        let Some(project_id) = self.project_id_for(path)? else {
            return Ok(());
        };
        let supersedes = self.latest_profile_layout_event_id(profile, &project_id)?;
        let event_id = self.ulids.next_ulid().to_hex();
        let values = [
            Value::text(event_id),
            Value::text(profile),
            Value::text(&project_id),
            opt_text_ref(layout),
            opt_text(supersedes),
            Value::Int(now_ms),
        ];
        let cols: Vec<&str> = PROFILE_LAYOUT_COLS.to_vec();
        runtime()?.block_on(
            self.store
                .insert("canopy_profile_layout_event", &cols, &values),
        )
    }

    /// The event_id of the latest layout event for `(profile, project)` (for
    /// `supersedes`).
    fn latest_profile_layout_event_id(
        &self,
        profile: &str,
        project_id: &str,
    ) -> Result<Option<String>> {
        let sql = "SELECT event_id FROM canopy_profile_layout_event \
                   WHERE profile_id = ? AND project_id = ? \
                   ORDER BY created_at DESC, event_id DESC LIMIT 1";
        let rows = runtime()?.block_on(
            self.store
                .query(sql, &[Value::text(profile), Value::text(project_id)]),
        )?;
        match rows.first() {
            Some(r) => Ok(Some(r.get_text(0)?)),
            None => Ok(None),
        }
    }

    /// Read every profile event row into the engine-neutral [`ProfileEvent`],
    /// ordered by transaction time then event id (folds re-sort defensively).
    fn profile_events(&self) -> Result<Vec<ProfileEvent>> {
        let sql = "SELECT event_id, profile_id, op, name, created_at \
                   FROM canopy_profile_event ORDER BY created_at, event_id";
        let rows = runtime()?.block_on(self.store.query(sql, &[]))?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let event_id = r.get_text(0)?;
            let profile_id = r.get_text(1)?;
            let op = r.get_text(2)?;
            let name = r.get_opt_text(3)?;
            let created_at = r.get_i64(4)?;
            out.push(ProfileEvent {
                event_id,
                profile_id,
                op: decode_profile_op(&op, name)?,
                created_at,
            });
        }
        Ok(out)
    }

    /// Append one theme-slot override event: `colour` is resolved
    /// `#RRGGBB` hex, `None` a reset — a superseding event per (profile, slot)
    /// either way. The slot key and the hex are validated at this writer (the
    /// early "CHECK deferred" posture).
    ///
    /// # Errors
    /// [`RiftError::Validity`] for an unknown slot or a malformed colour.
    pub fn set_profile_theme(
        &self,
        profile: &str,
        slot: &str,
        colour: Option<&str>,
        now_ms: i64,
    ) -> Result<()> {
        if rift_canopy::ThemeSlot::from_key(slot).is_none() {
            return Err(RiftError::validity(format!("unknown theme slot {slot:?}")));
        }
        if let Some(hex) = colour {
            if rift_canopy::parse_hex_colour(hex).is_none() {
                return Err(RiftError::validity(format!(
                    "malformed theme colour {hex:?} (want #RRGGBB)"
                )));
            }
        }
        let supersedes = self.latest_theme_event_id(profile, slot)?;
        let event_id = self.ulids.next_ulid().to_hex();
        let values = [
            Value::text(event_id),
            Value::text(profile),
            Value::text(slot),
            opt_text_ref(colour),
            opt_text(supersedes),
            Value::Int(now_ms),
        ];
        let cols: Vec<&str> = PROFILE_THEME_COLS.to_vec();
        runtime()?.block_on(
            self.store
                .insert("canopy_profile_theme_event", &cols, &values),
        )
    }

    /// One profile's theme-slot overrides: latest-wins per
    /// (profile, slot), folded into a [`rift_canopy::ThemeOverrides`]. A NULL
    /// (reset) or malformed latest row leaves its slot at the default, so a
    /// hostile blob can never break the render — Rift dark is total.
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the theme log cannot be read.
    pub fn profile_theme(&self, profile: &str) -> Result<rift_canopy::ThemeOverrides> {
        let sql = "SELECT slot, colour FROM canopy_profile_theme_event \
                   WHERE profile_id = ? ORDER BY created_at, event_id";
        let rows = runtime()?.block_on(self.store.query(sql, &[Value::text(profile)]))?;
        let mut overrides = rift_canopy::ThemeOverrides::NONE;
        for r in rows {
            let slot = r.get_text(0)?;
            let colour = r.get_opt_text(1)?;
            if let Some(slot) = rift_canopy::ThemeSlot::from_key(&slot) {
                // Replayed in time order, so the latest event per slot wins.
                overrides.set(slot, colour.and_then(|h| rift_canopy::parse_hex_colour(&h)));
            }
        }
        Ok(overrides)
    }

    /// The event_id of the latest theme event for `(profile, slot)` (for
    /// `supersedes`).
    fn latest_theme_event_id(&self, profile: &str, slot: &str) -> Result<Option<String>> {
        let sql = "SELECT event_id FROM canopy_profile_theme_event \
                   WHERE profile_id = ? AND slot = ? \
                   ORDER BY created_at DESC, event_id DESC LIMIT 1";
        let rows = runtime()?.block_on(
            self.store
                .query(sql, &[Value::text(profile), Value::text(slot)]),
        )?;
        match rows.first() {
            Some(r) => Ok(Some(r.get_text(0)?)),
            None => Ok(None),
        }
    }

    /// Pin — or with `None` unpin — the startup profile: one
    /// superseding latest-wins event. Pinning refuses a dead or unknown
    /// profile at write time; unpinning always lands.
    ///
    /// # Errors
    /// [`RiftError::Validity`] for a dead pin; [`RiftError::Storage`] on a
    /// write failure.
    pub fn set_startup_profile(&self, profile: Option<&str>, now_ms: i64) -> Result<()> {
        if let Some(p) = profile {
            let pevents = self.profile_events()?;
            if !current_profiles(&pevents).iter().any(|e| e.profile_id == p) {
                return Err(RiftError::validity(format!("unknown profile {p:?}")));
            }
        }
        let supersedes = self.latest_startup_event_id()?;
        let event_id = self.ulids.next_ulid().to_hex();
        let values = [
            Value::text(event_id),
            opt_text_ref(profile),
            opt_text(supersedes),
            Value::Int(now_ms),
        ];
        let cols: Vec<&str> = STARTUP_COLS.to_vec();
        runtime()?.block_on(
            self.store
                .insert("canopy_profile_startup_event", &cols, &values),
        )
    }

    /// The pinned startup profile: the latest-wins fold, `None`
    /// when unpinned. Liveness is deliberately resolved at launch
    /// ([`rift_canopy::resolve_launch_profile`]), not here — a dead pin stays
    /// in the fold and simply loses at resolution.
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the startup log cannot be read.
    pub fn startup_profile(&self) -> Result<Option<String>> {
        Ok(rift_canopy::startup_profile_id(&self.startup_events()?))
    }

    /// Read every startup-pin event row into the engine-neutral
    /// [`rift_canopy::ProfileStartupEvent`].
    fn startup_events(&self) -> Result<Vec<rift_canopy::ProfileStartupEvent>> {
        let sql = "SELECT event_id, profile_id, created_at \
                   FROM canopy_profile_startup_event ORDER BY created_at, event_id";
        let rows = runtime()?.block_on(self.store.query(sql, &[]))?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(rift_canopy::ProfileStartupEvent {
                event_id: r.get_text(0)?,
                profile_id: r.get_opt_text(1)?,
                created_at: r.get_i64(2)?,
            });
        }
        Ok(out)
    }

    /// The event_id of the latest startup event (for `supersedes`).
    fn latest_startup_event_id(&self) -> Result<Option<String>> {
        let sql = "SELECT event_id FROM canopy_profile_startup_event \
                   ORDER BY created_at DESC, event_id DESC LIMIT 1";
        let rows = runtime()?.block_on(self.store.query(sql, &[]))?;
        match rows.first() {
            Some(r) => Ok(Some(r.get_text(0)?)),
            None => Ok(None),
        }
    }

    /// The pin-aware launch view: the resolved launch profile's
    /// effective projects — the pinned profile when live, else the
    /// last-selected chain; the catalogue on an unseeded store. One helper, so
    /// the binary's injected rows and the loop's launch load resolve
    /// identically, and launch stays a pure read.
    ///
    /// # Errors
    /// [`RiftError::Storage`] if a log cannot be read.
    pub fn launch_projects(&self) -> Result<Vec<Project>> {
        let pevents = self.profile_events()?;
        let pinned = self.startup_profile()?;
        match rift_canopy::resolve_launch_profile(pinned.as_deref(), &pevents) {
            Some(p) => Ok(view_with_worktrees(self.effective_entries(&p, &pevents)?)),
            None => self.projects(),
        }
    }

    /// Read one profile's membership event rows into the engine-neutral
    /// [`ProfileProjectEvent`].
    fn membership_events(&self, profile: &str) -> Result<Vec<ProfileProjectEvent>> {
        let sql = "SELECT event_id, project_id, op, order_key, created_at \
                   FROM canopy_profile_project_event WHERE profile_id = ? \
                   ORDER BY created_at, event_id";
        let rows = runtime()?.block_on(self.store.query(sql, &[Value::text(profile)]))?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let event_id = r.get_text(0)?;
            let project_id = r.get_text(1)?;
            let op = r.get_text(2)?;
            let order_key = r.get_opt_i64(3)?;
            let created_at = r.get_i64(4)?;
            out.push(ProfileProjectEvent {
                event_id,
                project_id,
                op: decode_membership_op(&op, order_key)?,
                created_at,
            });
        }
        Ok(out)
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

/// A typed `canopy_profile_event` row — constructed only through the
/// per-op constructors; the single mapping onto [`PROFILE_COLS`] lives in
/// [`CanopyStore::append_profile`].
struct ProfileRow<'a> {
    op: &'static str,
    name: Option<&'a str>,
    supersedes: Option<String>,
    created_at: i64,
}

impl<'a> ProfileRow<'a> {
    /// A `created` mint (fresh profile).
    fn created(name: &'a str, created_at: i64) -> Self {
        Self {
            op: "created",
            name: Some(name),
            supersedes: None,
            created_at,
        }
    }

    /// A superseding `renamed` (new display name).
    fn renamed(name: &'a str, supersedes: Option<String>, created_at: i64) -> Self {
        Self {
            op: "renamed",
            name: Some(name),
            supersedes,
            created_at,
        }
    }

    /// A superseding `removed` (hides the profile; history kept).
    fn removed(supersedes: Option<String>, created_at: i64) -> Self {
        Self {
            op: "removed",
            name: None,
            supersedes,
            created_at,
        }
    }

    /// A superseding `selected` (names the active profile).
    fn selected(supersedes: Option<String>, created_at: i64) -> Self {
        Self {
            op: "selected",
            name: None,
            supersedes,
            created_at,
        }
    }
}

/// A typed `canopy_profile_project_event` row — the single mapping
/// onto [`PROFILE_PROJECT_COLS`] lives in [`CanopyStore::append_membership`].
struct MembershipRow {
    op: &'static str,
    order_key: Option<i64>,
    supersedes: Option<String>,
    created_at: i64,
}

impl MembershipRow {
    /// An `added` inclusion (explicit key for copies; `None` sorts by time).
    fn added(order_key: Option<i64>, created_at: i64) -> Self {
        Self {
            op: "added",
            order_key,
            supersedes: None,
            created_at,
        }
    }

    /// A superseding `removed` (hides the project from this view).
    fn removed(supersedes: Option<String>, created_at: i64) -> Self {
        Self {
            op: "removed",
            order_key: None,
            supersedes,
            created_at,
        }
    }

    /// A superseding `reordered` (new explicit order key within this view).
    fn reordered(order_key: i64, supersedes: Option<String>, created_at: i64) -> Self {
        Self {
            op: "reordered",
            order_key: Some(order_key),
            supersedes,
            created_at,
        }
    }
}

/// Decode a stored profile `op` + its `name` column into a typed [`ProfileOp`].
/// An unknown op or a created/renamed missing its name is a data error.
fn decode_profile_op(op: &str, name: Option<String>) -> Result<ProfileOp> {
    match op {
        "created" => {
            let name = name.ok_or_else(|| RiftError::storage("created event missing name"))?;
            Ok(ProfileOp::Created { name })
        }
        "renamed" => {
            let name = name.ok_or_else(|| RiftError::storage("renamed event missing name"))?;
            Ok(ProfileOp::Renamed { name })
        }
        "removed" => Ok(ProfileOp::Removed),
        "selected" => Ok(ProfileOp::Selected),
        other => Err(RiftError::storage(format!("unknown profile op {other:?}"))),
    }
}

/// Decode a stored membership `op` + its `order_key` into a typed
/// [`ProfileProjectOp`]. An unknown op or a reordered missing its key is a
/// data error.
fn decode_membership_op(op: &str, order_key: Option<i64>) -> Result<ProfileProjectOp> {
    match op {
        "added" => Ok(ProfileProjectOp::Added { order_key }),
        "removed" => Ok(ProfileProjectOp::Removed),
        "reordered" => {
            let order_key =
                order_key.ok_or_else(|| RiftError::storage("reordered event missing order_key"))?;
            Ok(ProfileProjectOp::Reordered { order_key })
        }
        other => Err(RiftError::storage(format!(
            "unknown profile-project op {other:?}"
        ))),
    }
}

/// The event_id of the latest profile event recorded for `profile_id` (events
/// arrive sorted), or `None` — the `supersedes` back-pointer.
fn latest_profile_event_id(events: &[ProfileEvent], profile_id: &str) -> Option<String> {
    events
        .iter()
        .rev()
        .find(|e| e.profile_id == profile_id)
        .map(|e| e.event_id.clone())
}

/// The event_id of the latest membership event for `project_id` within one
/// profile's log (events arrive sorted), or `None`.
fn latest_membership_event_id(events: &[ProfileProjectEvent], project_id: &str) -> Option<String> {
    events
        .iter()
        .rev()
        .find(|e| e.project_id == project_id)
        .map(|e| e.event_id.clone())
}

/// Project a list of registry entries onto Canopy's [`Project`] view (statuses
/// start `Idle`; the live bus refines them), with each repository's **linked
/// worktrees** listed as child rows directly under it. Worktree rows
/// are derived from git — never persisted; a worktree that is itself a listed
/// entry keeps its own row instead. Shared by the catalogue and every profile
/// view, so the child-row semantics can never diverge between them.
///
/// The children come from `family_members` rather than `list_worktrees`
/// (the family view, which refines that listing here). Two consequences, both deliberate:
///
/// * A **`prunable`** member — the checkout deleted, the administrative entry
///   still there — now has a row. `list_worktrees` skips it on the grounds that
///   "a row for a nonexistent path would be an invented lie", which was right
///   while nothing could say what was wrong with it; the bus now reads such a
///   row as `Error(RepoMissing)` whose hint names the one act that clears it,
///   and a member the operator half-removed is a fact worth showing rather than
///   one worth hiding.
/// * Both sides of the "it has its own registry row" comparison are now
///   **canonical**: `CanopyStore::add` canonicalises a registered path on the
///   way in, and `family_members` canonicalises each checkout, where
///   `list_worktrees` returned whatever `<admin>/gitdir` happened to spell. A
///   worktree added through a symlink or a path with a `..` segment therefore
///   used to get BOTH its own registry row and a duplicate child under its
///   repository — the self-comparison defect, in the one place it had
///   not yet been closed.
fn view_with_worktrees(entries: Vec<ProjectEntry>) -> Vec<Project> {
    let listed: std::collections::BTreeSet<std::path::PathBuf> = entries
        .iter()
        .map(|e| std::path::PathBuf::from(&e.path))
        .collect();
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
        for member in rift_git::family_members(&paths) {
            if member.is_main {
                continue; // the repository's own row, already pushed
            }
            if listed.contains(&member.work_tree) {
                continue; // it has its own registry row
            }
            out.push(Project {
                name: member.name,
                path: member.work_tree.to_string_lossy().into_owned(),
                status: ProjectStatus::Idle,
                kind: EntryKind::Worktree,
            });
        }
    }
    out
}

/// A path as the registry compares it: canonical where the directory resolves,
/// the recorded spelling where it does not (an unmounted volume still has a
/// row, and no canonical form can be invented for it).
fn comparable(path: impl AsRef<Path>) -> std::path::PathBuf {
    rift_git::family::canonical_or_lexical(path.as_ref())
}

/// The repository family a checkout belongs to, as the one-row-per-family rule
/// needs to see it (the family view).
///
/// The family **is** its common dir: two paths are checkouts of one
/// repository exactly when they share it. Resolved by discovery only — no
/// manifest is read and none is minted, because registering a project must not
/// write into the repository being registered.
struct FamilyOf {
    /// The canonical common dir: this family's identity.
    common_dir: std::path::PathBuf,
    /// Every checkout of the family, canonical: the main work tree and each
    /// linked worktree, including one whose directory is gone (it still holds
    /// a row's worth of identity).
    members: Vec<std::path::PathBuf>,
    /// The main work tree, where the family has a provable one.
    main: Option<std::path::PathBuf>,
}

impl FamilyOf {
    /// Whether a registered row's `path` is a checkout of this family.
    fn holds(&self, path: &str) -> bool {
        let canonical = comparable(path);
        self.members.contains(&canonical)
    }

    /// Whether `path` is the family's main work tree — the one path the family view
    /// lets supersede a member's row.
    fn is_root(&self, path: &Path) -> bool {
        self.main.as_deref() == Some(path)
    }
}

/// Resolve `path`'s family, or `None` when it is in no repository at all (a
/// plain directory is still registrable, and dedups by path alone).
fn family_of(path: &Path) -> Option<FamilyOf> {
    let paths = rift_git::discover(path).ok()?;
    let common_dir = std::fs::canonicalize(&paths.common_dir).ok()?;
    let members = rift_git::family_members(&paths);
    Some(FamilyOf {
        common_dir,
        main: members
            .iter()
            .find(|member| member.is_main)
            .map(|member| member.work_tree.clone()),
        members: members.into_iter().map(|member| member.work_tree).collect(),
    })
}

/// One entry per repository family, for the legacy import.
///
/// The family **root** wins where the file names it, whatever order it is
/// listed in; otherwise the first entry of the family keeps the seat, so the
/// import is a pure function of the file and re-running it cannot shuffle the
/// dashboard. An entry in no repository is never deduplicated — there is no
/// family to compare it against.
fn one_row_per_family(
    entries: &[(String, std::path::PathBuf)],
) -> Vec<&(String, std::path::PathBuf)> {
    let mut chosen: Vec<(Option<std::path::PathBuf>, &(String, std::path::PathBuf))> = Vec::new();
    for entry in entries {
        let Some(family) = family_of(&entry.1) else {
            chosen.push((None, entry));
            continue;
        };
        if let Some(slot) = chosen
            .iter_mut()
            .find(|(key, _)| key.as_ref() == Some(&family.common_dir))
        {
            if family.is_root(&comparable(&entry.1)) {
                slot.1 = entry;
            }
            continue;
        }
        chosen.push((Some(family.common_dir), entry));
    }
    chosen.into_iter().map(|(_, entry)| entry).collect()
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

    /// A refused registration names the folder as typed and the step that
    /// refused it — the alert's one line has to be actionable on its own.
    #[test]
    fn registering_a_missing_path_names_the_path_and_the_canonicalize_step() {
        let (dir, cs) = store();
        let missing = dir.path().join("nope").to_string_lossy().into_owned();
        let err = cs
            .apply(
                &PersistOp::AddProject {
                    path: missing.clone(),
                },
                1,
            )
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with(&format!("could not register {missing} — canonicalize: ")),
            "{err}"
        );
        assert!(
            err.len() > missing.len() + 40,
            "the OS's words follow: {err}"
        );
    }

    #[test]
    fn registering_a_file_names_the_check_step() {
        let (dir, cs) = store();
        let file = dir.path().join("notes.md");
        std::fs::write(&file, b"x").unwrap();
        let err = cs
            .apply(
                &PersistOp::AddProject {
                    path: file.to_string_lossy().into_owned(),
                },
                1,
            )
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("could not register "), "{err}");
        assert!(err.ends_with(" — check: not a directory"), "{err}");
        assert!(
            err.contains("notes.md"),
            "the canonical path names the file: {err}"
        );
    }

    /// The last step — the store write — names itself too, so a locked or
    /// read-only `ui-state.db` reads as exactly that.
    #[test]
    fn a_read_only_store_names_the_append_step() {
        let dir = tempfile::tempdir().unwrap();
        let project = make_dir(dir.path(), "alpha");
        drop(CanopyStore::open(dir.path()).unwrap());
        let read_only = CanopyStore::open_read_only(dir.path()).unwrap().unwrap();
        let err = read_only
            .apply(&PersistOp::AddProject { path: project }, 1)
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("could not register "), "{err}");
        assert!(
            err.contains(" — append to ui-state.db: storage error: sqlite read-only"),
            "{err}"
        );
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

    /// Add a linked worktree beside `repo`, through the one builder that writes
    /// the file layout `git worktree add` writes (family identity). Hand-building
    /// it here is how this fixture used to drift from the shape the product
    /// accepts — the admin dir had no `HEAD`, and neither did `<repo>/.git`.
    fn add_worktree_fixture(repo: &Path, name: &str) -> String {
        let wt = repo.parent().unwrap().join(name);
        rift_git::test_support::fake_worktree(&repo.join(".git"), &wt, name).unwrap();
        wt.to_string_lossy().into_owned()
    }

    /// A registry row written the way a build **before** the family view wrote one:
    /// straight into the log, past the family rule `add` now enforces.
    ///
    /// The door is closed; the rows are not. They are on every machine that ran
    /// the old build and the log is append-only, so the rendering rules below
    /// still have to hold for them — which is exactly what the two tests using
    /// this helper have always asserted. Seeding them any other way would be
    /// asserting a state the product can no longer be in.
    fn seed_legacy_row(cs: &CanopyStore, path: &str, label: &str, now_ms: i64) {
        let project_id = ProjectId::new(cs.ulids.next_ulid()).to_string();
        let repo = repo_id_for(Path::new(path)).unwrap();
        cs.append(
            &project_id,
            &EventRow::added(repo.as_str(), path, label, now_ms),
        )
        .unwrap();
    }

    #[test]
    fn linked_worktrees_list_as_child_rows_under_their_repo() {
        let (dir, cs) = store();
        let repo = make_dir(dir.path(), "repo");
        rift_git::test_support::fake_repo(std::path::Path::new(&repo)).unwrap();
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
    fn a_prunable_member_still_gets_a_row_to_be_missing_in() {
        // `git worktree remove`'s other half, and the row the family view asks for:
        // the checkout is deleted, the administrative entry survives, git
        // calls it prunable. Before this the rail simply stopped listing it,
        // so a half-removed member was indistinguishable from one that had
        // never existed and nothing on screen could tell the operator that a
        // `git worktree prune` was owed.
        let (dir, cs) = store();
        let repo = make_dir(dir.path(), "repo");
        rift_git::test_support::fake_repo(std::path::Path::new(&repo)).unwrap();
        add_worktree_fixture(std::path::Path::new(&repo), "alive");
        let gone = add_worktree_fixture(std::path::Path::new(&repo), "gone");
        std::fs::remove_dir_all(&gone).unwrap();
        cs.apply(&PersistOp::AddProject { path: repo.clone() }, 1)
            .unwrap();

        let projects = cs.projects().unwrap();
        let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["repo", "alive", "gone"],
            "the member whose checkout is gone keeps its row"
        );
        assert_eq!(projects[2].path, gone, "named by the path it had");
        assert_eq!(projects[2].kind, EntryKind::Worktree);
        // The status is the live bus's to derive (`Error(RepoMissing)`, whose
        // worktree-row hint says prunable); the registry view starts every row
        // `Idle` and says nothing it has not read.
        assert_eq!(projects[2].status, ProjectStatus::Idle);
    }

    #[test]
    fn a_child_is_recognised_as_its_own_row_whatever_its_gitdir_file_spells_it() {
        // The dedup compares canonically, and the drift it closes is on the
        // WORKTREE side: a registered path is canonicalised on the way in
        // (`add`), while a member's checkout is whatever `<admin>/gitdir`
        // names — which for a checkout reached through a symlink, or added
        // from a path with a `.` segment, is not the canonical spelling. A raw
        // string compare then gave that project BOTH its own row and a
        // duplicate child under its repository. Written without a symlink so
        // it runs on every platform CI builds.
        let (dir, cs) = store();
        let repo = make_dir(dir.path(), "repo");
        rift_git::test_support::fake_repo(std::path::Path::new(&repo)).unwrap();
        let wt = add_worktree_fixture(std::path::Path::new(&repo), "own-row");
        // Respell the administrative pointer the way git records it when the
        // worktree was added through a non-canonical path.
        let admin = std::path::Path::new(&repo).join(".git/worktrees/own-row");
        let parent = std::path::Path::new(&wt).parent().unwrap().to_path_buf();
        // A `..` hop, not a `.` one: `Path` equality already normalises `.`
        // away, so only a spelling that survives component comparison and
        // differs after `canonicalize` states the claim (a symlinked checkout
        // is the real-world case, and needs a platform this cannot assume).
        // Spelled from a non-verbatim base: under a `\\?\` prefix only `\`
        // separates, so a `/`-joined tail would be one component and the
        // hop would collapse the checkout to the temp root. Git itself never
        // writes the verbatim form into `gitdir`.
        let parent = rift_git::child_process_path(&parent);
        std::fs::write(
            admin.join("gitdir"),
            format!("{p}/own-row/../own-row/.git\n", p = parent.display()),
        )
        .unwrap();

        cs.apply(&PersistOp::AddProject { path: repo.clone() }, 1)
            .unwrap();
        // A row `AddProject` would now refuse (the family view), seeded as the old
        // build left it: the rule under test is how such a row RENDERS.
        seed_legacy_row(&cs, &wt, "own-row", 2);

        let projects = cs.projects().unwrap();
        assert_eq!(
            projects.len(),
            2,
            "the repo and the worktree's own row, no duplicate child: {:?}",
            projects.iter().map(|p| &p.path).collect::<Vec<_>>()
        );
        assert_eq!(projects[1].path, wt);
        assert_eq!(projects[1].kind, EntryKind::Repo, "it is a registry row");
    }

    #[test]
    fn repo_without_worktrees_renders_exactly_as_today() {
        let (dir, cs) = store();
        let repo = make_dir(dir.path(), "plain");
        rift_git::test_support::fake_repo(std::path::Path::new(&repo)).unwrap();
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
        rift_git::test_support::fake_repo(std::path::Path::new(&repo)).unwrap();
        let wt_path = add_worktree_fixture(std::path::Path::new(&repo), "wt-own");
        cs.apply(&PersistOp::AddProject { path: repo.clone() }, 1)
            .unwrap();
        // Written before the one-row-per-family rule, as above: the registration is
        // refused today, the row
        // it left behind is not.
        seed_legacy_row(&cs, &wt_path, "wt-own", 2);

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

    /// ★ the family view — a linked worktree of a registered repository is refused,
    /// as an **error** and never as an `Ok` no-op.
    ///
    /// The distinction is the point. `apply_persist` fires the dashboard's
    /// success toast on `Ok` and opens the alert only on `Err`, so returning
    /// `Ok` for a registration that did not happen would tell the user their
    /// worktree is registered while the rail shows one row — the exact
    /// disagreement the family rule removed from storage, recreated in the UI.
    ///
    /// The message is asserted through the typed refusal, so the sentence has
    /// one definition and the CLI and the alert cannot drift apart.
    #[test]
    fn registering_a_member_of_a_registered_family_is_refused() {
        let (dir, cs) = store();
        let repo = make_dir(dir.path(), "repo");
        rift_git::test_support::fake_repo(Path::new(&repo)).unwrap();
        let member = add_worktree_fixture(Path::new(&repo), "feature");

        cs.apply(&PersistOp::AddProject { path: repo.clone() }, 1)
            .unwrap();
        let refused = cs
            .apply(
                &PersistOp::AddProject {
                    path: member.clone(),
                },
                2,
            )
            .expect_err("a member of a registered family is not registrable");
        assert_eq!(
            refused.to_string(),
            RiftError::from(CanopyError::AlreadyRegisteredAs(repo.clone())).to_string(),
            "the refusal names where the family already lives"
        );

        let projects = cs.projects().unwrap();
        assert_eq!(projects.len(), 2, "the repo and its DERIVED child, no more");
        assert_eq!(projects[0].path, repo);
        assert_eq!(projects[0].kind, EntryKind::Repo);
        assert_eq!(projects[1].path, member);
        assert_eq!(
            projects[1].kind,
            EntryKind::Worktree,
            "the member is shown as what it is, not as a second repository"
        );
        assert_eq!(
            cs.events().unwrap().len(),
            1,
            "a refusal writes nothing at all"
        );
    }

    /// ★ the family view — the family root arriving over a member's row supersedes
    /// it: `removed` then `added`, two events, nothing deleted.
    ///
    /// A member CAN hold the family's seat — it is the only row when a user
    /// registers a worktree before its repository, and earlier builds wrote
    /// plenty of them. The root is the one path allowed to take that seat back,
    /// because it is the one the whole family agrees on; a second *member*
    /// arriving is refused by the test above.
    ///
    /// Append-only is asserted, not assumed: the member's own `added` event is
    /// still in the log after the supersession, with a `removed` beside it.
    #[test]
    fn registering_the_root_supersedes_a_member_row() {
        let (dir, cs) = store();
        let repo = make_dir(dir.path(), "repo");
        rift_git::test_support::fake_repo(Path::new(&repo)).unwrap();
        let member = add_worktree_fixture(Path::new(&repo), "early");

        cs.apply(
            &PersistOp::AddProject {
                path: member.clone(),
            },
            1,
        )
        .expect("a member is registrable while its family has no row");
        assert_eq!(cs.projects().unwrap().len(), 1);

        cs.apply(&PersistOp::AddProject { path: repo.clone() }, 2)
            .expect("the family root supersedes the member's row");

        let projects = cs.projects().unwrap();
        assert_eq!(
            projects.len(),
            2,
            "the root's row and the member as its child: {:?}",
            projects.iter().map(|p| &p.path).collect::<Vec<_>>()
        );
        assert_eq!(projects[0].path, repo);
        assert_eq!(projects[0].kind, EntryKind::Repo);
        assert_eq!(projects[1].path, member);
        assert_eq!(projects[1].kind, EntryKind::Worktree);

        let events = cs.events().unwrap();
        assert_eq!(events.len(), 3, "added, removed, added — no deletion");
        assert!(
            matches!(&events[0].op, ProjectOp::Added { path, .. } if path == &member),
            "the member's registration is still in the history: {:?}",
            events[0].op
        );
        assert!(
            matches!(events[1].op, ProjectOp::Removed),
            "superseded by a removed event: {:?}",
            events[1].op
        );
        assert_eq!(
            events[0].project_id, events[1].project_id,
            "and the removal supersedes that same project"
        );
    }

    /// The legacy TOML registry could hold a repository AND one of its
    /// worktrees — `add` deduped by raw path there until the family view.
    /// Importing
    /// both would recreate the duplicate on the first launch after the upgrade,
    /// so the import takes one row per family, and the ROOT wins whatever order
    /// the file lists them in.
    #[test]
    fn a_legacy_registry_of_a_repository_and_its_member_imports_one_row() {
        let (dir, cs) = store();
        let repo = make_dir(dir.path(), "repo");
        rift_git::test_support::fake_repo(Path::new(&repo)).unwrap();
        let member = add_worktree_fixture(Path::new(&repo), "legacy-member");
        let unrelated = make_dir(dir.path(), "unrelated");

        // The member FIRST, so keeping "the first entry" would keep the wrong
        // one and the assertion below would catch it.
        let entries = vec![
            (
                "legacy-member".to_owned(),
                std::path::PathBuf::from(&member),
            ),
            ("repo".to_owned(), std::path::PathBuf::from(&repo)),
            ("unrelated".to_owned(), std::path::PathBuf::from(&unrelated)),
        ];
        cs.import_legacy(&entries, 1).unwrap();

        let rows: Vec<String> = cs
            .registry()
            .unwrap()
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        assert_eq!(
            rows,
            vec![repo.clone(), unrelated],
            "one row for the family — the root — and the unrelated project untouched"
        );
        assert_eq!(
            cs.projects()
                .unwrap()
                .iter()
                .filter(|p| p.path == member)
                .map(|p| p.kind)
                .collect::<Vec<_>>(),
            vec![EntryKind::Worktree],
            "the member survives as the child row it always was"
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

    /// Seed and return the active (default) profile's id.
    fn seed(cs: &CanopyStore, now_ms: i64) -> String {
        cs.seed_default_profile(now_ms).unwrap();
        cs.profiles_view().unwrap().0.unwrap()
    }

    #[test]
    fn set_layout_appends_and_reads_back_the_latest_with_reset() {
        use rift_canopy::LayoutModel;
        let (dir, cs) = store();
        let active = seed(&cs, 0);
        let path = make_dir(dir.path(), "lay");
        cs.apply(&PersistOp::AddProject { path: path.clone() }, 1)
            .unwrap();
        // No layout event yet → the default (loader returns None).
        assert_eq!(cs.profile_layout(&active, &path).unwrap(), None);

        // Save a rearranged layout (swap the first two columns).
        let mut m = LayoutModel::default();
        m.columns.swap(0, 1);
        cs.apply(
            &PersistOp::SetLayout {
                profile: active.clone(),
                path: path.clone(),
                layout: Some(m.encode()),
            },
            2,
        )
        .unwrap();
        assert_eq!(cs.profile_layout(&active, &path).unwrap(), Some(m));

        // A reset is a NEW superseding event → the reader returns None (default).
        cs.apply(
            &PersistOp::SetLayout {
                profile: active.clone(),
                path: path.clone(),
                layout: None,
            },
            3,
        )
        .unwrap();
        assert_eq!(
            cs.profile_layout(&active, &path).unwrap(),
            None,
            "reset supersedes to default"
        );

        // Both events are kept (append-only).
        let pid = cs.project_id_for(&path).unwrap().unwrap();
        assert!(cs
            .latest_profile_layout_event_id(&active, &pid)
            .unwrap()
            .is_some());
    }

    #[test]
    fn layout_for_unknown_path_is_a_noop() {
        let (dir, cs) = store();
        let active = seed(&cs, 0);
        let ghost = dir.path().join("ghost").to_string_lossy().into_owned();
        cs.apply(
            &PersistOp::SetLayout {
                profile: active.clone(),
                path: ghost.clone(),
                layout: Some("v1;20=rail".into()),
            },
            1,
        )
        .unwrap();
        assert_eq!(cs.profile_layout(&active, &ghost).unwrap(), None);
    }

    // --- the profile plane ----------------------------------------

    #[test]
    fn seed_happens_once_and_folds_to_an_active_default() {
        let (_dir, cs) = store();
        cs.seed_default_profile(1).unwrap();
        let (active, live) = cs.profiles_view().unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].name, DEFAULT_PROFILE_NAME);
        assert_eq!(active.as_deref(), Some(live[0].profile_id.as_str()));

        // A second launch appends nothing.
        cs.seed_default_profile(2).unwrap();
        assert_eq!(cs.profile_events().unwrap().len(), 2, "created + selected");
    }

    #[test]
    fn rename_the_default_is_an_ordinary_superseding_event() {
        let (_dir, cs) = store();
        let active = seed(&cs, 1);
        cs.apply(
            &PersistOp::RenameProfile {
                profile: active.clone(),
                name: "work".into(),
            },
            2,
        )
        .unwrap();
        let (_, live) = cs.profiles_view().unwrap();
        assert_eq!(live[0].name, "work");
        assert_eq!(cs.profile_events().unwrap().len(), 3, "history kept");
    }

    #[test]
    fn rename_refuses_invalid_and_duplicate_names() {
        let (_dir, cs) = store();
        let active = seed(&cs, 1);
        cs.apply(&PersistOp::CreateProfile { name: "lab".into() }, 2)
            .unwrap();
        for bad in ["", "   ", "lab", &"x".repeat(25)] {
            assert!(
                cs.apply(
                    &PersistOp::RenameProfile {
                        profile: active.clone(),
                        name: (*bad).to_string(),
                    },
                    3,
                )
                .is_err(),
                "rename to {bad:?} must refuse"
            );
        }
        // Rename to its own current name passes (a no-op rename).
        cs.apply(
            &PersistOp::RenameProfile {
                profile: active,
                name: DEFAULT_PROFILE_NAME.into(),
            },
            4,
        )
        .unwrap();
    }

    #[test]
    fn create_copies_the_creating_profiles_effective_list() {
        let (dir, cs) = store();
        seed(&cs, 0);
        let a = make_dir(dir.path(), "a");
        let b = make_dir(dir.path(), "b");
        cs.apply(&PersistOp::AddProject { path: a.clone() }, 1)
            .unwrap();
        cs.apply(&PersistOp::AddProject { path: b.clone() }, 2)
            .unwrap();

        let lab = cs.create_profile("lab", 3).unwrap();
        let view = cs.profile_projects(&lab).unwrap();
        assert_eq!(
            view.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
            vec![a.as_str(), b.as_str()],
            "the new profile starts from the creating profile's list, in order"
        );
        // The copy is explicit events, frozen at creation: a later catalogue
        // registration does NOT appear in the copied view.
        let c = make_dir(dir.path(), "c");
        cs.apply(&PersistOp::AddProject { path: c.clone() }, 4)
            .unwrap();
        let view = cs.profile_projects(&lab).unwrap();
        assert!(
            !view.iter().any(|p| p.path == c),
            "membership diverges after creation"
        );
    }

    #[test]
    fn duplicate_clones_membership_layouts_and_theme_in_full() {
        use rift_canopy::LayoutModel;
        let (dir, cs) = store();
        seed(&cs, 0);
        let a = make_dir(dir.path(), "a");
        let b = make_dir(dir.path(), "b");
        cs.apply(&PersistOp::AddProject { path: a.clone() }, 1)
            .unwrap();
        cs.apply(&PersistOp::AddProject { path: b.clone() }, 2)
            .unwrap();
        let lab = cs.create_profile("lab", 3).unwrap();
        cs.apply(
            &PersistOp::SelectProfile {
                profile: lab.clone(),
            },
            4,
        )
        .unwrap();
        // The source gets an arrangement on `a` and two theme slots.
        let mut m = LayoutModel::default();
        m.columns.swap(0, 1);
        cs.apply(
            &PersistOp::SetLayout {
                profile: lab.clone(),
                path: a.clone(),
                layout: Some(m.encode()),
            },
            5,
        )
        .unwrap();
        cs.set_profile_theme(&lab, "background", Some("#112233"), 6)
            .unwrap();
        cs.set_profile_theme(&lab, "text", Some("#aabbcc"), 7)
            .unwrap();

        let copy = cs.duplicate_profile(&lab, "lab-copy", 8).unwrap();
        // Membership: the source's effective list, in order.
        let view = cs.profile_projects(&copy).unwrap();
        assert_eq!(
            view.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
            vec![a.as_str(), b.as_str()],
            "the clone carries the source's membership in order"
        );
        // Arrangements: effective equality per member.
        assert_eq!(
            cs.profile_layout(&copy, &a).unwrap(),
            Some(m),
            "the clone carries the source's arrangement"
        );
        assert_eq!(cs.profile_layout(&copy, &b).unwrap(), None);
        // Theme: fold-equal to the source; unset slots stay unset.
        assert_eq!(
            cs.profile_theme(&copy).unwrap(),
            cs.profile_theme(&lab).unwrap()
        );
        assert_eq!(cs.profile_theme(&copy).unwrap().border, None);
        // A duplicate never writes `selected`: the active profile is unmoved.
        let (active, _) = cs.profiles_view().unwrap();
        assert_eq!(
            active.as_deref(),
            Some(lab.as_str()),
            "cloning is not switching"
        );
    }

    #[test]
    fn duplicate_of_the_default_freezes_the_catalogue_and_legacy_layout_fallback() {
        use rift_canopy::LayoutModel;
        let (dir, cs) = store();
        let active = seed(&cs, 0);
        let a = make_dir(dir.path(), "a");
        cs.apply(&PersistOp::AddProject { path: a.clone() }, 1)
            .unwrap();
        let pid = cs.project_id_for(&a).unwrap().unwrap();
        // A pre-upgrade arrangement sits in the frozen legacy table —
        // the default profile's effective layout comes from the fallback fold.
        let mut m = LayoutModel::default();
        m.columns.swap(0, 1);
        let cols = vec![
            "event_id",
            "project_id",
            "layout",
            "supersedes",
            "created_at",
        ];
        let values = [
            Value::text("legacy-1"),
            Value::text(&pid),
            Value::text(m.encode()),
            Value::Null,
            Value::Int(0),
        ];
        runtime()
            .unwrap()
            .block_on(cs.store.insert("canopy_layout_event", &cols, &values))
            .unwrap();

        let copy = cs.duplicate_profile(&active, "frozen", 2).unwrap();
        // The clone materialised the default's implicit catalogue view…
        let view = cs.profile_projects(&copy).unwrap();
        assert_eq!(
            view.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
            vec![a.as_str()]
        );
        // …captured the legacy fold as an explicit profile-scoped row…
        assert_eq!(
            cs.profile_layout(&copy, &a).unwrap(),
            Some(m),
            "the clone owns the default's effective (legacy-fold) arrangement"
        );
        // …and is frozen at clone time: a later registration joins the default
        // (catalogue view) but never the clone.
        let b = make_dir(dir.path(), "b");
        cs.apply(&PersistOp::AddProject { path: b.clone() }, 3)
            .unwrap();
        assert!(cs
            .profile_projects(&active)
            .unwrap()
            .iter()
            .any(|p| p.path == b));
        assert!(!cs
            .profile_projects(&copy)
            .unwrap()
            .iter()
            .any(|p| p.path == b));
    }

    #[test]
    fn duplicate_refuses_invalid_duplicate_names_and_dead_sources() {
        let (_dir, cs) = store();
        let active = seed(&cs, 0);
        // Name rules hold: empty, over-long, and colliding names refuse.
        assert!(cs.duplicate_profile(&active, "   ", 1).is_err());
        assert!(cs.duplicate_profile(&active, &"x".repeat(25), 2).is_err());
        assert!(cs
            .duplicate_profile(&active, DEFAULT_PROFILE_NAME, 3)
            .is_err());
        // Unknown and removed sources refuse.
        assert!(cs.duplicate_profile("ghost", "copy", 4).is_err());
        let lab = cs.create_profile("lab", 5).unwrap();
        cs.apply(
            &PersistOp::RemoveProfile {
                profile: lab.clone(),
            },
            6,
        )
        .unwrap();
        assert!(cs.duplicate_profile(&lab, "copy", 7).is_err());
    }

    #[test]
    fn duplicate_and_source_diverge_freely_after_the_clone() {
        let (dir, cs) = store();
        seed(&cs, 0);
        let a = make_dir(dir.path(), "a");
        cs.apply(&PersistOp::AddProject { path: a.clone() }, 1)
            .unwrap();
        let lab = cs.create_profile("lab", 2).unwrap();
        let copy = cs.duplicate_profile(&lab, "lab-copy", 3).unwrap();
        // Source edits after the clone touch the source alone — and vice versa.
        cs.apply(
            &PersistOp::RemoveProfileProject {
                profile: lab.clone(),
                path: a.clone(),
            },
            4,
        )
        .unwrap();
        assert!(!cs
            .profile_projects(&lab)
            .unwrap()
            .iter()
            .any(|p| p.path == a));
        assert!(cs
            .profile_projects(&copy)
            .unwrap()
            .iter()
            .any(|p| p.path == a));
        cs.set_profile_theme(&copy, "graph", Some("#00ff00"), 5)
            .unwrap();
        assert_eq!(cs.profile_theme(&lab).unwrap().graph, None);
        assert!(cs.profile_theme(&copy).unwrap().graph.is_some());
    }

    #[test]
    fn default_membership_edit_from_the_editor_materializes_first() {
        // The members editor end-to-end: its first membership op on the DEFAULT
        // profile materialises its implicit catalogue view as explicit rows
        // (copy-on-first-divergence, the profile plane), THEN applies the edit.
        let (dir, cs) = store();
        let active = seed(&cs, 0);
        let a = make_dir(dir.path(), "a");
        let b = make_dir(dir.path(), "b");
        cs.apply(&PersistOp::AddProject { path: a.clone() }, 1)
            .unwrap();
        cs.apply(&PersistOp::AddProject { path: b.clone() }, 2)
            .unwrap();
        cs.apply(
            &PersistOp::RemoveProfileProject {
                profile: active.clone(),
                path: a.clone(),
            },
            3,
        )
        .unwrap();
        // The default's view lost `a` but kept `b` (the copy materialised)…
        let view = cs.profile_projects(&active).unwrap();
        assert_eq!(
            view.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
            vec![b.as_str()]
        );
        // …and once diverged, a registration under ANOTHER active profile no
        // longer reaches the default implicitly (its catalogue fallback ended).
        let lab = cs.create_profile("lab", 4).unwrap();
        cs.apply(&PersistOp::SelectProfile { profile: lab }, 5)
            .unwrap();
        let c = make_dir(dir.path(), "c");
        cs.apply(&PersistOp::AddProject { path: c.clone() }, 6)
            .unwrap();
        let view = cs.profile_projects(&active).unwrap();
        assert!(
            !view.iter().any(|p| p.path == c),
            "the first edit froze the default's implicit catalogue view"
        );
    }

    #[test]
    fn startup_pin_supersedes_unpins_and_refuses_dead_profiles() {
        let (_dir, cs) = store();
        let active = seed(&cs, 0);
        assert_eq!(cs.startup_profile().unwrap(), None, "unpinned by default");
        let lab = cs.create_profile("lab", 1).unwrap();
        // Pin, re-pin, unpin — one latest-wins fold, all superseding inserts.
        cs.set_startup_profile(Some(&active), 2).unwrap();
        assert_eq!(cs.startup_profile().unwrap().as_deref(), Some(&*active));
        cs.set_startup_profile(Some(&lab), 3).unwrap();
        assert_eq!(cs.startup_profile().unwrap().as_deref(), Some(&*lab));
        cs.set_startup_profile(None, 4).unwrap();
        assert_eq!(cs.startup_profile().unwrap(), None);
        // A dead or unknown pin refuses at the writer; unpin always lands.
        assert!(cs.set_startup_profile(Some("ghost"), 5).is_err());
        cs.apply(
            &PersistOp::RemoveProfile {
                profile: lab.clone(),
            },
            6,
        )
        .unwrap();
        assert!(cs.set_startup_profile(Some(&lab), 7).is_err());
    }

    #[test]
    fn launch_projects_resolves_a_live_pin_and_a_dead_pin_falls_back() {
        let (dir, cs) = store();
        seed(&cs, 0);
        let a = make_dir(dir.path(), "a");
        let b = make_dir(dir.path(), "b");
        cs.apply(&PersistOp::AddProject { path: a.clone() }, 1)
            .unwrap();
        let lab = cs.create_profile("lab", 2).unwrap();
        // Diverge lab: it sees only `b`.
        cs.apply(
            &PersistOp::RemoveProfileProject {
                profile: lab.clone(),
                path: a.clone(),
            },
            3,
        )
        .unwrap();
        cs.apply(
            &PersistOp::SelectProfile {
                profile: lab.clone(),
            },
            4,
        )
        .unwrap();
        cs.apply(&PersistOp::AddProject { path: b.clone() }, 5)
            .unwrap();
        // Unpinned: launch follows last-selected (lab → [b]).
        let view = cs.launch_projects().unwrap();
        assert_eq!(
            view.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
            vec![b.as_str()]
        );
        // Pinned to the default: launch opens the default's catalogue view
        // even though lab is last-selected — and pinning wrote the ONLY new
        // event (launch_projects itself reads).
        let (_, live) = cs.profiles_view().unwrap();
        let default = live
            .iter()
            .find(|p| p.name == DEFAULT_PROFILE_NAME)
            .unwrap()
            .profile_id
            .clone();
        cs.set_startup_profile(Some(&default), 6).unwrap();
        let view = cs.launch_projects().unwrap();
        assert!(view.iter().any(|p| p.path == a));
        assert!(view.iter().any(|p| p.path == b));
        // The fold's ACTIVE is untouched by pin resolution — no write occurred.
        let (active_after, _) = cs.profiles_view().unwrap();
        assert_eq!(
            active_after.as_deref(),
            Some(&*lab),
            "launch resolution never writes a selected event"
        );
        // A pin that dies falls back through the chain (lab's view again).
        cs.apply(
            &PersistOp::SelectProfile {
                profile: default.clone(),
            },
            7,
        )
        .unwrap();
        cs.set_startup_profile(Some(&lab), 8).unwrap();
        cs.apply(&PersistOp::RemoveProfile { profile: lab }, 9)
            .unwrap();
        let view = cs.launch_projects().unwrap();
        assert!(
            view.iter().any(|p| p.path == a),
            "a dead pin loses to the last-selected chain (the default's view)"
        );
    }

    #[test]
    fn create_refuses_invalid_and_duplicate_names() {
        let (_dir, cs) = store();
        seed(&cs, 0);
        assert!(cs.create_profile("", 1).is_err());
        assert!(cs.create_profile("   ", 1).is_err());
        assert!(cs.create_profile(DEFAULT_PROFILE_NAME, 1).is_err());
        assert!(cs.create_profile(&"x".repeat(25), 1).is_err());
        // Trimming applies before the checks.
        cs.create_profile("  lab  ", 2).unwrap();
        let (_, live) = cs.profiles_view().unwrap();
        assert!(live.iter().any(|p| p.name == "lab"));
    }

    #[test]
    fn remove_refuses_the_active_and_the_last_profile() {
        let (_dir, cs) = store();
        let active = seed(&cs, 1);
        // The last profile cannot be removed (it is also the active one).
        assert!(cs
            .apply(
                &PersistOp::RemoveProfile {
                    profile: active.clone(),
                },
                2,
            )
            .is_err());
        let lab = cs.create_profile("lab", 3).unwrap();
        // The active profile cannot be removed even with another live.
        assert!(cs
            .apply(
                &PersistOp::RemoveProfile {
                    profile: active.clone(),
                },
                4,
            )
            .is_err());
        // A non-active, non-last profile removes as one superseding event.
        cs.apply(&PersistOp::RemoveProfile { profile: lab }, 5)
            .unwrap();
        let (_, live) = cs.profiles_view().unwrap();
        assert_eq!(live.len(), 1);
        // And now the survivor is the last profile again.
        assert!(cs
            .apply(&PersistOp::RemoveProfile { profile: active }, 6)
            .is_err());
    }

    #[test]
    fn select_switches_the_view_and_survives_a_relaunch() {
        let dir = tempfile::tempdir().unwrap();
        let cs = CanopyStore::open(dir.path()).unwrap();
        let _active = seed(&cs, 0);
        let a = make_dir(dir.path(), "a");
        let b = make_dir(dir.path(), "b");
        cs.apply(&PersistOp::AddProject { path: a.clone() }, 1)
            .unwrap();
        let lab = cs.create_profile("lab", 2).unwrap();
        cs.apply(
            &PersistOp::SelectProfile {
                profile: lab.clone(),
            },
            3,
        )
        .unwrap();
        // The lab view diverges: remove `a`, add `b`.
        cs.apply(
            &PersistOp::RemoveProfileProject {
                profile: lab.clone(),
                path: a.clone(),
            },
            4,
        )
        .unwrap();
        cs.apply(&PersistOp::AddProject { path: b.clone() }, 5)
            .unwrap();
        let view = cs.active_projects().unwrap();
        assert_eq!(
            view.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
            vec![b.as_str()]
        );

        // A simulated relaunch (a fresh handle over the same db) restores the
        // active profile and its membership.
        drop(cs);
        let cs = CanopyStore::open(dir.path()).unwrap();
        cs.seed_default_profile(6).unwrap(); // no-op: already seeded
        let (active, _) = cs.profiles_view().unwrap();
        assert_eq!(active.as_deref(), Some(lab.as_str()));
        let view = cs.active_projects().unwrap();
        assert_eq!(
            view.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
            vec![b.as_str()]
        );
        // The default profile still shows its own (full-catalogue) view.
        let (_, live) = cs.profiles_view().unwrap();
        let default = live
            .iter()
            .find(|p| p.name == DEFAULT_PROFILE_NAME)
            .unwrap();
        let dview = cs.profile_projects(&default.profile_id).unwrap();
        assert!(dview.iter().any(|p| p.path == a));
        assert!(dview.iter().any(|p| p.path == b));
    }

    #[test]
    fn select_refuses_an_unknown_profile() {
        let (_dir, cs) = store();
        seed(&cs, 0);
        assert!(cs
            .apply(
                &PersistOp::SelectProfile {
                    profile: "ghost".into(),
                },
                1,
            )
            .is_err());
    }

    #[test]
    fn default_shows_the_catalogue_until_its_first_membership_edit() {
        let (dir, cs) = store();
        let active = seed(&cs, 0);
        let a = make_dir(dir.path(), "a");
        let b = make_dir(dir.path(), "b");
        // Catalogue registrations (the CLI path) appear with no membership rows.
        cs.import_legacy(
            &[
                ("a".to_string(), std::path::PathBuf::from(&a)),
                ("b".to_string(), std::path::PathBuf::from(&b)),
            ],
            1,
        )
        .unwrap();
        assert_eq!(cs.active_projects().unwrap().len(), 2);
        assert!(cs.membership_events(&active).unwrap().is_empty());

        // The first membership edit materialises the view as explicit events…
        cs.apply(
            &PersistOp::RemoveProfileProject {
                profile: active.clone(),
                path: a.clone(),
            },
            2,
        )
        .unwrap();
        assert!(!cs.membership_events(&active).unwrap().is_empty());
        let view = cs.active_projects().unwrap();
        assert_eq!(
            view.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
            vec![b.as_str()]
        );

        // …after which a catalogue-only registration (the CLI path) no longer
        // joins the materialised view.
        let c = make_dir(dir.path(), "c");
        cs.add(&c, 3).unwrap();
        assert!(cs.projects().unwrap().iter().any(|p| p.path == c));
        assert!(!cs.active_projects().unwrap().iter().any(|p| p.path == c));
    }

    #[test]
    fn profile_reorder_swaps_within_the_view_only() {
        let (dir, cs) = store();
        let active = seed(&cs, 0);
        let a = make_dir(dir.path(), "a");
        let b = make_dir(dir.path(), "b");
        cs.apply(&PersistOp::AddProject { path: a.clone() }, 1)
            .unwrap();
        cs.apply(&PersistOp::AddProject { path: b.clone() }, 2)
            .unwrap();
        let after = cs
            .apply(
                &PersistOp::ReorderProfileProject {
                    profile: active.clone(),
                    path: a.clone(),
                    up: false,
                },
                3,
            )
            .unwrap();
        assert_eq!(after[0].path, b);
        assert_eq!(after[1].path, a);
        // The global catalogue order is untouched (the view diverged instead).
        let global = cs.projects().unwrap();
        assert_eq!(global[0].path, a);
        assert_eq!(global[1].path, b);
        // Off-the-end within the view is a no-op.
        let n = cs.membership_events(&active).unwrap().len();
        cs.apply(
            &PersistOp::ReorderProfileProject {
                profile: active.clone(),
                path: b,
                up: true,
            },
            4,
        )
        .unwrap();
        assert_eq!(cs.membership_events(&active).unwrap().len(), n);
    }

    #[test]
    fn default_falls_back_to_the_legacy_layout_and_other_profiles_do_not() {
        use rift_canopy::LayoutModel;
        let (dir, cs) = store();
        let active = seed(&cs, 0);
        let path = make_dir(dir.path(), "lay");
        cs.apply(&PersistOp::AddProject { path: path.clone() }, 1)
            .unwrap();
        let pid = cs.project_id_for(&path).unwrap().unwrap();

        // A pre-upgrade arrangement sits in the frozen legacy table.
        let mut m = LayoutModel::default();
        m.columns.swap(0, 1);
        let cols = vec![
            "event_id",
            "project_id",
            "layout",
            "supersedes",
            "created_at",
        ];
        let values = [
            Value::text("legacy-1"),
            Value::text(&pid),
            Value::text(m.encode()),
            Value::Null,
            Value::Int(0),
        ];
        runtime()
            .unwrap()
            .block_on(cs.store.insert("canopy_layout_event", &cols, &values))
            .unwrap();

        // The default profile reads it where it has no profile-scoped row…
        assert_eq!(cs.profile_layout(&active, &path).unwrap(), Some(m.clone()));
        // …but a new profile starts canonical (no fallback).
        let lab = cs.create_profile("lab", 2).unwrap();
        assert_eq!(cs.profile_layout(&lab, &path).unwrap(), None);

        // A profile-scoped reset supersedes the fallback for the default too.
        cs.apply(
            &PersistOp::SetLayout {
                profile: active.clone(),
                path: path.clone(),
                layout: None,
            },
            3,
        )
        .unwrap();
        assert_eq!(cs.profile_layout(&active, &path).unwrap(), None);
    }

    #[test]
    fn theme_events_supersede_per_slot_and_fold_latest_wins() {
        use rift_canopy::{ThemeSlot, PRESETS};
        let (_dir, cs) = store();
        let active = seed(&cs, 0);
        // No rows → no overrides (Rift dark, byte-identical).
        assert_eq!(
            cs.profile_theme(&active).unwrap(),
            rift_canopy::ThemeOverrides::NONE
        );

        // Set two slots; both rows are kept (append-only) and both fold.
        let red = rift_canopy::colour_to_hex(PRESETS[0].1).unwrap();
        let blue = rift_canopy::colour_to_hex(PRESETS[1].1).unwrap();
        cs.apply(
            &PersistOp::SetProfileTheme {
                profile: active.clone(),
                slot: "background".into(),
                colour: Some(red.clone()),
            },
            1,
        )
        .unwrap();
        cs.apply(
            &PersistOp::SetProfileTheme {
                profile: active.clone(),
                slot: "text".into(),
                colour: Some(blue.clone()),
            },
            2,
        )
        .unwrap();
        let o = cs.profile_theme(&active).unwrap();
        assert_eq!(o.get(ThemeSlot::Background), Some(PRESETS[0].1));
        assert_eq!(o.get(ThemeSlot::Text), Some(PRESETS[1].1));

        // A later event supersedes per (profile, slot); the other slot holds.
        cs.apply(
            &PersistOp::SetProfileTheme {
                profile: active.clone(),
                slot: "background".into(),
                colour: Some(blue.clone()),
            },
            3,
        )
        .unwrap();
        let o = cs.profile_theme(&active).unwrap();
        assert_eq!(o.get(ThemeSlot::Background), Some(PRESETS[1].1));
        assert_eq!(o.get(ThemeSlot::Text), Some(PRESETS[1].1));

        // NULL resets to default — a superseding event, never a delete.
        cs.apply(
            &PersistOp::SetProfileTheme {
                profile: active.clone(),
                slot: "background".into(),
                colour: None,
            },
            4,
        )
        .unwrap();
        let o = cs.profile_theme(&active).unwrap();
        assert_eq!(o.get(ThemeSlot::Background), None, "reset → default");
        assert_eq!(o.get(ThemeSlot::Text), Some(PRESETS[1].1));

        // Themes are per profile: a fresh profile starts at Rift dark.
        let lab = cs.create_profile("lab", 5).unwrap();
        assert_eq!(
            cs.profile_theme(&lab).unwrap(),
            rift_canopy::ThemeOverrides::NONE
        );
    }

    #[test]
    fn theme_writer_refuses_unknown_slots_and_malformed_hex() {
        let (_dir, cs) = store();
        let active = seed(&cs, 0);
        assert!(cs.set_profile_theme(&active, "sparkle", None, 1).is_err());
        assert!(cs
            .set_profile_theme(&active, "background", Some("red"), 1)
            .is_err());
        assert!(cs
            .set_profile_theme(&active, "background", Some("#12345"), 1)
            .is_err());
        assert!(cs
            .set_profile_theme(&active, "background", Some("#a1b2c3"), 1)
            .is_ok());
    }

    #[test]
    fn add_project_joins_an_explicit_view_but_not_the_catalogue_fallback() {
        let (dir, cs) = store();
        let active = seed(&cs, 0);
        let a = make_dir(dir.path(), "a");
        // Pre-materialisation: an add writes NO membership rows for the default
        // (the catalogue fallback already shows it — story 19 keeps holding).
        cs.apply(&PersistOp::AddProject { path: a.clone() }, 1)
            .unwrap();
        assert!(cs.membership_events(&active).unwrap().is_empty());

        // In an explicit view (a created profile), an add joins that view.
        let lab = cs.create_profile("lab", 2).unwrap();
        cs.apply(
            &PersistOp::SelectProfile {
                profile: lab.clone(),
            },
            3,
        )
        .unwrap();
        let b = make_dir(dir.path(), "b");
        cs.apply(&PersistOp::AddProject { path: b.clone() }, 4)
            .unwrap();
        assert!(cs
            .profile_projects(&lab)
            .unwrap()
            .iter()
            .any(|p| p.path == b));
        // …and only that view: the default profile keeps its own fold.
        assert!(
            cs.profile_projects(&active)
                .unwrap()
                .iter()
                .any(|p| p.path == b),
            "default still tracks the catalogue"
        );
    }
}
