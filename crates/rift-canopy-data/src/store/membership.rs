//! Profile membership: `canopy_profile_project_event`, which catalogue
//! projects one profile's view shows and in what order.
//!
//! Its own module because the default profile's copy-on-first-divergence —
//! the catalogue fallback until its first membership edit — is a rule about
//! this relation alone, and every path that includes a project goes through
//! one writer here.

use rift_canopy::{
    current_membership, default_profile_id, ProfileEvent, ProfileProjectEvent, ProfileProjectOp,
    ProjectEntry,
};
use rift_core::{Result, RiftError, UlidSource};
use rift_store::{CanonicalStore, Value};

use super::{neighbor, opt_text, runtime, CanopyStore, PROFILE_PROJECT_COLS};

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

impl CanopyStore {
    /// Include a catalogue project in a profile's view (this profile only). A
    /// no-op for an unknown path or a project already in the view; the default
    /// profile materialises its catalogue view first.
    pub(super) fn add_profile_project(&self, profile: &str, path: &str, now_ms: i64) -> Result<()> {
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
    pub(super) fn remove_profile_project(
        &self,
        profile: &str,
        path: &str,
        now_ms: i64,
    ) -> Result<()> {
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
    pub(super) fn reorder_profile_project(
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
    pub(super) fn effective_entries(
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

    /// Append one membership `added` event (the single include writer, so the
    /// copy paths and the edit paths cannot diverge).
    pub(super) fn include_in_profile(
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

    /// Read one profile's membership event rows into the engine-neutral
    /// [`ProfileProjectEvent`].
    pub(super) fn membership_events(&self, profile: &str) -> Result<Vec<ProfileProjectEvent>> {
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

/// The event_id of the latest membership event for `project_id` within one
/// profile's log (events arrive sorted), or `None`.
fn latest_membership_event_id(events: &[ProfileProjectEvent], project_id: &str) -> Option<String> {
    events
        .iter()
        .rev()
        .find(|e| e.project_id == project_id)
        .map(|e| e.event_id.clone())
}

#[cfg(test)]
mod tests {
    use rift_canopy::PersistOp;

    use crate::store::fixtures::{make_dir, seed, store};

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
