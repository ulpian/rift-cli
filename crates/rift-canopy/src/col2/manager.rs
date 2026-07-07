//! The feature-install seam (P5 S2 / the Stage-5 handoff).
//!
//! Canopy decides *what* should change (via [`super::toggles::plan_flip`] →
//! [`FeatureAction`]s) but never installs anything itself — supply-chain gating,
//! download, and verification are Stage 5's job. The loop hands each action to an
//! injected [`FeatureManager`]; today the only implementation is
//! [`NullFeatureManager`] (a sink that succeeds without doing anything), so the
//! wiring is exercised end-to-end while no crate is actually installed. The real
//! Stage-5 manager implements the *same* trait with zero change to Canopy.

use super::toggles::FeatureAction;

/// Applies feature install/remove actions. Returns `Ok(())` on success or
/// `Err(reason)` with a short, display-safe reason on failure (never transcript or
/// key bytes) — the reason surfaces as a toggle's [`super::toggles::ToggleState::Failed`].
pub trait FeatureManager {
    /// Perform one install/remove/reinstall action.
    ///
    /// # Errors
    /// A short reason string when the action fails.
    fn apply(&mut self, action: &FeatureAction) -> Result<(), String>;
}

/// The no-op manager: accepts every action and does nothing (no crate is installed
/// or removed). The default until the Stage-5 manager lands.
#[derive(Debug, Default)]
pub struct NullFeatureManager;

impl FeatureManager for NullFeatureManager {
    fn apply(&mut self, _action: &FeatureAction) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::col2::toggles::{Feature, FeatureActionKind};

    #[test]
    fn null_manager_accepts_every_action() {
        let mut m = NullFeatureManager;
        for kind in [
            FeatureActionKind::Install,
            FeatureActionKind::Remove,
            FeatureActionKind::Reinstall,
        ] {
            assert!(m
                .apply(&FeatureAction {
                    feature: Feature::Fff,
                    kind,
                    path: "/r/x".into(),
                })
                .is_ok());
        }
    }
}
