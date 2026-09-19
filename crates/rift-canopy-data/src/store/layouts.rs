//! Arrangements: `canopy_profile_layout_event`, one superseding row per
//! `(profile, project)`, over the frozen legacy per-project
//! `canopy_layout_event` the default profile still falls back to.
//!
//! Its own module because the fallback order — profile row, then the legacy
//! fold, then the canonical default — is the whole rule, and it reads two
//! relations nothing else in the store touches.

use rift_canopy::default_profile_id;
use rift_core::{Result, UlidSource};
use rift_store::{CanonicalStore, Value};

use super::{opt_text, opt_text_ref, runtime, CanopyStore, PROFILE_LAYOUT_COLS};

impl CanopyStore {
    /// The saved **legacy** per-project layout: the latest event's
    /// arrangement, or `None` when the project is unknown, has no layout event, or
    /// its latest event is a reset — the caller then uses `LayoutModel::default`.
    /// A malformed/forward-version blob decodes to `None` (degrade to default).
    /// The table is **frozen for writes** since the profile plane (all new arrangement
    /// writes are profile-scoped); this reader is the default profile's fallback.
    ///
    /// # Errors
    /// [`rift_core::RiftError::Storage`] if the layout log cannot be read.
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

    /// The saved arrangement for `(profile, project)`: the latest
    /// profile-scoped event wins (`None` for a reset or a malformed blob); with
    /// **no** profile-scoped row, the default profile falls back to the legacy
    /// per-project fold ([`Self::layout`]) so every legacy arrangement survives —
    /// other profiles start canonical instead.
    ///
    /// # Errors
    /// [`rift_core::RiftError::Storage`] if a log cannot be read.
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

    /// Append a per-(profile, project) layout event: `layout` is the
    /// encoded arrangement, `None` a reset — a superseding event either way.
    /// All new arrangement writes land here; the legacy per-project table is
    /// frozen for writes. A no-op for an unknown path.
    pub(super) fn set_profile_layout(
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
}

#[cfg(test)]
mod tests {
    use rift_canopy::PersistOp;

    use super::*;
    use crate::store::fixtures::{make_dir, seed, store};

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
}
