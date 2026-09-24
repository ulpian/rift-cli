//! The seeders the reducer's test modules share: a dashboard past the splash
//! and one with a live profile plane.
//!
//! One file so the menu and the picker tests start from the same profiles, and
//! so a seeder is built one way.

use crate::profile::ProfileEntry;
use crate::{App, EntryKind, Project, ProjectStatus};

pub(super) fn project(name: &str) -> Project {
    Project {
        name: name.into(),
        path: format!("/r/{name}"),
        status: ProjectStatus::Idle,
        kind: EntryKind::Repo,
    }
}

/// An app past the splash, ready for Normal-mode interaction (the first-run
/// wizard, which an empty registry raises, is dismissed so keys reach the
/// dashboard — overlay-key tests open their own overlay).
pub(super) fn ready(projects: Vec<Project>) -> App {
    let mut app = App::new(projects);
    app.splash.skip();
    app.overlay = None;
    app
}

/// An app with three live profiles (active first) past the splash.
pub(super) fn with_profiles() -> App {
    let mut app = ready(vec![project("a")]);
    app.profiles = ["default", "work", "lab"]
        .iter()
        .enumerate()
        .map(|(i, name)| ProfileEntry {
            profile_id: format!("pf{i}"),
            name: (*name).to_string(),
            created_at: i64::try_from(i).unwrap(),
        })
        .collect();
    app.active_profile = "pf0".into();
    app
}
