//! The setup plan a wizard leaves beside a checkout while setup runs.
//!
//! The plan is a choice in flight: the selections a retried setup is
//! pre-filled with. It is never a fact about whether the project is set up.
//! That answer is the family's one predicate (its projection under the cache
//! root and a live binary of record), so a plan left behind by a crash, a
//! failed remove or a copied checkout cannot contradict it. The card, the
//! settings page and the doctor all read the plan through this module, so the
//! rule is written once.

use std::path::Path;

use rift_store::projection_db_for_family_under;

use crate::feeds::project_family;

use super::settings_view::hooks_installed_here;

/// Where the plan lives, relative to the checkout it was written for.
const SETUP_PLAN: &str = ".rift/state/project-setup.json";

/// What the setup plan beside a checkout means for its status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupPlanMarker {
    /// No plan is left beside the checkout.
    Absent,
    /// A plan is there and the family is not set up here: setup is in
    /// flight or stopped part way, and the plan pre-fills its retry.
    Pending,
    /// A plan is there although the family is set up here: a leftover that
    /// decides nothing.
    Stale,
}

/// The setup plan beside `dir`, judged against the family's predicate as
/// the cache root `root` holds it.
#[must_use]
pub fn setup_plan_marker_under(root: &Path, dir: &Path) -> SetupPlanMarker {
    marker_for(root, dir, project_family(dir).as_ref())
}

/// Whether the family is set up on this machine: its projection exists under
/// `root` and its hook install is on record with a binary still there.
fn set_up_here(root: &Path, family: &rift_git::Family) -> bool {
    projection_db_for_family_under(root, family).exists() && hooks_installed_here(family)
}

/// Whether the plan beside `dir` makes the status `SetupIncomplete`: only
/// when the family predicate fails.
pub(super) fn setup_left_pending(
    root: &Path,
    dir: &Path,
    family: Option<&rift_git::Family>,
) -> bool {
    marker_for(root, dir, family) == SetupPlanMarker::Pending
}

/// The pending plan's selections, for the wizard's retry. `None` when there
/// is no plan or it is not a schema this build reads.
pub(super) fn pending_setup_plan(dir: &Path) -> Option<serde_json::Value> {
    let bytes = std::fs::read(dir.join(SETUP_PLAN)).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    (value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        == Some(1))
    .then_some(value)
}

/// The judgement itself. The plan is stat'ed first only because it is the
/// cheaper read and absent on almost every call; when it is there the
/// predicate decides, so the plan never outranks it.
fn marker_for(root: &Path, dir: &Path, family: Option<&rift_git::Family>) -> SetupPlanMarker {
    if !dir.join(SETUP_PLAN).is_file() {
        return SetupPlanMarker::Absent;
    }
    if family.is_some_and(|family| set_up_here(root, family)) {
        SetupPlanMarker::Stale
    } else {
        SetupPlanMarker::Pending
    }
}

#[cfg(test)]
mod tests {
    use rift_canopy::{ProjectInitialization, ProjectStatus};

    use super::super::fixtures::record_hooks_installed;
    use super::super::project_read::read_project;
    use super::super::settings_view::project_settings_view_under;
    use super::*;

    fn write_plan(repo: &Path) {
        std::fs::create_dir_all(repo.join(".rift/state")).unwrap();
        std::fs::write(
            repo.join(SETUP_PLAN),
            r#"{"schema_version": 1, "total_recall": false, "integrations": []}"#,
        )
        .unwrap();
    }

    fn provision(root: &Path, repo: &Path) {
        let family = project_family(repo).unwrap();
        let db = projection_db_for_family_under(root, &family);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, b"").unwrap();
        record_hooks_installed(repo);
    }

    #[test]
    fn a_setup_plan_left_beside_a_set_up_family_decides_nothing() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(repo.path()).unwrap();
        provision(root.path(), repo.path());
        let path = repo.path().to_string_lossy();
        let without_marker = read_project(root.path(), &path).status;

        // Setup finished but its plan was never removed: a crash before the
        // remove, a failed remove, or a copied checkout.
        write_plan(repo.path());
        assert_eq!(read_project(root.path(), &path).status, without_marker);
        assert_ne!(without_marker, ProjectStatus::SetupIncomplete);
        let view = project_settings_view_under(root.path(), repo.path());
        assert_eq!(view.initialization, ProjectInitialization::Initialized);
        assert!(
            view.settings[0].enabled,
            "a stale plan does not pre-fill a page that offers no setup"
        );
        assert_eq!(
            setup_plan_marker_under(root.path(), repo.path()),
            SetupPlanMarker::Stale
        );
    }

    #[test]
    fn a_setup_plan_beside_an_unprovisioned_family_is_setup_incomplete_and_prefills() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(repo.path()).unwrap();
        write_plan(repo.path());
        assert_eq!(
            read_project(root.path(), &repo.path().to_string_lossy()).status,
            ProjectStatus::SetupIncomplete
        );
        let view = project_settings_view_under(root.path(), repo.path());
        assert_eq!(view.initialization, ProjectInitialization::SetupIncomplete);
        assert!(!view.settings[0].enabled, "the pending plan pre-fills");
        assert_eq!(
            setup_plan_marker_under(root.path(), repo.path()),
            SetupPlanMarker::Pending
        );
    }

    #[test]
    fn no_setup_plan_beside_a_config_only_clone_reads_not_provisioned() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(repo.path()).unwrap();
        std::fs::create_dir_all(repo.path().join(".rift")).unwrap();
        assert_eq!(
            read_project(root.path(), &repo.path().to_string_lossy()).status,
            ProjectStatus::NotProvisioned
        );
        assert_eq!(
            setup_plan_marker_under(root.path(), repo.path()),
            SetupPlanMarker::Absent
        );
    }
}
