//! The append-only profile plane folds (ADR-0036).
//!
//! A **profile** is a named, machine-global operator view of the dashboard: its
//! own project membership over the global catalogue, its own per-(profile,
//! project) arrangements, and (M5.11) its own theme. Every profile mutation —
//! create / rename / remove / select — is a new, superseding **event** (never an
//! in-place edit), so the log is immutable by construction (04 §4.3) and the
//! current state is a pure fold. [`current_profiles`] replays the profile log
//! into the live profile list; [`active_profile_id`] names the profile the
//! dashboard shows (the latest `selected`, falling back to the default when it
//! was removed); [`current_membership`] folds one profile's membership view.
//! All folds are pure and total — any event sequence yields a well-formed
//! result — and engine-neutral so they test without a store.

use std::collections::BTreeMap;

/// The operator-facing budget on a profile name (ADR-0037 §4): trimmed,
/// non-empty, at most this many characters, unique among live profiles.
pub const MAX_PROFILE_NAME: usize = 24;

/// The seeded default profile's name (ADR-0036 §3). Ordinary data — renaming
/// the default is an ordinary `renamed` event, and nothing downstream keys on
/// this string; it only names the seed.
pub const DEFAULT_PROFILE_NAME: &str = "default";

/// A single mutation to the profile plane. Every variant is an **insert** into
/// the append-only log; corrections are new events, so the log is never edited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileOp {
    /// Mint a profile (its `profile_id` is the stable key; `name` the display).
    Created {
        /// The operator-chosen display name.
        name: String,
    },
    /// Change a profile's display name.
    Renamed {
        /// The new display name.
        name: String,
    },
    /// Hide a profile (a superseding `removed` event; history kept).
    Removed,
    /// Make this profile the one the dashboard shows (latest `selected` wins).
    Selected,
}

/// One row of the append-only `canopy_profile_event` log, engine-neutral so the
/// fold is testable without a store. `created_at` is transaction-time epoch-ms;
/// `event_id` is a ULID (lexical time-order) used as the tie-break.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileEvent {
    /// The event's own id (ULID hex).
    pub event_id: String,
    /// The profile this event concerns (stable across its whole history).
    pub profile_id: String,
    /// What changed.
    pub op: ProfileOp,
    /// Transaction-time (epoch-ms).
    pub created_at: i64,
}

/// The current state of one live profile — the fold's output row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileEntry {
    /// The profile's stable id (ULID hex).
    pub profile_id: String,
    /// The display name (latest `Created`/`Renamed` wins).
    pub name: String,
    /// When the profile was first created (epoch-ms) — the list order.
    pub created_at: i64,
}

/// The mutable per-profile accumulator used while replaying the log.
#[derive(Default)]
struct Acc {
    present: bool,
    name: String,
    created_at: Option<i64>,
}

/// Sort `events` into replay order: transaction-time, then the ULID event id as
/// the stable tie-break (so a caller's row order can never change a fold).
fn replay_order(events: &[ProfileEvent]) -> Vec<&ProfileEvent> {
    let mut ordered: Vec<&ProfileEvent> = events.iter().collect();
    ordered.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.event_id.cmp(&b.event_id))
    });
    ordered
}

/// Replay `events` and return the live profiles, sorted by creation time (then
/// `profile_id`). Pure and total — latest `Created`/`Renamed` names a profile,
/// a trailing `Removed` hides it (history kept), a re-`Created` restores it,
/// and a rename/select without a create is ignored.
#[must_use]
pub fn current_profiles(events: &[ProfileEvent]) -> Vec<ProfileEntry> {
    let mut acc: BTreeMap<&str, Acc> = BTreeMap::new();
    for ev in replay_order(events) {
        let a = acc.entry(ev.profile_id.as_str()).or_default();
        match &ev.op {
            ProfileOp::Created { name } => {
                a.present = true;
                a.name.clone_from(name);
                a.created_at.get_or_insert(ev.created_at);
            }
            // A rename before any create is meaningless; ignore until created.
            ProfileOp::Renamed { name } => {
                if a.created_at.is_some() {
                    a.name.clone_from(name);
                }
            }
            ProfileOp::Removed => a.present = false,
            ProfileOp::Selected => {}
        }
    }
    let mut entries: Vec<ProfileEntry> = acc
        .into_iter()
        .filter_map(|(id, a)| {
            let created_at = a.created_at?;
            if !a.present {
                return None;
            }
            Some(ProfileEntry {
                profile_id: id.to_string(),
                name: a.name,
                created_at,
            })
        })
        .collect();
    entries.sort_by(|x, y| {
        x.created_at
            .cmp(&y.created_at)
            .then_with(|| x.profile_id.cmp(&y.profile_id))
    });
    entries
}

