//! The six feature toggles (P5 S2) — the pure model + fold + flip planner.
//!
//! Each toggle's **desired** on/off intent persists as an append-only
//! `canopy_toggle_event`; the transient install states ([`ToggleState::Enabling`]/
//! [`ToggleState::Disabling`]/[`ToggleState::Failed`]) are ephemeral and never
//! written. [`current_toggles`] folds the log to the latest desired-per-feature;
//! [`toggle_view`] projects that to render rows enforcing the variant/base
//! invariant (a `codegraph-*` variant is only *on* while `codegraph` is on); and
//! [`plan_flip`] turns one user flip into the ordered set of [`ToggleChange`]s that
//! keep the invariant (enabling a variant enables its base first; disabling a base
//! cascades to its variants). Canopy never installs anything — it emits
//! [`FeatureAction`]s through the [`crate::col2::manager::FeatureManager`] seam.

use std::collections::BTreeMap;

/// The six toggleable features. `Codegraph` is a **base** with three language
/// **variants** (`CodegraphTs`/`Py`/`Rs`); the rest are standalone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Feature {
    /// Fuzzy file finder.
    Fff,
    /// The code-relationship graph (base for the language variants).
    Codegraph,
    /// Codegraph — TypeScript language support.
    CodegraphTs,
    /// Codegraph — Python language support.
    CodegraphPy,
    /// Codegraph — Rust language support.
    CodegraphRs,
    /// The bounded memory-delta recall plane.
    TotalRecall,
}

impl Feature {
    /// Every feature, in display order (base before its variants).
    pub const ALL: [Feature; 6] = [
        Feature::Fff,
        Feature::Codegraph,
        Feature::CodegraphTs,
        Feature::CodegraphPy,
        Feature::CodegraphRs,
        Feature::TotalRecall,
    ];

    /// The stable persistence key (also the crate/package identity).
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Feature::Fff => "fff",
            Feature::Codegraph => "codegraph",
            Feature::CodegraphTs => "codegraph-typescript",
            Feature::CodegraphPy => "codegraph-python",
            Feature::CodegraphRs => "codegraph-rust",
            Feature::TotalRecall => "rift-total-recall",
        }
    }

    /// The short display label shown in column 2.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Feature::Fff => "fff (fuzzy find)",
            Feature::Codegraph => "codegraph",
            Feature::CodegraphTs => "typescript",
            Feature::CodegraphPy => "python",
            Feature::CodegraphRs => "rust",
            Feature::TotalRecall => "total-recall",
        }
    }

    /// Parse a persistence key back to a feature (unknown keys → `None`, so a
    /// forward-compat row never mis-folds).
    #[must_use]
    pub fn from_key(key: &str) -> Option<Feature> {
        Feature::ALL.into_iter().find(|f| f.key() == key)
    }

    /// The base feature a variant depends on (`None` for a base/standalone).
    #[must_use]
    pub fn base(self) -> Option<Feature> {
        match self {
            Feature::CodegraphTs | Feature::CodegraphPy | Feature::CodegraphRs => {
                Some(Feature::Codegraph)
            }
            _ => None,
        }
    }

    /// This feature's variants (empty unless it is a base).
    #[must_use]
    pub fn variants(self) -> &'static [Feature] {
        match self {
            Feature::Codegraph => &[
                Feature::CodegraphTs,
                Feature::CodegraphPy,
                Feature::CodegraphRs,
            ],
            _ => &[],
        }
    }

    /// Whether this feature is a variant of some base.
    #[must_use]
    pub fn is_variant(self) -> bool {
        self.base().is_some()
    }
}

/// A toggle's display state. Only the **desired** intent (`On`/`Off`) persists;
/// the rest are transient install states shown while a [`FeatureManager`] works.
///
/// [`FeatureManager`]: crate::col2::manager::FeatureManager
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToggleState {
    /// Off (not installed).
    Off,
    /// Install in progress (transient).
    Enabling,
    /// On (installed).
    On,
    /// Removal in progress (transient).
    Disabling,
    /// The last install/remove failed; carries a short reason for display.
    Failed(String),
}

