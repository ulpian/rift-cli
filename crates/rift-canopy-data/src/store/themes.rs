//! Theme overrides: `canopy_profile_theme_event`, one superseding row per
//! `(profile, slot)`, folded latest-wins into a
//! [`rift_canopy::ThemeOverrides`].
//!
//! Its own module because the slot key and the colour are validated at this
//! writer and tolerated at this reader, and no other relation shares either
//! rule.

use rift_core::{Result, RiftError, UlidSource};
use rift_store::{CanonicalStore, Value};

use super::{opt_text, opt_text_ref, runtime, CanopyStore, PROFILE_THEME_COLS};

impl CanopyStore {
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
}

#[cfg(test)]
mod tests {
    use rift_canopy::PersistOp;

    use crate::store::fixtures::{seed, store};

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
}