/// The seeded **default profile**'s id: the profile the earliest `Created`
/// event minted (regardless of later renames or removal). `None` on an
/// unseeded (empty) log. The default profile is the legacy-fallback scope
/// (ADR-0036 §4) and the fallback when the selected profile was removed.
#[must_use]
pub fn default_profile_id(events: &[ProfileEvent]) -> Option<String> {
    replay_order(events)
        .into_iter()
        .find(|e| matches!(e.op, ProfileOp::Created { .. }))
        .map(|e| e.profile_id.clone())
}

/// The **active** profile's id: the latest `Selected` event whose profile is
/// still live; else the default profile if live; else the earliest live
/// profile; `None` only when no profile is live (an unseeded log).
#[must_use]
pub fn active_profile_id(events: &[ProfileEvent]) -> Option<String> {
    let live = current_profiles(events);
    let is_live = |id: &str| live.iter().any(|p| p.profile_id == id);
    let selected = replay_order(events)
        .into_iter()
        .rev()
        .find(|e| matches!(e.op, ProfileOp::Selected))
        .map(|e| e.profile_id.clone());
    if let Some(id) = selected {
        if is_live(&id) {
            return Some(id);
        }
    }
    if let Some(id) = default_profile_id(events) {
        if is_live(&id) {
            return Some(id);
        }
    }
    live.first().map(|p| p.profile_id.clone())
}

/// Normalize and validate an operator-entered profile name (ADR-0037 §4):
/// trimmed, non-empty, at most [`MAX_PROFILE_NAME`] characters, and unique
/// among `live` names (rename passes its own current name as allowed via
/// `except`). Returns the trimmed name, or `None` when the rules refuse it.
#[must_use]
pub fn normalize_profile_name(
    raw: &str,
    live: &[ProfileEntry],
    except: Option<&str>,
) -> Option<String> {
    let name = raw.trim();
    if name.is_empty() || name.chars().count() > MAX_PROFILE_NAME {
        return None;
    }
    let taken = live
        .iter()
        .any(|p| p.name == name && Some(p.profile_id.as_str()) != except);
    if taken {
        return None;
    }
    Some(name.to_string())
}

// --- membership -------------------------------------------------------------

/// A single mutation to one profile's membership view over the global
/// catalogue. Every variant is an **insert**; `canopy_project_event` stays the
/// registration source of truth (ADR-0036 §2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileProjectOp {
    /// Include a catalogue project in this profile's view.
    Added {
        /// An explicit order key (copy-on-create preserves the source order);
        /// `None` sorts by inclusion time.
        order_key: Option<i64>,
    },
    /// Hide a project from this profile's view (a superseding event).
    Removed,
    /// Set an explicit ordering key within this profile.
    Reordered {
        /// The new order key; entries sort ascending by it.
        order_key: i64,
    },
}

/// One row of the append-only `canopy_profile_project_event` log for a single
/// profile, engine-neutral so the fold is testable without a store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileProjectEvent {
    /// The event's own id (ULID hex).
    pub event_id: String,
    /// The catalogue project this event concerns.
    pub project_id: String,
    /// What changed.
    pub op: ProfileProjectOp,
    /// Transaction-time (epoch-ms).
    pub created_at: i64,
}

/// One project in a profile's current membership view — the fold's output row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MembershipEntry {
    /// The catalogue project id.
    pub project_id: String,
    /// The effective sort key (explicit key, else inclusion time).
    pub order_key: i64,
}

/// The mutable per-project accumulator used while replaying a membership log.
#[derive(Default)]
struct MemberAcc {
    present: bool,
    added_at: Option<i64>,
    explicit_order: Option<i64>,
}

