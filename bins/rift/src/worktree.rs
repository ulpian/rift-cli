//! `rift worktree list|status|adopt|prune` — the family's members as a
//! lifecycle surface, plus the orphan-projection enumeration
//! `rift doctor` and `rift projects prune --orphan-projections` share.
//!
//! The operator's vocabulary grows by one noun, not a new workflow: `worktree`
//! sits beside `session`, `projects` and `doctor` and answers the same three
//! questions they do — what is there, what is the state of one of them, and
//! what can be swept.
//!
//! # Everything here reports; almost nothing writes
//!
//! * `list` and `status` are pure reads. `status` reaches into the family's
//!   projection **read-only** and into the client wiring through
//!   `rift-agent-install`'s verify-only probes, which is why it can name
//!   Codex's per-path hook trust without ever writing Codex's configuration
//!   (the trust file is the user's to approve, in Codex).
//! * `adopt` is [`crate::adopt`]'s, unchanged — this module only routes to it.
//! * `prune` **reports** members whose checkout is gone. `--yes` runs
//!   `git worktree prune` through the argv-safe seam (an argv array, explicit
//!   `GIT_DIR`/`GIT_WORK_TREE`, no shell — `rift_git::run_git`). Without
//!   `--yes` nothing is executed at all: the decision to remove an
//!   administrative entry is the operator's, and a verb that pruned on being
//!   asked what is prunable would be answering a question with an act.
//!
//! # The orphan projections
//!
//! Until the family mint the projection was content-addressed over the
//! *working tree* path, so one repository in seven checkouts had seven
//! databases. [`orphan_projections`] derives that retired id again — the one
//! place the legacy per-path derivation is sanctioned in a product path,
//! because a diagnosis of drift has to be able to SEE the old rule — and both
//! readers of it, the doctor leg and `rift projects prune
//! --orphan-projections`, share this one enumeration so they can never
//! disagree about what is an orphan. The family's own directory is excluded
//! by identity, not by name.

use std::path::{Path, PathBuf};

use rift_core::Result;

use crate::cli::{exit, Outcome, WorktreeCmd};

/// The file whose presence means "this member has been provisioned": the
/// native-integration manifest `rift enable`/adoption writes. The same oracle
/// the dashboard's `NotAdopted` rule uses, named once.
const PROVISIONED_MARKER: &str = ".rift/state/native-integrations.json";

/// `rift worktree …` — the group's dispatcher.
///
/// # Errors
/// Propagates repository discovery and projection read failures. A member that
/// cannot be adopted is a refusal [`Outcome`], not an `Err`.
pub(crate) fn run(cmd: &WorktreeCmd, root: &Path) -> Result<Outcome> {
    match cmd {
        WorktreeCmd::Adopt(args) => crate::adopt::run(args, root),
        WorktreeCmd::List => list(root),
        WorktreeCmd::Status(args) => status(root, args.path.as_deref()),
        WorktreeCmd::Prune(args) => prune(root, args.yes),
    }
}

/// One member's row, as both surfaces report it.
struct Row {
    member: rift_git::FamilyMember,
    branch: Option<String>,
    provisioned: bool,
    /// Why the last adoption stopped, when one did (`adoption.json`).
    deferred: Option<String>,
    live_sessions: usize,
    /// The newest recorded capture instant among the checkpoints this member
    /// archived; `None` when it has archived none the projection can date.
    last_capture_ms: Option<i64>,
}

