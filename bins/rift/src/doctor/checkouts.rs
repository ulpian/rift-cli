//! Registry rows of this family that name a checkout which is gone.
//!
//! The machine-global project registry keeps one row per family, keyed by
//! the family id the row was registered under. When the checkout a row names
//! is deleted, the dashboard keeps listing a project with nothing behind it.
//! The report names each such row with its path; `--fix` removes the row from
//! this machine's index and touches nothing else: never the family identity,
//! never a canonical row, never another family's row.
//!
//! A row counts as obsolete only when its absence is proven: the path is gone
//! and its parent directory is there to ask. A checkout on an unmounted
//! volume has no parent to read either, so it is kept and not reported.

use std::path::Path;

use rift_git::{FamilyState, MintPolicy};
use rift_registry::{ProjectRegistry, RegisteredProject};

/// The obsolete-checkout leg over this machine's registry.
pub(super) fn doctor_obsolete_checkouts(repo: &Path, fix: bool) -> (String, serde_json::Value) {
    obsolete_checkouts(repo, &rift_registry::default_registry_path(), fix)
}

/// The leg over an explicit registry file, so a fixture never reads the
/// machine's own registry.
fn obsolete_checkouts(repo: &Path, registry_path: &Path, fix: bool) -> (String, serde_json::Value) {
    let unreadable = || (String::new(), serde_json::json!({ "readable": false }));
    let Ok(paths) = rift_git::discover(repo) else {
        return unreadable();
    };
    // Never mints: a family with no identity yet owns no registry row.
    let Ok(FamilyState::Minted(family)) = rift_git::resolve_family(&paths, MintPolicy::Never)
    else {
        return unreadable();
    };
    let Ok(mut registry) = ProjectRegistry::load(registry_path) else {
        return unreadable();
    };
    let family_id = family.family_id.as_str();
    let obsolete = |row: &RegisteredProject| {
        row.family_id.as_deref() == Some(family_id) && proven_absent(&row.path)
    };
    let found: Vec<RegisteredProject> = registry
        .projects()
        .iter()
        .filter(|row| obsolete(row))
        .cloned()
        .collect();
    if found.is_empty() {
        return (
            String::new(),
            serde_json::json!({ "obsolete": 0, "removed": 0 }),
        );
    }
    let removed = if fix {
        let dropped = registry.remove_where(obsolete);
        registry.save(registry_path).map_or(0, |()| dropped.len())
    } else {
        0
    };
    render(&found, fix, removed)
}

/// Gone, and provably so: the parent directory exists and the path does not.
fn proven_absent(path: &Path) -> bool {
    !path.exists() && path.parent().is_some_and(Path::is_dir)
}

/// The leg's lines and object.
fn render(found: &[RegisteredProject], fix: bool, removed: usize) -> (String, serde_json::Value) {
    let mut human = format!(
        "\n  obsolete checkouts: {} registry row(s) of this family name a checkout that is gone",
        found.len()
    );
    for row in found {
        human.push_str(&format!(
            "\n    {} \u{2192} {}",
            row.name,
            row.path.display()
        ));
    }
    if fix {
        human.push_str(&format!(
            "\n    removed {removed} row(s) from this machine's project registry"
        ));
    } else {
        human.push_str("\n    `rift doctor --fix` removes these rows; the family identity is kept");
    }
    let rows: Vec<serde_json::Value> = found
        .iter()
        .map(|row| serde_json::json!({ "name": row.name, "path": row.path.display().to_string() }))
        .collect();
    (
        human,
        serde_json::json!({ "obsolete": found.len(), "removed": removed, "found": rows }),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// A minted fixture repository at `root` and its family id.
    fn minted_repo(root: &Path) -> String {
        std::fs::create_dir_all(root).unwrap();
        rift_git::test_support::fake_repo(root).unwrap();
        let paths = rift_git::discover(root).unwrap();
        match rift_git::resolve_family(&paths, MintPolicy::Allow).unwrap() {
            FamilyState::Minted(family) => family.family_id.as_str().to_owned(),
            other => panic!("expected a mint, got {other:?}"),
        }
    }

    /// Only this family's row for a provably gone checkout is reported; a
    /// report changes nothing, `--fix` removes that row alone, and the family
    /// identity reads the same afterwards.
    #[test]
    fn only_this_familys_provably_gone_checkout_is_named_and_fix_removes_that_row_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        let family_id = minted_repo(&main);
        let registry_path = tmp.path().join("config/rift/projects.toml");
        let mut registry = ProjectRegistry::default();
        // A family holds one seat, so this family's row is the gone one; the
        // other two rows are gone too and are not this family's to remove.
        let rows = [
            ("gone", tmp.path().join("deleted"), Some(family_id.as_str())),
            (
                "other",
                tmp.path().join("elsewhere"),
                Some("another-family"),
            ),
            ("legacy", tmp.path().join("old"), None),
        ];
        assert!(!proven_absent(&main), "a live checkout is never obsolete");
        assert!(
            !proven_absent(&PathBuf::from("/no-such-volume/repo")),
            "a path with no parent to read is unproven, not gone"
        );
        for (name, path, family) in rows {
            registry.add_with_family(name, path, family);
        }
        registry.save(&registry_path).unwrap();
        let before = std::fs::read(&registry_path).unwrap();

        let (human, json) = obsolete_checkouts(&main, &registry_path, false);
        assert_eq!(json["obsolete"], 1, "{human}");
        assert_eq!(json["found"][0]["name"], "gone");
        assert!(human.contains("--fix"), "{human}");
        assert_eq!(
            std::fs::read(&registry_path).unwrap(),
            before,
            "a report writes nothing"
        );

        let (_, json) = obsolete_checkouts(&main, &registry_path, true);
        assert_eq!(json["removed"], 1);
        let names: Vec<String> = ProjectRegistry::load(&registry_path)
            .unwrap()
            .projects()
            .iter()
            .map(|row| row.name.clone())
            .collect();
        assert_eq!(names, ["other", "legacy"]);
        let paths = rift_git::discover(&main).unwrap();
        match rift_git::resolve_family(&paths, MintPolicy::Never).unwrap() {
            FamilyState::Minted(family) => assert_eq!(family.family_id.as_str(), family_id),
            other => panic!("the family identity is kept, got {other:?}"),
        }
        let (human, _) = obsolete_checkouts(&main, &registry_path, false);
        assert_eq!(human, "");
    }
}
