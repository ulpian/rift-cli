//! The canopy store behind the profile loader seam (ADR-0036 §6).
//!
//! One public adapter — every profile-plane read the dashboard performs
//! (live profiles + active id, a profile's membership, a (profile, project)
//! arrangement, a profile's theme) crosses it. Public here, not private to
//! the binary, so the interactive loop, the headless one-frame render, and
//! store-backed tests all wire the **same** loader: the W0 defect class was
//! a load only the interactive path performed, which no test could see.

use rift_canopy::{LayoutModel, ProfileEntry, ProfileLoader, Project, ThemeOverrides};
use rift_core::Result;

use crate::store::CanopyStore;

/// [`ProfileLoader`] over the machine-global [`CanopyStore`] — the production
/// impl of the seam; the binary stays wiring.
pub struct StoreProfileLoader<'a> {
    store: &'a CanopyStore,
}

impl<'a> StoreProfileLoader<'a> {
    /// The loader over an open store.
    #[must_use]
    pub fn new(store: &'a CanopyStore) -> Self {
        Self { store }
    }
}

impl ProfileLoader for StoreProfileLoader<'_> {
    fn profiles(&mut self) -> Result<(Option<String>, Vec<ProfileEntry>)> {
        self.store.profiles_view()
    }

    fn projects(&mut self, profile: &str) -> Result<Vec<Project>> {
        self.store.profile_projects(profile)
    }

    fn layout(&mut self, profile: &str, path: &str) -> Result<Option<LayoutModel>> {
        self.store.profile_layout(profile, path)
    }

    fn theme(&mut self, profile: &str) -> Result<ThemeOverrides> {
        self.store.profile_theme(profile)
    }

    fn startup(&mut self) -> Result<Option<String>> {
        self.store.startup_profile()
    }

    fn catalogue(&mut self) -> Result<Vec<Project>> {
        self.store.projects()
    }
}

#[cfg(test)]
mod tests {
    use rift_canopy::render::Size;
    use rift_canopy::{AppState, Capabilities, EntryKind, Frame, Project, ProjectStatus};

    use super::*;

    /// The W0 regression class: a **store-backed** launch must render the
    /// identity button from frame one. Every pre-W0 test wired profiles by
    /// calling `set_profiles` directly, so a launch path that skipped the
    /// loader (the headless frame did) — or a store/loader/fold mismatch —
    /// was invisible to the suite. This drives the real seed → real store →
    /// real loader → the shared launch load → the drawn header row.
    #[test]
    fn a_store_backed_launch_renders_the_identity_button_on_frame_one() {
        let dir = tempfile::tempdir().unwrap();
        let store = CanopyStore::open(dir.path()).unwrap();
        store.seed_default_profile(1_000).unwrap();

        let mut state = AppState::new(
            vec![Project {
                name: "proj".into(),
                path: "/r/proj".into(),
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            }],
            Size::new(100, 40),
        );
        state.app.splash.skip();

        let mut loader = StoreProfileLoader::new(&store);
        rift_canopy::load_profile_plane(&mut state, &mut loader).unwrap();
        assert_eq!(
            state.app.active_profile_name(),
            Some("default"),
            "the seeded default profile is active after the launch load"
        );

        let vm = state.snapshot();
        let mut frame = Frame::new(vm.size);
        let mut backend = rift_canopy::AsciiBackend::new(Capabilities::empty());
        rift_canopy::view::draw(&vm, &mut backend, &mut frame);
        let row0: String = (0..frame.width())
            .map(|x| frame.get(x, 0).map_or(' ', |c| c.ch))
            .collect();
        assert!(
            row0.contains("default"),
            "the header row shows the active profile: {row0:?}"
        );
    }
}
