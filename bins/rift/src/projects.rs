//! `rift projects list|prune`: the registry CLI over the append-only canopy
//! UI-state store.
//!
//! Separate from the dashboard launcher because it is a read of the same
//! store with no terminal attached, and the palette's `/projects` verbs
//! reach it through the same two functions.

use std::path::Path;

use rift_canopy::{PersistOp, Project};

use crate::cli::{self, Outcome};
use crate::{hooks, worktree};

/// `rift projects list|prune`: the registry CLI over the append-only
/// canopy UI-state store. `list` shows every registered row with its presence
/// (the same path-missing fact behind the dashboard's `!` glyph); `prune`
/// appends one superseding removed event per path-missing row. Registered rows
/// only — derived worktree children are never registry rows.
pub(crate) fn projects_cmd(cmd: &cli::ProjectsCmd, root: &Path) -> rift_core::Result<Outcome> {
    // The cache sweep is a different noun on the same verb: it removes
    // per-path CACHE directories the family mint orphaned, never a registry
    // row, so it answers before the registry is even opened.
    if let cli::ProjectsCmd::Prune(a) = cmd {
        if a.orphan_projections {
            return worktree::prune_orphan_projections(root, a.yes && !a.dry_run);
        }
    }
    let cache_root = rift_store::resolve_cache_root(|k| std::env::var(k).ok());
    let strictly_read_only = matches!(cmd, cli::ProjectsCmd::List(_))
        || matches!(cmd, cli::ProjectsCmd::Prune(a) if a.dry_run);
    let cstore = if strictly_read_only {
        rift_canopy_data::CanopyStore::open_read_only(&cache_root)?
    } else {
        Some(rift_canopy_data::CanopyStore::open(&cache_root)?)
    };
    let all: Vec<Project> = cstore
        .as_ref()
        .map_or(Ok(Vec::new()), rift_canopy_data::CanopyStore::projects)?;
    match cmd {
        cli::ProjectsCmd::List(a) => {
            let (human, json_rows) = projects_list_rows(&all, a.worktrees);
            Ok(Outcome::ok(
                human.trim_end().to_owned(),
                serde_json::json!({ "projects": json_rows }),
            ))
        }
        cli::ProjectsCmd::Prune(a) => projects_prune(cstore.as_ref(), &all, a.dry_run),
    }
}

/// `rift projects prune`: one superseding removed event per registered row
/// whose path is gone, or the same list said and not written on a dry run.
///
/// Registered rows only: a derived worktree child has no row of its own to
/// supersede — members are derived, never registered — and sweeping one would
/// be sweeping its repository.
fn projects_prune(
    cstore: Option<&rift_canopy_data::CanopyStore>,
    all: &[Project],
    dry_run: bool,
) -> rift_core::Result<Outcome> {
    let rows: Vec<&Project> = all
        .iter()
        .filter(|p| p.kind == rift_canopy::EntryKind::Repo)
        .collect();
    let victims: Vec<&Project> = rows
        .iter()
        .copied()
        .filter(|p| !Path::new(&p.path).is_dir())
        .collect();
    let mut human = String::new();
    for v in &victims {
        human.push_str(&format!(
            "{} {}  ({})\n",
            if dry_run { "would remove" } else { "removed" },
            v.path,
            v.name
        ));
    }
    if !dry_run {
        let Some(cstore) = cstore else {
            return Err(rift_core::RiftError::storage(
                "Canopy project store unavailable",
            ));
        };
        for v in &victims {
            cstore.apply(
                &PersistOp::RemoveProject {
                    path: v.path.clone(),
                },
                hooks::now_ms(),
            )?;
        }
    }
    human.push_str(&format!(
        "{} {} of {} registered rows{}",
        if dry_run { "would prune" } else { "pruned" },
        victims.len(),
        rows.len(),
        if dry_run {
            " (dry run — nothing written)"
        } else {
            ""
        }
    ));
    let json = serde_json::json!({
        "dry_run": dry_run,
        "victims": victims.iter().map(|v| v.path.clone()).collect::<Vec<_>>(),
        "registered": rows.len(),
    });
    Ok(Outcome::ok(human, json))
}