impl Row {
    fn kind(&self) -> &'static str {
        if self.member.is_main {
            "main"
        } else {
            "linked"
        }
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.member.name,
            "path": self.member.work_tree.display().to_string(),
            "git_dir": self.member.git_dir.display().to_string(),
            "kind": self.kind(),
            "branch": self.branch,
            "provisioned": self.provisioned,
            "adoption_deferred": self.deferred,
            "live_sessions": self.live_sessions,
            "last_capture_ms": self.last_capture_ms,
            "prunable": self.member.prunable,
        })
    }

    fn human(&self) -> String {
        format!(
            "  {} {}  {}  branch={}  {}  live={}  last-capture={}{}\n",
            if self.member.prunable { "!" } else { " " },
            crate::provenance::Label::of(&self.member.name),
            self.kind(),
            self.branch.as_ref().map_or_else(
                || "(detached)".to_owned(),
                |b| crate::provenance::Label::of(b).to_string()
            ),
            if self.provisioned {
                "provisioned"
            } else {
                "not adopted"
            },
            self.live_sessions,
            self.last_capture_ms
                .map_or_else(|| "never".to_owned(), |ms| ms.to_string()),
            if self.member.prunable {
                "  (checkout gone — `rift worktree prune --yes`)"
            } else {
                ""
            },
        )
    }
}

/// Build every member's row for the family `root` belongs to.
fn rows(root: &Path) -> Result<(rift_git::RepoPaths, Vec<Row>)> {
    let paths = rift_git::discover(root)?;
    let members = rift_git::family_members(&paths);
    // ONE read of the family projection for every member's last capture: a
    // per-member query would open the same database once per checkout, and
    // this verb is read-only anyway.
    let captures = last_captures(root);
    let mut out = Vec::with_capacity(members.len());
    for member in members {
        // A prunable member has no checkout to read, so every filesystem
        // question about it answers absence rather than guessing.
        let live_sessions = if member.prunable {
            0
        } else {
            rift_shadow::hooks::session_cache::live_sessions_for(
                &member.git_dir,
                &paths.common_dir,
                &member.work_tree,
            )
            .map_or(0, |v| v.len())
        };
        let token = member_token(&member);
        out.push(Row {
            branch: member_branch(&member),
            provisioned: !member.prunable && member.work_tree.join(PROVISIONED_MARKER).is_file(),
            deferred: crate::adopt::marker_reason(&member.work_tree),
            live_sessions,
            last_capture_ms: token.and_then(|t| captures.get(&t).copied()),
            member,
        });
    }
    Ok((paths, out))
}

/// One member's worktree token, or `None` when its gitdir cannot be resolved
/// (a prunable member, or one whose administrative entry is unreadable).
fn member_token(member: &rift_git::FamilyMember) -> Option<String> {
    rift_git::discover(&member.work_tree)
        .ok()
        .map(|p| p.worktree_token())
}

/// `HEAD`'s branch in one member, read without running git.
fn member_branch(member: &rift_git::FamilyMember) -> Option<String> {
    rift_git::head_state(&rift_git::RepoPaths {
        git_dir: member.git_dir.clone(),
        common_dir: member.git_dir.clone(),
        work_tree: member.work_tree.clone(),
        worktree_hash: [0; 6],
    })
    .branch
}

/// The newest recorded capture instant per worktree token, from
/// `checkpoint_worktree` joined to `checkpoint`.
///
/// Total over a projection that has neither the relation nor a file: an
/// unreadable store is an empty map and every member reports `never`, which is
/// the same answer a member that has archived nothing gives. The unknown-time
/// sentinel is filtered here rather than rendered, on
/// `provenance::checkpoint_facts`'s rule — `-1` is "no recorded time", not an
/// instant in 1969.
fn last_captures(root: &Path) -> std::collections::BTreeMap<String, i64> {
    use rift_store::CanonicalStore as _;
    let mut out = std::collections::BTreeMap::new();
    let (Ok(db), Ok(rt)) = (crate::family::projection_db(root), crate::runtime()) else {
        return out;
    };
    let Ok(store) = rift_store::SqliteStore::open_read_only(&db) else {
        return out;
    };
    let Ok(rows) = rt.block_on(store.query(
        "SELECT w.worktree_token, MAX(c.created_at) \
         FROM checkpoint_worktree w JOIN checkpoint c ON c.checkpoint_id = w.checkpoint_id \
         GROUP BY w.worktree_token",
        &[],
    )) else {
        return out;
    };
    for r in &rows {
        if let (Ok(token), Ok(at)) = (r.get_text(0), r.get_i64(1)) {
            if at != rift_shadow::CREATED_AT_UNKNOWN {
                out.insert(token, at);
            }
        }
    }
    out
}

