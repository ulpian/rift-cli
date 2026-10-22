//! The global catalogue: `canopy_project_event`, the registration log every
//! profile view is a selection from.
//!
//! Its own module because registration carries the one-row-per-family rule
//! and the legacy import, and nothing else in the store needs to know how a
//! path becomes a row.

use rift_canopy::{
    active_profile_id, current_registry, default_profile_id, ProjectEntry, ProjectEvent, ProjectOp,
};
use rift_core::{ProjectId, Result, RiftError, UlidSource};
use rift_git::repo_id_for;
use rift_store::{CanonicalStore, Value};

use super::views::{comparable, family_of, one_row_per_family};
use super::{neighbor, opt_text, opt_text_ref, runtime, CanopyError, CanopyStore, EVENT_COLS};

/// A typed `canopy_project_event` row — the compile-checked shape of every
/// project-event insert. Constructed only through the per-op constructors, so a
/// column can never be filled positionally with the wrong value; the single
/// mapping onto [`EVENT_COLS`] lives in [`CanopyStore::append`].
pub(super) struct EventRow<'a> {
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
    pub(super) fn added(repo_id: &'a str, path: &'a str, label: &'a str, created_at: i64) -> Self {
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

impl CanopyStore {
    /// Every registered path, including hidden projects, for owned-integration
    /// cleanup. This reads registration history without restoring hidden rows.
    ///
    /// # Errors
    /// Returns the store error if registration history cannot be read.
    pub fn installed_project_paths(&self) -> Result<Vec<String>> {
        let mut paths = self
            .events()?
            .into_iter()
            .filter_map(|event| match event.op {
                rift_canopy::ProjectOp::Added { path, .. } => Some(path),
                _ => None,
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        Ok(paths)
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

    /// Register a project in the global catalogue (idempotent per canonical
    /// path) **and** include it in the active profile's view where that view is
    /// explicit (the profile plane: the unmaterialised default profile already shows
    /// the whole catalogue, so no membership event is written there).
    pub(super) fn add_to_view(&self, path: &str, now_ms: i64) -> Result<()> {
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

    pub(super) fn remove(&self, path: &str, now_ms: i64) -> Result<()> {
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

    pub(super) fn relabel(&self, path: &str, label: &str, now_ms: i64) -> Result<()> {
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

    pub(super) fn reorder(&self, path: &str, up: bool, now_ms: i64) -> Result<()> {
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

    /// The current registry entries (the fold over the event log).
    pub(super) fn registry(&self) -> Result<Vec<ProjectEntry>> {
        Ok(current_registry(&self.events()?))
    }

    /// Resolve a canonical path to its stable `project_id` (via the registry fold),
    /// or `None` if no present project has that path.
    pub(super) fn project_id_for(&self, path: &str) -> Result<Option<String>> {
        Ok(self
            .registry()?
            .into_iter()
            .find(|e| e.path == path)
            .map(|e| e.project_id))
    }

    /// Append one typed event row for `project_id` — the **only** writer of
    /// `canopy_project_event`, so the column order is encoded exactly once,
    /// against [`EVENT_COLS`].
    pub(super) fn append(&self, project_id: &str, row: &EventRow<'_>) -> Result<()> {
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

    /// Register a project in the global catalogue and return its stable id —
    /// the existing id when the canonical path is already present (idempotent),
    /// a fresh one after the first `added` event otherwise.
    pub(super) fn add(&self, path: &str, now_ms: i64) -> Result<String> {
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use rift_canopy::{EntryKind, PersistOp};

    use super::*;
    use crate::store::fixtures::{add_worktree_fixture, make_dir, store};

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
}
