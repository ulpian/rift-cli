//! The profile log: `canopy_profile_event`, the mint / rename / remove /
//! select history every other profile-scoped relation keys by.
//!
//! Its own module because the profile guards (a valid unique name, never the
//! active or the last profile) are decided here and nowhere else, and a
//! duplicate is the one write that reaches into every other relation.

use rift_canopy::{
    active_profile_id, current_profiles, normalize_profile_name, ProfileEntry, ProfileEvent,
    ProfileOp, Project, DEFAULT_PROFILE_NAME,
};
use rift_core::{Result, RiftError, UlidSource};
use rift_store::{CanonicalStore, Value};

use super::views::view_with_worktrees;
use super::{opt_text, opt_text_ref, runtime, CanopyStore, PROFILE_COLS};

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

impl CanopyStore {
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

    /// Read every profile event row into the engine-neutral [`ProfileEvent`],
    /// ordered by transaction time then event id (folds re-sort defensively).
    pub(super) fn profile_events(&self) -> Result<Vec<ProfileEvent>> {
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

/// The event_id of the latest profile event recorded for `profile_id` (events
/// arrive sorted), or `None` — the `supersedes` back-pointer.
fn latest_profile_event_id(events: &[ProfileEvent], profile_id: &str) -> Option<String> {
    events
        .iter()
        .rev()
        .find(|e| e.profile_id == profile_id)
        .map(|e| e.event_id.clone())
}

#[cfg(test)]
mod tests {
    use rift_canopy::PersistOp;

    use super::*;
    use crate::store::fixtures::{make_dir, seed, store};

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
}