/// Replay one profile's membership `events` and return its current view,
/// sorted by `(order_key, project_id)`. Pure and total — latest event per
/// project wins, a `Removed` hides (history kept), a reorder before any add is
/// ignored. **An empty result is meaningful only alongside the fallback rule**
/// (ADR-0036 §4): the default profile with no membership events shows the full
/// catalogue — that fallback lives at the read seam, not in this fold.
#[must_use]
pub fn current_membership(events: &[ProfileProjectEvent]) -> Vec<MembershipEntry> {
    let mut ordered: Vec<&ProfileProjectEvent> = events.iter().collect();
    ordered.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.event_id.cmp(&b.event_id))
    });
    let mut acc: BTreeMap<&str, MemberAcc> = BTreeMap::new();
    for ev in ordered {
        let a = acc.entry(ev.project_id.as_str()).or_default();
        match &ev.op {
            ProfileProjectOp::Added { order_key } => {
                a.present = true;
                a.added_at.get_or_insert(ev.created_at);
                if order_key.is_some() {
                    a.explicit_order = *order_key;
                }
            }
            ProfileProjectOp::Removed => a.present = false,
            ProfileProjectOp::Reordered { order_key } => {
                if a.added_at.is_some() {
                    a.explicit_order = Some(*order_key);
                }
            }
        }
    }
    let mut entries: Vec<MembershipEntry> = acc
        .into_iter()
        .filter_map(|(id, a)| {
            let added_at = a.added_at?;
            if !a.present {
                return None;
            }
            Some(MembershipEntry {
                project_id: id.to_string(),
                order_key: a.explicit_order.unwrap_or(added_at),
            })
        })
        .collect();
    entries.sort_by(|x, y| {
        x.order_key
            .cmp(&y.order_key)
            .then_with(|| x.project_id.cmp(&y.project_id))
    });
    entries
}

// --- the loader seam ----------------------------------------------------------

/// The injected profile loader (ADR-0036 §6) — the loop's one read door into
/// the profile plane, generalising the M5.9 layout loader: the active profile,
/// the live profiles, a profile's effective project view, and the (profile,
/// project) arrangement all load through this seam. The reducer only *requests*
/// loads (via [`crate::state::Effects`]); the loop performs them; the binary
/// wires the seam to the canopy store and stays wiring.
pub trait ProfileLoader {
    /// The live profiles plus the active profile's id (`None` active only on an
    /// unseeded store).
    ///
    /// # Errors
    /// [`rift_core::RiftError`] if the profile log cannot be read.
    fn profiles(&mut self) -> rift_core::Result<(Option<String>, Vec<ProfileEntry>)>;

    /// One profile's effective project view (the membership fold over the
    /// catalogue, with the default profile's full-catalogue fallback).
    ///
    /// # Errors
    /// [`rift_core::RiftError`] if a log cannot be read.
    fn projects(&mut self, profile: &str) -> rift_core::Result<Vec<crate::Project>>;

    /// The saved arrangement for `(profile, project)`, or `None` for the
    /// default (including the legacy fallback rules — ADR-0036 §4).
    ///
    /// # Errors
    /// [`rift_core::RiftError`] if a log cannot be read.
    fn layout(
        &mut self,
        profile: &str,
        path: &str,
    ) -> rift_core::Result<Option<crate::layout_model::LayoutModel>>;

    /// One profile's theme-slot overrides (ADR-0038): latest-wins per
    /// (profile, slot); no rows ⇒ no overrides ⇒ Rift dark, byte-identical.
    ///
    /// # Errors
    /// [`rift_core::RiftError`] if the theme log cannot be read.
    fn theme(&mut self, profile: &str) -> rift_core::Result<crate::theme::ThemeOverrides>;
}

/// A loader that serves nothing: no profiles, empty views, default layouts —
/// the headless/test double (the app-loop tests inject it like the old
/// `|_| Ok(None)` layout closure).
#[derive(Clone, Copy, Debug, Default)]
pub struct NullProfileLoader;

impl ProfileLoader for NullProfileLoader {
    fn profiles(&mut self) -> rift_core::Result<(Option<String>, Vec<ProfileEntry>)> {
        Ok((None, Vec::new()))
    }

    fn projects(&mut self, _profile: &str) -> rift_core::Result<Vec<crate::Project>> {
        Ok(Vec::new())
    }

    fn layout(
        &mut self,
        _profile: &str,
        _path: &str,
    ) -> rift_core::Result<Option<crate::layout_model::LayoutModel>> {
        Ok(None)
    }