/// `rift worktree list` — every member with its state.
fn list(root: &Path) -> Result<Outcome> {
    let (paths, rows) = rows(root)?;
    let mut human = format!(
        "worktrees of the family at {}:\n",
        crate::provenance::safe_path(&paths.common_dir.display().to_string())
    );
    if rows.is_empty() {
        human.push_str("  (none — this repository has no resolvable members)\n");
    }
    for r in &rows {
        human.push_str(&r.human());
    }
    let prunable = rows.iter().filter(|r| r.member.prunable).count();
    let unadopted = rows
        .iter()
        .filter(|r| !r.member.prunable && !r.provisioned && !r.member.is_main)
        .count();
    human.push_str(&format!(
        "  {} member(s); {prunable} prunable; {unadopted} linked member(s) not adopted",
        rows.len()
    ));
    Ok(Outcome::ok(
        human,
        serde_json::json!({
            "common_dir": paths.common_dir.display().to_string(),
            "members": rows.iter().map(Row::json).collect::<Vec<_>>(),
            "prunable": prunable,
            "not_adopted": unadopted,
        }),
    ))
}

/// `rift worktree status [PATH]` — one member in full: its row, the client
/// wiring `rift-agent-install` can verify without writing anything, and the
/// worktree observations recorded in it (the observation history, from this
/// member's side).
fn status(root: &Path, path: Option<&Path>) -> Result<Outcome> {
    let start = path.unwrap_or(root);
    let (_, rows) = rows(start)?;
    let here = rift_git::discover(start)?;
    let Some(row) = rows
        .iter()
        .find(|r| crate::adopt::same_tree(&r.member.work_tree, &here.work_tree))
    else {
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            format!(
                "{} is not a member of the family it sits in — `rift family remint` \
                 if the recorded identity is wrong",
                crate::provenance::safe_path(&here.work_tree.display().to_string())
            ),
            serde_json::json!({ "error": "not_a_member" }),
        ));
    };
    let wiring = wiring_notes(&row.member.work_tree);
    let observations = member_observations(root, &here.worktree_token());
    let mut human = format!(
        "worktree {}\n{}",
        crate::provenance::Label::of(&row.member.name),
        row.human()
    );
    human.push_str("  wiring:\n");
    if wiring.is_empty() {
        human.push_str("    (nothing wired — `rift worktree adopt`)\n");
    }
    for note in &wiring {
        human.push_str(&format!("    {note}\n"));
    }
    human.push_str(&format!("  observations: {}", observations.len()));
    for o in &observations {
        human.push_str(&format!(
            "\n    {} — basis {} @{}",
            crate::provenance::Label::of(&o.0),
            crate::provenance::Label::of(&o.1),
            o.2
        ));
    }
    let mut json = row.json();
    if let Some(obj) = json.as_object_mut() {
        obj.insert("wiring".to_owned(), serde_json::json!(wiring));
        obj.insert(
            "observations".to_owned(),
            serde_json::json!(observations
                .iter()
                .map(|(session, basis, at)| serde_json::json!({
                    "session_id": session, "basis": basis, "first_observed_at": at,
                }))
                .collect::<Vec<_>>()),
        );
    }
    Ok(Outcome::ok(human, json))
}

/// The verify-only client facts this member carries. Read-only throughout: the Codex trust file is the user's,
/// the Recall skill regions are tracked files, and this verb writes neither.
fn wiring_notes(member: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for agent in ["claude-code", "codex", "antigravity"] {
        // An agent with no hook fragment in this member is not wired here, and
        // "recall skill absent" about it would be a finding about a member that
        // has nothing to find. Silence is the honest report; the row's
        // `provisioned: false` is what says the member is unadopted.
        if let Some(binary) = rift_agent_install::native::installed_hook_binary(member, agent) {
            out.push(format!(
                "{agent}: hooks → {}",
                crate::provenance::safe_path(&binary.display().to_string())
            ));
            if let Some(present) = rift_agent_install::native::recall_skill_present(member, agent) {
                out.push(format!(
                    "{agent}: recall skill {}",
                    if present { "present" } else { "absent" }
                ));
            }
        }
    }
    // Codex trusts hooks per absolute path and per hook: a member's fresh
    // `.codex/hooks.json` is untrusted until the user approves it IN Codex,
    // and Rift never writes that file. Reported, never granted.
    let home = rift_agent_install::native::codex_home_from_process();
    if let Some(trust) = rift_agent_install::native::codex_trust(member, home.as_deref()) {
        out.push(format!("codex: trust {}", trust.summary()));
        out.extend(trust.remedy());
    }
    out
}

