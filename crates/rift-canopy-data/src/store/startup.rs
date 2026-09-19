//! The startup pin: `canopy_profile_startup_event`, the latest-wins record of
//! which profile the dashboard opens on, and the launch view it resolves to.
//!
//! Its own module because a pin is written without liveness and resolved with
//! it at launch, so the writer, the fold and the one launch read belong
//! together and apart from the profile log they consult.

use rift_canopy::{current_profiles, Project};
use rift_core::{Result, RiftError, UlidSource};
use rift_store::{CanonicalStore, Value};

use super::views::view_with_worktrees;
use super::{opt_text, opt_text_ref, runtime, CanopyStore, STARTUP_COLS};

impl CanopyStore {
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
}

#[cfg(test)]
mod tests {
    use rift_canopy::{PersistOp, DEFAULT_PROFILE_NAME};

    use crate::store::fixtures::{make_dir, seed, store};

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
}
