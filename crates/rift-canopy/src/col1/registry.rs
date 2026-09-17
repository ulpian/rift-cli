//! The append-only project registry fold.
//!
//! Each user action on column 1 (add / remove / relabel / reorder) is a new,
//! superseding **event** — never an in-place edit — so the log is immutable by
//! construction. [`current_registry`] replays the log in time order and
//! returns the current [`ProjectEntry`] list: latest-`Added` wins for path/label,
//! a trailing `Removed` hides a project (history kept), an explicit `Reordered`
//! order key overrides registration order, and the result is sorted
//! deterministically. The fold is pure and total — any event sequence yields a
//! well-formed, duplicate-free list.

use std::collections::BTreeMap;

/// Whether a registered entry is a repository root or a linked worktree. Not
/// persisted (there is no `kind` column); every entry defaults to
/// [`EntryKind::Repo`] until worktree detection lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// A repository working tree (the default).
    Repo,
    /// A linked git worktree of a repository.
    Worktree,
}

/// A single mutation to the project registry. Every variant is an **insert** into
/// the append-only log; corrections are new events, so the log is never edited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectOp {
    /// Register a project (its path is the stable natural key; `repo_id` is the
    /// per-repo projection key; `label` is the display name).
    Added {
        /// The project's `repo_id` (the projection scope), if known.
        repo_id: Option<String>,
        /// The canonical project path.
        path: String,
        /// The display label.
        label: String,
    },
    /// Hide a previously-added project (a superseding `removed` event; the log,
    /// and therefore the audit history, is preserved).
    Removed,
    /// Change a project's display label.
    Relabeled {
        /// The new display label.
        label: String,
    },
    /// Set a project's explicit ordering key (overrides registration order).
    Reordered {
        /// The new order key; entries sort ascending by it.
        order_key: i64,
    },
}

/// One row of the append-only `canopy_project_event` log, engine-neutral so the
/// fold is testable without a store. `created_at` is transaction-time epoch-ms;
/// `event_id` is a ULID (lexical time-order) used as the tie-break.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectEvent {
    /// The event's own id (ULID hex).
    pub event_id: String,
    /// The project this event concerns (stable across its whole history).
    pub project_id: String,
    /// What changed.
    pub op: ProjectOp,
    /// Transaction-time (epoch-ms).
    pub created_at: i64,
}

/// The current state of one registered project — the fold's output row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectEntry {
    /// The project's stable id (ULID hex).
    pub project_id: String,
    /// The per-repo projection scope, if known.
    pub repo_id: Option<String>,
    /// The canonical project path.
    pub path: String,
    /// The display label.
    pub label: String,
    /// Repo vs linked worktree (defaults to [`EntryKind::Repo`], never persisted).
    pub kind: EntryKind,
    /// The effective sort key (explicit reorder, else registration time).
    pub order_key: i64,
}

/// The mutable per-project accumulator used while replaying the log.
#[derive(Default)]
struct Acc {
    present: bool,
    repo_id: Option<String>,
    path: String,
    label: String,
    /// Registration time (the first `Added`'s `created_at`) — the default order.
    added_at: Option<i64>,
    /// An explicit reorder key, if any (overrides `added_at`).
    explicit_order: Option<i64>,
}