/// Every `session_worktree` observation recorded in one member, oldest first:
/// `(session_id, basis, first_observed_at)`.
///
/// Total, on [`last_captures`]'s rule: no projection, no relation, or an
/// unreadable one is an empty history rather than a failed verb.
fn member_observations(root: &Path, token: &str) -> Vec<(String, String, i64)> {
    use rift_store::CanonicalStore as _;
    let (Ok(db), Ok(rt)) = (crate::family::projection_db(root), crate::runtime()) else {
        return Vec::new();
    };
    let Ok(store) = rift_store::SqliteStore::open_read_only(&db) else {
        return Vec::new();
    };
    let Ok(rows) = rt.block_on(store.query(
        "SELECT session_id, basis, first_observed_at FROM session_worktree \
         WHERE worktree_token = ? ORDER BY first_observed_at, observation_id",
        &[rift_store::Value::text(token.to_owned())],
    )) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|r| Some((r.get_text(0).ok()?, r.get_text(1).ok()?, r.get_i64(2).ok()?)))
        .collect()
}

/// `rift worktree prune [--yes]` — report the members whose checkout is gone;
/// with `--yes`, run `git worktree prune` through the argv-safe seam.
///
/// The two halves are deliberately asymmetric. Reporting is free and always
/// happens; removing an administrative entry is irreversible for the branch
/// state git keeps beside it, so it needs the word.
fn prune(root: &Path, yes: bool) -> Result<Outcome> {
    let (paths, rows) = rows(root)?;
    let victims: Vec<&Row> = rows.iter().filter(|r| r.member.prunable).collect();
    let mut human = String::new();
    for v in &victims {
        human.push_str(&format!(
            "  {} → {} (administrative entry survives; checkout gone)\n",
            crate::provenance::Label::of(&v.member.name),
            crate::provenance::safe_path(&v.member.work_tree.display().to_string()),
        ));
    }
    if victims.is_empty() {
        human.push_str("  (nothing prunable — every member's checkout is present)\n");
    }
    let mut pruned = false;
    if yes && !victims.is_empty() {
        // An argv ARRAY through the seam that sets `GIT_DIR`/`GIT_WORK_TREE`
        // explicitly. No shell, ever: this verb takes no user-supplied string
        // and it must stay that way.
        rift_git::run_git(&paths, &["worktree".to_owned(), "prune".to_owned()])?;
        pruned = true;
    }
    human.push_str(if pruned {
        "pruned: `git worktree prune` ran in this family"
    } else if victims.is_empty() {
        "nothing to do"
    } else {
        "reported only — re-run with --yes to `git worktree prune`"
    });
    Ok(Outcome::ok(
        human,
        serde_json::json!({
            "prunable": victims.iter().map(|v| serde_json::json!({
                "name": v.member.name,
                "path": v.member.work_tree.display().to_string(),
                "git_dir": v.member.git_dir.display().to_string(),
            })).collect::<Vec<_>>(),
            "pruned": pruned,
        }),
    ))
}

// --- orphan projections -----------------------------------------------------

/// One per-path projection minted for a member before the family mint.
pub(crate) struct Orphan {
    /// The member whose retired id this is.
    pub(crate) member: String,
    /// The `projection.db` file itself.
    pub(crate) db: PathBuf,
    /// The id directory holding it — the unit `--orphan-projections` removes.
    pub(crate) dir: PathBuf,
    /// The retired id, as the cache directory spells it.
    pub(crate) id: Option<String>,
    /// How many captured events it holds; `None` when it cannot be read.
    pub(crate) events: Option<i64>,
}