    fn theme(&mut self, _profile: &str) -> rift_core::Result<crate::theme::ThemeOverrides> {
        Ok(crate::theme::ThemeOverrides::NONE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn ev(id: &str, op: ProfileOp, at: i64) -> ProfileEvent {
        ProfileEvent {
            event_id: format!("e{at:04}"),
            profile_id: id.into(),
            op,
            created_at: at,
        }
    }

    fn created(id: &str, name: &str, at: i64) -> ProfileEvent {
        ev(id, ProfileOp::Created { name: name.into() }, at)
    }

    #[test]
    fn empty_log_folds_empty() {
        assert!(current_profiles(&[]).is_empty());
        assert_eq!(default_profile_id(&[]), None);
        assert_eq!(active_profile_id(&[]), None);
    }

    #[test]
    fn seed_shape_folds_to_one_active_default() {
        let log = [
            created("p1", "default", 1),
            ev("p1", ProfileOp::Selected, 1),
        ];
        let live = current_profiles(&log);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].name, "default");
        assert_eq!(default_profile_id(&log).as_deref(), Some("p1"));
        assert_eq!(active_profile_id(&log).as_deref(), Some("p1"));
    }

    #[test]
    fn rename_is_an_ordinary_superseding_event() {
        let log = [
            created("p1", "default", 1),
            ev(
                "p1",
                ProfileOp::Renamed {
                    name: "work".into(),
                },
                2,
            ),
        ];
        assert_eq!(current_profiles(&log)[0].name, "work");
        // The default profile id survives the rename (it names the seed).
        assert_eq!(default_profile_id(&log).as_deref(), Some("p1"));
    }

    #[test]
    fn latest_selected_names_the_active_profile() {
        let log = [
            created("p1", "default", 1),
            ev("p1", ProfileOp::Selected, 1),
            created("p2", "lab", 2),
            ev("p2", ProfileOp::Selected, 3),
        ];
        assert_eq!(active_profile_id(&log).as_deref(), Some("p2"));
    }

    #[test]
    fn removed_active_falls_back_to_the_default() {
        let log = [
            created("p1", "default", 1),
            ev("p1", ProfileOp::Selected, 1),
            created("p2", "lab", 2),
            ev("p2", ProfileOp::Selected, 3),
            ev("p2", ProfileOp::Removed, 4),
        ];
        assert_eq!(active_profile_id(&log).as_deref(), Some("p1"));
    }

    #[test]
    fn removed_default_falls_back_to_the_earliest_live() {
        let log = [
            created("p1", "default", 1),
            ev("p1", ProfileOp::Selected, 1),
            created("p2", "lab", 2),
            ev("p2", ProfileOp::Selected, 3),
            ev("p2", ProfileOp::Removed, 4),
            ev("p1", ProfileOp::Removed, 5),
            created("p3", "new", 6),
        ];
        assert_eq!(active_profile_id(&log).as_deref(), Some("p3"));
    }

    #[test]
    fn rename_without_create_is_ignored() {
        let log = [ev("ghost", ProfileOp::Renamed { name: "x".into() }, 1)];
        assert!(current_profiles(&log).is_empty());
    }

    #[test]
    fn name_rules_trim_bound_and_dedupe() {
        let live = vec![ProfileEntry {
            profile_id: "p1".into(),
            name: "work".into(),
            created_at: 1,
        }];
        assert_eq!(
            normalize_profile_name("  lab  ", &live, None).as_deref(),
            Some("lab")
        );
        assert_eq!(normalize_profile_name("   ", &live, None), None);
        assert_eq!(normalize_profile_name("", &live, None), None);
        let long = "x".repeat(MAX_PROFILE_NAME + 1);
        assert_eq!(normalize_profile_name(&long, &live, None), None);
        let max = "x".repeat(MAX_PROFILE_NAME);
        assert_eq!(
            normalize_profile_name(&max, &live, None).as_deref(),
            Some(max.as_str())
        );
        // Duplicate among live names is refused …
        assert_eq!(normalize_profile_name("work", &live, None), None);
        // … unless it is the profile's own current name (rename to itself).
        assert_eq!(
            normalize_profile_name("work", &live, Some("p1")).as_deref(),
            Some("work")
        );
    }

    fn mev(pid: &str, op: ProfileProjectOp, at: i64) -> ProfileProjectEvent {
        ProfileProjectEvent {
            event_id: format!("e{at:04}"),
            project_id: pid.into(),
            op,
            created_at: at,
        }
    }