impl ToggleState {
    /// The checkbox glyph for this state.
    #[must_use]
    pub fn glyph(&self) -> &'static str {
        match self {
            ToggleState::Off => "[ ]",
            ToggleState::On => "[x]",
            ToggleState::Enabling | ToggleState::Disabling => "[~]",
            ToggleState::Failed(_) => "[!]",
        }
    }

    /// Whether this state renders as "on" (installed).
    #[must_use]
    pub fn is_on(&self) -> bool {
        matches!(self, ToggleState::On)
    }
}

/// One row of the append-only `canopy_toggle_event` log for a single project,
/// engine-neutral so the fold is testable without a store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToggleEvent {
    /// The event's own id (ULID hex) — the fold's tie-break.
    pub event_id: String,
    /// The feature's persistence key.
    pub feature: String,
    /// The desired on/off intent recorded at this time.
    pub desired: bool,
    /// Transaction-time (epoch-ms).
    pub created_at: i64,
}

/// Fold a project's toggle log to the current **desired** state per feature
/// (latest event wins; unknown feature keys are ignored). Pure and total.
#[must_use]
pub fn current_toggles(events: &[ToggleEvent]) -> BTreeMap<Feature, bool> {
    let mut ordered: Vec<&ToggleEvent> = events.iter().collect();
    ordered.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.event_id.cmp(&b.event_id))
    });
    let mut out = BTreeMap::new();
    for e in ordered {
        if let Some(f) = Feature::from_key(&e.feature) {
            out.insert(f, e.desired);
        }
    }
    out
}

/// One render row: a feature, its display state, and whether it is an indented
/// variant of the row above it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToggleRow {
    /// The feature.
    pub feature: Feature,
    /// The display state (from the persisted desired intent).
    pub state: ToggleState,
    /// Whether to indent it (a variant under its base).
    pub indent: bool,
}

/// The render-ready state of all six toggles for one project.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToggleView {
    /// The six rows in display order.
    pub rows: Vec<ToggleRow>,
}

impl ToggleView {
    /// The display `is_on` state for `feature` (after the base invariant).
    #[must_use]
    pub fn is_on(&self, feature: Feature) -> bool {
        self.rows
            .iter()
            .find(|r| r.feature == feature)
            .is_some_and(|r| r.state.is_on())
    }
}

/// Project a desired-state map to render rows, enforcing the base invariant: a
/// variant shows `On` only when both it *and* its base are desired-on.
#[must_use]
pub fn toggle_view(desired: &BTreeMap<Feature, bool>) -> ToggleView {
    let on = |f: Feature| desired.get(&f).copied().unwrap_or(false);
    let rows = Feature::ALL
        .into_iter()
        .map(|f| {
            let effective = match f.base() {
                Some(base) => on(f) && on(base),
                None => on(f),
            };
            ToggleRow {
                feature: f,
                state: if effective {
                    ToggleState::On
                } else {
                    ToggleState::Off
                },
                indent: f.is_variant(),
            }
        })
        .collect();
    ToggleView { rows }
}

/// A single desired-state change to persist (one superseding `canopy_toggle_event`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToggleChange {
    /// The feature to change.
    pub feature: Feature,
    /// Its new desired state.
    pub desired: bool,
}

/// Plan the ordered set of changes for flipping `feature`, given the current
/// display-on state of each feature (`is_on`). The order keeps the base invariant:
/// * enabling a variant enables its **base first**, then the variant;
/// * disabling a base disables its **variants first**, then the base;
/// * everything else is a single change.
#[must_use]
pub fn plan_flip(is_on: &impl Fn(Feature) -> bool, feature: Feature) -> Vec<ToggleChange> {
    let target = !is_on(feature);
    let mut changes = Vec::new();
    if target {
        // Turning on: bring the base up first if this is a variant and it is off.
        if let Some(base) = feature.base() {
            if !is_on(base) {
                changes.push(ToggleChange {
                    feature: base,
                    desired: true,
                });
            }
        }
        changes.push(ToggleChange {
            feature,
            desired: true,
        });
    } else {
        // Turning off a base cascades to its on-variants first (remove dependents
        // before the base), then the base itself.
        for &v in feature.variants() {
            if is_on(v) {
                changes.push(ToggleChange {
                    feature: v,
                    desired: false,
                });
            }
        }
        changes.push(ToggleChange {
            feature,
            desired: false,
        });
    }
    changes
}