/// Every per-path projection orphaned by the family mint.
///
/// **The family's own database is excluded by identity**, not by name: the
/// legacy derivation is run for each member and any path equal to the family's
/// is skipped, so the family's adopted ids can never appear in this list and
/// therefore can never be swept by the verb that reads it.
///
/// Only members the family can still enumerate are checked: a worktree already
/// `git worktree prune`d leaves no path to derive an id from, so its projection
/// is unfindable by construction rather than silently counted as absent.
pub(crate) fn orphan_projections(repo: &Path) -> Vec<Orphan> {
    use rift_store::CanonicalStore as _;
    let (Ok(paths), Ok(family_db)) = (rift_git::discover(repo), crate::family::projection_db(repo))
    else {
        return Vec::new();
    };
    let rt = crate::runtime().ok();
    let mut found = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for member in rift_git::family_members(&paths) {
        let Ok(id) = rift_store::capture_repo_id(&member.work_tree) else {
            continue;
        };
        let db = rift_store::projection_db_under(&crate::family::cache_root(), &id);
        if db == family_db || seen.contains(&db) || !db.exists() {
            continue;
        }
        seen.push(db.clone());
        // The event count is what makes the line actionable: an orphan with
        // nothing in it is a file, and one with 58k events is a decision.
        let events = rift_store::SqliteStore::open_read_only(&db)
            .ok()
            .and_then(|s| {
                rt.as_ref().and_then(|rt| {
                    rt.block_on(s.query_opt_i64("SELECT COUNT(*) FROM captured_event", &[]))
                        .ok()
                        .flatten()
                })
            });
        found.push(Orphan {
            member: member.name.clone(),
            id: db
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned()),
            dir: db.parent().map_or_else(|| db.clone(), Path::to_path_buf),
            db,
            events,
        });
    }
    found
}

/// `rift projects prune --orphan-projections [--yes]`.
///
/// Reports path, id and event count; **deletes only** with the explicit
/// `--yes`, and only the per-path id directories [`orphan_projections`] found —
/// never the family's own, which that enumeration cannot return.
///
/// # Errors
/// Propagates nothing: an unreadable repository is an empty report, and a
/// directory that will not delete is named in the output rather than raised.
pub(crate) fn prune_orphan_projections(root: &Path, yes: bool) -> Result<Outcome> {
    let orphans = orphan_projections(root);
    let mut human = String::new();
    let mut removed = 0usize;
    let mut rows = Vec::new();
    for o in &orphans {
        let mut gone = false;
        if yes {
            gone = std::fs::remove_dir_all(&o.dir).is_ok();
            if gone {
                removed += 1;
            }
        }
        human.push_str(&format!(
            "  {} {} → {} ({} event(s)){}\n",
            if yes && gone { "removed" } else { "found" },
            crate::provenance::Label::of(&o.member),
            crate::provenance::safe_path(&o.dir.display().to_string()),
            o.events
                .map_or_else(|| "unreadable".to_owned(), |n| n.to_string()),
            if yes && !gone {
                "  (could not remove)"
            } else {
                ""
            },
        ));
        rows.push(serde_json::json!({
            "member": o.member,
            "id": o.id,
            "path": o.db.display().to_string(),
            "dir": o.dir.display().to_string(),
            "events": o.events,
            "removed": yes && gone,
        }));
    }
    if orphans.is_empty() {
        human.push_str("  (none — every member reads the family's projection)\n");
    }
    human.push_str(&if yes {
        format!(
            "removed {removed} of {} orphan projection(s); \
             projection-only facts they held (per-turn usage, the serve audit) \
             are not recoverable into the family store",
            orphans.len()
        )
    } else {
        format!(
            "{} orphan projection(s) found; nothing was removed — re-run with --yes",
            orphans.len()
        )
    });
    Ok(Outcome::ok(
        human,
        serde_json::json!({
            "orphans": orphans.len(),
            "removed": removed,
            "found": rows,
            "deleted": yes,
        }),
    ))
}