    #[test]
    fn membership_add_remove_fold_latest_wins() {
        let log = [
            mev("a", ProfileProjectOp::Added { order_key: None }, 1),
            mev("b", ProfileProjectOp::Added { order_key: None }, 2),
            mev("a", ProfileProjectOp::Removed, 3),
        ];
        let m = current_membership(&log);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].project_id, "b");
    }

    #[test]
    fn membership_copy_order_keys_are_preserved() {
        // Copy-on-create writes explicit order keys mirroring the source view.
        let log = [
            mev("b", ProfileProjectOp::Added { order_key: Some(1) }, 5),
            mev("a", ProfileProjectOp::Added { order_key: Some(2) }, 5),
        ];
        let m = current_membership(&log);
        assert_eq!(m[0].project_id, "b");
        assert_eq!(m[1].project_id, "a");
    }

    #[test]
    fn membership_reorder_overrides_inclusion_order() {
        let log = [
            mev("a", ProfileProjectOp::Added { order_key: None }, 1),
            mev("b", ProfileProjectOp::Added { order_key: None }, 2),
            mev("a", ProfileProjectOp::Reordered { order_key: 9 }, 3),
        ];
        let m = current_membership(&log);
        assert_eq!(m[0].project_id, "b");
        assert_eq!(m[1].project_id, "a");
    }

    #[test]
    fn membership_reorder_without_add_is_ignored() {
        let log = [mev(
            "ghost",
            ProfileProjectOp::Reordered { order_key: 1 },
            1,
        )];
        assert!(current_membership(&log).is_empty());
    }

    // --- property tests -----------------------------------------------------

    fn profile_op_strategy() -> impl Strategy<Value = ProfileOp> {
        prop_oneof![
            "[a-z]{1,6}".prop_map(|name| ProfileOp::Created { name }),
            "[a-z]{1,6}".prop_map(|name| ProfileOp::Renamed { name }),
            Just(ProfileOp::Removed),
            Just(ProfileOp::Selected),
        ]
    }

    proptest! {
        /// The profile fold is total and duplicate-free, and the active profile
        /// (when named) is always a live profile — validity never dangles.
        #[test]
        fn active_profile_is_always_live(
            raw in proptest::collection::vec((0usize..3, profile_op_strategy(), 1i64..50), 0..40)
        ) {
            let events: Vec<ProfileEvent> = raw
                .into_iter()
                .enumerate()
                .map(|(i, (pid, op, at))| ProfileEvent {
                    event_id: format!("e{i:04}"),
                    profile_id: format!("p{pid}"),
                    op,
                    created_at: at,
                })
                .collect();
            let live = current_profiles(&events);
            let mut ids: Vec<&String> = live.iter().map(|p| &p.profile_id).collect();
            let n = ids.len();
            ids.sort();
            ids.dedup();
            prop_assert_eq!(ids.len(), n, "profile ids are unique");

            match active_profile_id(&events) {
                Some(id) => prop_assert!(
                    live.iter().any(|p| p.profile_id == id),
                    "the active profile is live"
                ),
                None => prop_assert!(live.is_empty(), "no active only when none live"),
            }
        }

        /// The membership fold is total, duplicate-free, and deterministically
        /// sorted for any event sequence.
        #[test]
        fn membership_fold_is_latest_valid(
            raw in proptest::collection::vec(
                (0usize..4, prop_oneof![
                    proptest::option::of(0i64..8).prop_map(|order_key| ProfileProjectOp::Added { order_key }),
                    Just(ProfileProjectOp::Removed),
                    (0i64..8).prop_map(|order_key| ProfileProjectOp::Reordered { order_key }),
                ], 1i64..50),
                0..40,
            )
        ) {
            let events: Vec<ProfileProjectEvent> = raw
                .into_iter()
                .enumerate()
                .map(|(i, (pid, op, at))| ProfileProjectEvent {
                    event_id: format!("e{i:04}"),
                    project_id: format!("p{pid}"),
                    op,
                    created_at: at,
                })
                .collect();
            let m = current_membership(&events);
            let mut ids: Vec<&String> = m.iter().map(|e| &e.project_id).collect();
            let n = ids.len();
            ids.sort();
            ids.dedup();
            prop_assert_eq!(ids.len(), n, "project ids are unique");
            for w in m.windows(2) {
                prop_assert!(
                    (w[0].order_key, &w[0].project_id) <= (w[1].order_key, &w[1].project_id),
                    "membership is deterministically sorted"
                );
            }
        }
    }
}