/// The rows `rift projects list` prints, human and JSON.
///
/// `worktrees` adds the family's **members** under their repository, indented;
/// the default output is unchanged. The axis exists because
/// members are no longer registrable: before that rule a member could be
/// listed by registering it, which is the duplicate project the rule removes,
/// so listing them is now something the surface has to offer on purpose.
///
/// The order is the store's — `CanopyStore::projects` emits a repository
/// immediately followed by its derived children — so "under their root" is a
/// property this function preserves rather than one it invents.
fn projects_list_rows(all: &[Project], worktrees: bool) -> (String, Vec<serde_json::Value>) {
    let mut human = String::new();
    let mut json_rows = Vec::new();
    let mut registered = 0_usize;
    for p in all {
        let member = p.kind == rift_canopy::EntryKind::Worktree;
        if member {
            if !worktrees {
                continue;
            }
        } else {
            registered += 1;
        }
        let ok = Path::new(&p.path).is_dir();
        human.push_str(&format!(
            "{} {}{}  ({})\n",
            if ok { " " } else { "!" },
            if member { "  " } else { "" },
            p.path,
            p.name
        ));
        json_rows.push(serde_json::json!({
            "path": p.path,
            "label": p.name,
            "present": ok,
            "kind": if member { "worktree" } else { "repo" },
        }));
    }
    if registered == 0 {
        human.push_str("no projects registered — run `rift init` in a repository\n");
    }
    (human, json_rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★ The members of a family are listed **under** their repository, and
    /// only when asked for.
    ///
    /// This is the surface that replaces registering a worktree. Before the
    /// one-row-per-family rule a member could be listed by registering it —
    /// the duplicate project the rule removes — so the listing has to offer
    /// them on purpose, as children of the repository they belong to.
    ///
    /// Driven through the real store and a real family fixture, because "under
    /// their root" is a claim about ORDER that a hand-built row list would
    /// assume rather than test: the store emits a repository immediately
    /// followed by its derived children, and this renderer's job is to preserve
    /// that and indent them.
    #[test]
    fn projects_list_worktrees_shows_members_under_their_root() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git_dir = rift_git::test_support::fake_repo(&repo).unwrap();
        let member = tmp.path().join("feature");
        rift_git::test_support::fake_worktree(&git_dir, &member, "feature").unwrap();
        let repo = std::fs::canonicalize(&repo).unwrap();
        let member = std::fs::canonicalize(&member).unwrap();

        let store = rift_canopy_data::CanopyStore::open(&tmp.path().join("cache")).unwrap();
        store
            .apply(
                &PersistOp::AddProject {
                    path: repo.to_string_lossy().into_owned(),
                },
                1,
            )
            .unwrap();
        let all = store.projects().unwrap();

        // The default output is unchanged: registered rows only.
        let (human, rows) = projects_list_rows(&all, false);
        assert_eq!(rows.len(), 1, "one registered project: {rows:?}");
        assert!(
            !human.contains("feature"),
            "the member is not a row by default: {human}"
        );

        let (human, rows) = projects_list_rows(&all, true);
        let lines: Vec<&str> = human.lines().collect();
        assert_eq!(lines.len(), 2, "the repository and its member: {human}");
        assert!(
            lines[0].contains(&repo.display().to_string()),
            "the repository first: {human}"
        );
        assert!(
            lines[1].contains(&member.display().to_string()),
            "then the member it holds: {human}"
        );
        assert!(
            lines[1].starts_with("   ") && !lines[0].starts_with("   "),
            "and the member is indented UNDER it: {human}"
        );
        assert_eq!(rows[0]["kind"], "repo");
        assert_eq!(rows[1]["kind"], "worktree");
        assert_eq!(rows[1]["path"], member.display().to_string());
    }
}