/// Replay `events` and return the current registry: the present projects, sorted
/// by `(order_key, registration-time, project_id)`. Pure and total — later events
/// supersede earlier ones, a `Removed` hides a project, and an entry never
/// duplicates (grouped by `project_id`). A project with no `Added` is skipped
/// (a relabel/reorder without a registration is meaningless).
#[must_use]
pub fn current_registry(events: &[ProjectEvent]) -> Vec<ProjectEntry> {
    // Replay in a deterministic order: transaction-time, then the ULID event id as
    // the stable tie-break (so a caller's row order can never change the fold).
    let mut ordered: Vec<&ProjectEvent> = events.iter().collect();
    ordered.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.event_id.cmp(&b.event_id))
    });

    let mut acc: BTreeMap<&str, Acc> = BTreeMap::new();
    for ev in ordered {
        let a = acc.entry(ev.project_id.as_str()).or_default();
        match &ev.op {
            ProjectOp::Added {
                repo_id,
                path,
                label,
            } => {
                a.present = true;
                a.repo_id.clone_from(repo_id);
                a.path.clone_from(path);
                a.label.clone_from(label);
                a.added_at.get_or_insert(ev.created_at);
            }
            // A relabel/reorder before any add is meaningless; ignore until added.
            ProjectOp::Relabeled { label } => {
                if a.added_at.is_some() {
                    a.label.clone_from(label);
                }
            }
            ProjectOp::Reordered { order_key } => {
                if a.added_at.is_some() {
                    a.explicit_order = Some(*order_key);
                }
            }
            ProjectOp::Removed => a.present = false,
        }
    }

    let mut entries: Vec<ProjectEntry> = acc
        .into_iter()
        .filter_map(|(id, a)| {
            let added_at = a.added_at?;
            if !a.present {
                return None;
            }
            Some(ProjectEntry {
                project_id: id.to_string(),
                repo_id: a.repo_id,
                path: a.path,
                label: a.label,
                kind: EntryKind::Repo,
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

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn added(id: &str, path: &str, label: &str, at: i64) -> ProjectEvent {
        ProjectEvent {
            event_id: format!("e{at:04}"),
            project_id: id.into(),
            op: ProjectOp::Added {
                repo_id: Some(format!("repo-{id}")),
                path: path.into(),
                label: label.into(),
            },
            created_at: at,
        }
    }

    fn ev(id: &str, op: ProjectOp, at: i64) -> ProjectEvent {
        ProjectEvent {
            event_id: format!("e{at:04}"),
            project_id: id.into(),
            op,
            created_at: at,
        }
    }

    #[test]
    fn empty_log_is_empty_registry() {
        assert!(current_registry(&[]).is_empty());
    }

    #[test]
    fn add_then_registry_lists_it() {
        let reg = current_registry(&[added("p1", "/r/a", "alpha", 1)]);
        assert_eq!(reg.len(), 1);
        assert_eq!(reg[0].path, "/r/a");
        assert_eq!(reg[0].label, "alpha");
        assert_eq!(reg[0].kind, EntryKind::Repo);
    }

    #[test]
    fn remove_hides_but_keeps_history() {
        let log = [
            added("p1", "/r/a", "alpha", 1),
            ev("p1", ProjectOp::Removed, 2),
        ];
        assert!(current_registry(&log).is_empty(), "removed → hidden");
    }

    #[test]
    fn re_add_after_remove_restores() {
        let log = [
            added("p1", "/r/a", "alpha", 1),
            ev("p1", ProjectOp::Removed, 2),
            added("p1", "/r/a", "alpha2", 3),
        ];
        let reg = current_registry(&log);
        assert_eq!(reg.len(), 1);
        assert_eq!(reg[0].label, "alpha2", "latest add wins");
    }

    #[test]
    fn relabel_takes_latest() {
        let log = [
            added("p1", "/r/a", "alpha", 1),
            ev("p1", ProjectOp::Relabeled { label: "A!".into() }, 2),
        ];
        assert_eq!(current_registry(&log)[0].label, "A!");
    }

    #[test]
    fn reorder_overrides_registration_order() {
        // p1 registered first, p2 second → default order p1, p2.
        let log = [added("p1", "/r/a", "a", 1), added("p2", "/r/b", "b", 2)];
        let reg = current_registry(&log);
        assert_eq!(reg[0].project_id, "p1");

        // Swap their order keys → p2 sorts first.
        let mut swapped = log.to_vec();
        swapped.push(ev("p1", ProjectOp::Reordered { order_key: 2 }, 3));
        swapped.push(ev("p2", ProjectOp::Reordered { order_key: 1 }, 4));
        let reg = current_registry(&swapped);
        assert_eq!(reg[0].project_id, "p2");
        assert_eq!(reg[1].project_id, "p1");
    }

    #[test]
    fn relabel_without_add_is_ignored() {
        let log = [ev("ghost", ProjectOp::Relabeled { label: "x".into() }, 1)];
        assert!(current_registry(&log).is_empty());
    }

    #[test]
    fn fold_is_order_independent() {
        // The same events in any input order fold to the same registry (the fold
        // sorts by created_at then event_id internally).
        let forward = [
            added("p1", "/r/a", "a", 1),
            added("p2", "/r/b", "b", 2),
            ev("p1", ProjectOp::Relabeled { label: "A".into() }, 3),
        ];
        let mut reversed = forward.to_vec();
        reversed.reverse();
        assert_eq!(current_registry(&forward), current_registry(&reversed));
    }

    // --- property tests -----------------------------------------------------

    fn op_strategy() -> impl Strategy<Value = ProjectOp> {
        prop_oneof![
            ("/r/[a-z]{1,3}", "[a-z]{1,4}").prop_map(|(path, label)| ProjectOp::Added {
                repo_id: Some("r".into()),
                path,
                label,
            }),
            Just(ProjectOp::Removed),
            "[a-z]{1,4}".prop_map(|label| ProjectOp::Relabeled { label }),
            (0i64..8).prop_map(|order_key| ProjectOp::Reordered { order_key }),
        ]
    }

    proptest! {
        /// The fold is total (never panics) and produces a duplicate-free,
        /// path-consistent registry for any event sequence.
        #[test]
        fn current_registry_is_fold_latest_valid(
            raw in proptest::collection::vec((0usize..3, op_strategy(), 1i64..50), 0..40)
        ) {
            let events: Vec<ProjectEvent> = raw
                .into_iter()
                .enumerate()
                .map(|(i, (pid, op, at))| ProjectEvent {
                    event_id: format!("e{i:04}"),
                    project_id: format!("p{pid}"),
                    op,
                    created_at: at,
                })
                .collect();
            let reg = current_registry(&events);

            // No two entries share a project_id (grouped) or a path (each present
            // project has exactly one current row).
            let mut ids: Vec<&String> = reg.iter().map(|e| &e.project_id).collect();
            let n = ids.len();
            ids.sort();
            ids.dedup();
            prop_assert_eq!(ids.len(), n, "project_ids are unique");

            // The result is sorted by order_key then project_id.
            for w in reg.windows(2) {
                let (a, b) = (&w[0], &w[1]);
                prop_assert!(
                    (a.order_key, &a.project_id) <= (b.order_key, &b.project_id),
                    "registry is deterministically sorted"
                );
            }
        }

        /// Idempotence-ish: appending a `Removed` for every present project
        /// empties the registry (removal always hides).
        #[test]
        fn removing_everything_empties_the_registry(
            paths in proptest::collection::vec("/r/[a-z]{1,3}", 1..6)
        ) {
            let mut events = Vec::new();
            for (i, path) in paths.iter().enumerate() {
                let at = i64::try_from(i).unwrap() + 1;
                events.push(added(&format!("p{i}"), path, "l", at));
            }
            let present = current_registry(&events);
            let base = i64::try_from(events.len()).unwrap();
            for (i, entry) in present.iter().enumerate() {
                let at = base + i64::try_from(i).unwrap() + 1;
                events.push(ev(&entry.project_id, ProjectOp::Removed, at));
            }
            prop_assert!(current_registry(&events).is_empty());
        }
    }
}
