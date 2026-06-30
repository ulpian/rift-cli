//! The optional-package toggle registry.
//!
//! `fff`, `codegraph`, and `rift-nic-gauge` are optional, toggle-on/off/reinstall
//! without touching core data (`01-vision` principle 8). A user can run pure
//! rift-shadow + recall + canopy and nothing else. Each package carries a trust
//! level so a not-yet-vetted package can be sandboxed.

/// An optional package.
#[derive(
    Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Debug, ::serde::Serialize, ::serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Feature {
    /// Warm-index fuzzy file search.
    Fff,
    /// Code-relationship graph.
    Codegraph,
    /// Context-window % meter.
    NicGauge,
}

impl Feature {
    /// All optional features.
    #[must_use]
    pub fn all() -> &'static [Feature] {
        &[Feature::Fff, Feature::Codegraph, Feature::NicGauge]
    }
}

/// The trust level a package runs at.
#[derive(Clone, Copy, PartialEq, Eq, Debug, ::serde::Serialize, ::serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Trust {
    /// First-party / vetted: full capability.
    Trusted,
    /// Third-party / not yet vetted: sandboxed.
    Sandboxed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    enabled: bool,
    trust: Trust,
}

/// The per-machine optional-package registry.
#[derive(Clone, Debug, Default)]
pub struct FeatureRegistry {
    fff: Option<Entry>,
    codegraph: Option<Entry>,
    nic_gauge: Option<Entry>,
}

impl FeatureRegistry {
    /// A registry with nothing installed (pure-core default).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn slot(&mut self, f: Feature) -> &mut Option<Entry> {
        match f {
            Feature::Fff => &mut self.fff,
            Feature::Codegraph => &mut self.codegraph,
            Feature::NicGauge => &mut self.nic_gauge,
        }
    }

    fn peek(&self, f: Feature) -> &Option<Entry> {
        match f {
            Feature::Fff => &self.fff,
            Feature::Codegraph => &self.codegraph,
            Feature::NicGauge => &self.nic_gauge,
        }
    }

    /// Install a package at a trust level (enabled). Idempotent.
    pub fn install(&mut self, f: Feature, trust: Trust) {
        *self.slot(f) = Some(Entry {
            enabled: true,
            trust,
        });
    }

    /// Remove a package entirely (does not touch core data).
    pub fn remove(&mut self, f: Feature) {
        *self.slot(f) = None;
    }

    /// Reinstall: remove then install fresh at the given trust.
    pub fn reinstall(&mut self, f: Feature, trust: Trust) {
        self.remove(f);
        self.install(f, trust);
    }

    /// Disable without removing (keeps trust on re-enable).
    pub fn disable(&mut self, f: Feature) {
        if let Some(e) = self.slot(f) {
            e.enabled = false;
        }
    }

    /// Whether a package is installed and enabled.
    #[must_use]
    pub fn is_enabled(&self, f: Feature) -> bool {
        self.peek(f).is_some_and(|e| e.enabled)
    }

    /// Whether a package is sandboxed (untrusted). `false` if not installed.
    #[must_use]
    pub fn is_sandboxed(&self, f: Feature) -> bool {
        self.peek(f).is_some_and(|e| e.trust == Trust::Sandboxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_core_by_default() {
        let r = FeatureRegistry::new();
        for f in Feature::all() {
            assert!(!r.is_enabled(*f), "{f:?} must be off by default");
        }
    }

    #[test]
    fn install_remove_reinstall_cycle() {
        let mut r = FeatureRegistry::new();
        r.install(Feature::Fff, Trust::Trusted);
        assert!(r.is_enabled(Feature::Fff));
        assert!(!r.is_sandboxed(Feature::Fff));
        r.remove(Feature::Fff);
        assert!(!r.is_enabled(Feature::Fff));
        r.reinstall(Feature::Fff, Trust::Sandboxed);
        assert!(r.is_enabled(Feature::Fff));
        assert!(r.is_sandboxed(Feature::Fff));
    }

    #[test]
    fn disable_keeps_installed() {
        let mut r = FeatureRegistry::new();
        r.install(Feature::Codegraph, Trust::Trusted);
        r.disable(Feature::Codegraph);
        assert!(!r.is_enabled(Feature::Codegraph));
        // toggling a different feature does not affect this one
        r.install(Feature::NicGauge, Trust::Trusted);
        assert!(r.is_enabled(Feature::NicGauge));
        assert!(!r.is_enabled(Feature::Codegraph));
    }
}