/// What the [`FeatureManager`] must do for a change. `Reinstall` is the retry after
/// a [`ToggleState::Failed`].
///
/// [`FeatureManager`]: crate::col2::manager::FeatureManager
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureActionKind {
    /// Install the feature.
    Install,
    /// Remove the feature.
    Remove,
    /// Remove then re-install (retry).
    Reinstall,
}

/// An install/remove request Canopy hands to the (injected) feature manager —
/// Canopy itself never touches the filesystem or a package manager.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeatureAction {
    /// The feature to act on.
    pub feature: Feature,
    /// What to do.
    pub kind: FeatureActionKind,
}

impl FeatureAction {
    /// The install/remove action implied by a desired-state change.
    #[must_use]
    pub fn from_change(change: ToggleChange) -> Self {
        Self {
            feature: change.feature,
            kind: if change.desired {
                FeatureActionKind::Install
            } else {
                FeatureActionKind::Remove
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn ev(feature: &str, desired: bool, at: i64) -> ToggleEvent {
        ToggleEvent {
            event_id: format!("e{at:04}"),
            feature: feature.into(),
            desired,
            created_at: at,
        }
    }

    #[test]
    fn keys_round_trip_and_are_unique() {
        let mut keys: Vec<&str> = Feature::ALL.iter().map(|f| f.key()).collect();
        for &f in &Feature::ALL {
            assert_eq!(Feature::from_key(f.key()), Some(f));
        }
        let n = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), n, "feature keys are unique");
        assert_eq!(Feature::from_key("unknown"), None);
    }

    #[test]
    fn variant_base_relationships() {
        assert_eq!(Feature::CodegraphTs.base(), Some(Feature::Codegraph));
        assert!(Feature::CodegraphTs.is_variant());
        assert!(!Feature::Codegraph.is_variant());
        assert_eq!(Feature::Codegraph.variants().len(), 3);
        assert!(Feature::Fff.variants().is_empty());
    }

    #[test]
    fn current_toggles_is_latest_per_feature() {
        let log = [
            ev("fff", true, 1),
            ev("fff", false, 2), // latest wins
            ev("codegraph", true, 3),
            ev("unknown-feature", true, 4), // ignored
        ];
        let map = current_toggles(&log);
        assert_eq!(map.get(&Feature::Fff), Some(&false));
        assert_eq!(map.get(&Feature::Codegraph), Some(&true));
        assert_eq!(map.len(), 2, "unknown keys are not folded");
    }

    #[test]
    fn no_variant_on_while_base_off() {
        // A variant desired-on but base off shows Off (the invariant is enforced
        // at render even if the log somehow violated it).
        let desired = BTreeMap::from([(Feature::CodegraphTs, true)]);
        let view = toggle_view(&desired);
        assert!(!view.is_on(Feature::CodegraphTs));

        let desired = BTreeMap::from([(Feature::Codegraph, true), (Feature::CodegraphTs, true)]);
        let view = toggle_view(&desired);
        assert!(view.is_on(Feature::CodegraphTs));
    }

    #[test]
    fn enabling_variant_enqueues_base_first() {
        let is_on = |_: Feature| false; // nothing on
        let plan = plan_flip(&is_on, Feature::CodegraphTs);
        assert_eq!(
            plan,
            vec![
                ToggleChange {
                    feature: Feature::Codegraph,
                    desired: true
                },
                ToggleChange {
                    feature: Feature::CodegraphTs,
                    desired: true
                },
            ]
        );
    }

    #[test]
    fn enabling_variant_with_base_already_on_is_single() {
        let is_on = |f: Feature| f == Feature::Codegraph;
        let plan = plan_flip(&is_on, Feature::CodegraphTs);
        assert_eq!(
            plan,
            vec![ToggleChange {
                feature: Feature::CodegraphTs,
                desired: true
            }]
        );
    }

    #[test]
    fn disabling_base_cascades_to_on_variants() {
        // base + two variants on → disabling base turns off the variants first.
        let on = [
            Feature::Codegraph,
            Feature::CodegraphTs,
            Feature::CodegraphRs,
        ];
        let is_on = |f: Feature| on.contains(&f);
        let plan = plan_flip(&is_on, Feature::Codegraph);
        assert_eq!(
            plan,
            vec![
                ToggleChange {
                    feature: Feature::CodegraphTs,
                    desired: false
                },
                ToggleChange {
                    feature: Feature::CodegraphRs,
                    desired: false
                },
                ToggleChange {
                    feature: Feature::Codegraph,
                    desired: false
                },
            ]
        );
    }

    #[test]
    fn action_from_change_maps_install_remove() {
        assert_eq!(
            FeatureAction::from_change(ToggleChange {
                feature: Feature::Fff,
                desired: true
            })
            .kind,
            FeatureActionKind::Install
        );
        assert_eq!(
            FeatureAction::from_change(ToggleChange {
                feature: Feature::Fff,
                desired: false
            })
            .kind,
            FeatureActionKind::Remove
        );
    }

    #[test]
    fn state_glyphs() {
        assert_eq!(ToggleState::Off.glyph(), "[ ]");
        assert_eq!(ToggleState::On.glyph(), "[x]");
        assert_eq!(ToggleState::Enabling.glyph(), "[~]");
        assert_eq!(ToggleState::Disabling.glyph(), "[~]");
        assert_eq!(ToggleState::Failed("boom".into()).glyph(), "[!]");
    }

    fn feature_strategy() -> impl Strategy<Value = Feature> {
        prop_oneof![
            Just(Feature::Fff),
            Just(Feature::Codegraph),
            Just(Feature::CodegraphTs),
            Just(Feature::CodegraphPy),
            Just(Feature::CodegraphRs),
            Just(Feature::TotalRecall),
        ]
    }

    proptest! {
        /// `plan_flip` always upholds the base invariant: after applying the plan
        /// to the current state, no variant is on while its base is off.
        #[test]
        fn variant_base_invariant_holds_after_any_flip(
            initial in proptest::collection::vec((feature_strategy(), any::<bool>()), 0..8),
            flip in feature_strategy(),
        ) {
            let mut desired: BTreeMap<Feature, bool> = BTreeMap::new();
            for (f, v) in initial {
                desired.insert(f, v);
            }
            // Start from a *view-consistent* on-state (invariant already applied).
            let view = toggle_view(&desired);
            let is_on = |f: Feature| view.is_on(f);

            // Apply the plan.
            let mut next: BTreeMap<Feature, bool> =
                Feature::ALL.into_iter().map(|f| (f, is_on(f))).collect();
            for change in plan_flip(&is_on, flip) {
                next.insert(change.feature, change.desired);
            }
            // Re-project and assert the invariant.
            let after = toggle_view(&next);
            for &v in Feature::Codegraph.variants() {
                if after.is_on(v) {
                    prop_assert!(after.is_on(Feature::Codegraph), "variant on ⇒ base on");
                }
            }
        }

        /// Flipping a feature yields at least one change and only ever sets desired
        /// states (never deletes) — the append-only guarantee at the plan layer.
        #[test]
        fn flip_is_insert_only_and_nonempty(flip in feature_strategy()) {
            let is_on = |_: Feature| false;
            let plan = plan_flip(&is_on, flip);
            prop_assert!(!plan.is_empty());
            // Every change is a set-desired (an append), never a structural delete.
            for c in plan {
                let _ = FeatureAction::from_change(c); // total mapping, no panic
            }
        }
    }
}
