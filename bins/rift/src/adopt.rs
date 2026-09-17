//! `rift worktree adopt` — a linked worktree becomes a wired member of its
//! family.
//!
//! A fresh `git worktree add` leaves a checkout with no `.rift/agents/*.json`,
//! no `.rift/state/native-integrations.json` and no client fragments, so an
//! agent launched there runs with **zero** Rift hooks: capture stops at the
//! worktree boundary and nothing says so. Adoption closes that, and almost
//! every rule it follows is a refusal:
//!
//! * The family's intent is the projection's `agent_enablement` relation, read
//!   **read-only** — never `open_migrated`. This same function runs from
//!   `post-checkout`, and a git hook that migrates a projection is the
//!   migration-edit race this machine has already paid for once.
//! * Descriptors name the family's **binary of record**, never `current_exe()`:
//!   adopting from a throwaway build must not re-point the family.
//! * A **tracked** file is never rewritten as a side effect. `.agents/hooks.json`
//!   and the Recall skill regions are verified and reported; writing them stays
//!   with the explicit `rift enable <agent>` path, because a git hook that
//!   dirties the tree the user just created is not a repair.
//! * Global client configuration is read, never written. Codex
//!   trusts hooks per absolute path, so a member's fresh `.codex/hooks.json` is
//!   untrusted until the user approves it *in Codex* — reported, never granted.
//! * Git hooks belong to the family and are installed by `rift init` /
//!   `rift hooks install`. Adoption does not touch them, and does not register
//!   the member as a project.
//!
//! What it cannot do, it records: a refusal writes `.rift/state/adoption.json`
//! with its reason, and nothing retries until that marker is cleared — the
//! `post-checkout` hook honours it, and this verb clears it by succeeding.

use std::path::{Path, PathBuf};

use rift_agent_install::native;
use rift_core::Result;
use rift_store::{CanonicalStore, SqliteStore};

use crate::cli::{exit, AdoptArgs, Outcome};

/// The member-local marker a deferred or refused adoption leaves behind.
const MARKER: &str = ".rift/state/adoption.json";

/// What the family wants wired into every member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Intent {
    /// The projection answered: exactly these agents are enabled, in the order
    /// the fold produced them.
    Enabled(Vec<String>),
    /// The projection could not be read. Adopt nothing and say why — an empty
    /// enabled set and an unreadable one are different facts, and treating the
    /// second as the first would silently un-wire a member on a bad read.
    Unknown(String),
}

/// The machine facts adoption writes with, injected rather than resolved, so a
/// test can adopt into a temporary tree without reading this machine's family
/// or this user's Codex configuration.
#[derive(Clone, Debug)]
pub(crate) struct Machine {
    /// The family's binary of record: the absolute `rift` every descriptor and
    /// fragment in this member will name.
    pub(crate) rift_bin: PathBuf,
    /// Codex's home, when adoption may look in it. `None` reads nothing.
    pub(crate) codex_home: Option<PathBuf>,
}

/// What happened to one agent in one member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentState {
    /// Files were written (count).
    Provisioned(usize),
    /// Already wired with these exact bytes.
    Unchanged,
    /// Refused, with the installer's reason. Never overwritten, never retried.
    Refused(String),
}

/// One member's adoption, as a report rather than a side effect.
#[derive(Clone, Debug)]
pub(crate) struct AdoptionReport {
    /// The member work tree that was adopted.
    pub(crate) member: PathBuf,
    /// Per-agent outcome, in the order the family's intent named them.
    pub(crate) agents: Vec<(String, AgentState)>,
    /// Verify-only findings: tracked files and client state Rift may not write.
    pub(crate) notes: Vec<String>,
    /// Why nothing was adopted, when nothing was.
    pub(crate) deferred: Option<String>,
}

impl AdoptionReport {
    /// Whether every agent the family wants is wired in this member.
    pub(crate) fn is_clean(&self) -> bool {
        self.deferred.is_none()
            && !self
                .agents
                .iter()
                .any(|(_, state)| matches!(state, AgentState::Refused(_)))
    }

    /// One line: what this adoption did, for a caller that reports rather than
    /// renders (the doctor leg).
    pub(crate) fn summary(&self) -> String {
        if let Some(reason) = &self.deferred {
            return format!("adopted nothing: {reason}");
        }
        let refused = self.refusals();
        if refused.is_empty() {
            return format!(
                "wired {}",
                if self.agents.is_empty() {
                    "nothing — the family has no agents enabled".to_owned()
                } else {
                    self.agents
                        .iter()
                        .map(|(agent, _)| agent.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            );
        }
        format!("refused: {}", refused.join(", "))
    }

    fn refusals(&self) -> Vec<&str> {
        self.agents
            .iter()
            .filter_map(|(agent, state)| match state {
                AgentState::Refused(_) => Some(agent.as_str()),
                _ => None,
            })
            .collect()
    }

    fn human(&self) -> String {
        let mut out = format!("worktree {}", self.member.display());
        if let Some(reason) = &self.deferred {
            out.push_str(&format!("\n  adopted nothing: {reason}"));
        }
        for (agent, state) in &self.agents {
            let line = match state {
                AgentState::Provisioned(files) => format!("provisioned ({files} file(s))"),
                AgentState::Unchanged => "already wired".to_owned(),
                AgentState::Refused(reason) => format!("refused: {reason}"),
            };
            out.push_str(&format!("\n  {agent:<14} {line}"));
        }
        for note in &self.notes {
            out.push_str(&format!("\n  {note}"));
        }
        out
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "member": self.member.display().to_string(),
            "deferred": self.deferred,
            "agents": self.agents.iter().map(|(agent, state)| serde_json::json!({
                "agent": agent,
                "state": match state {
                    AgentState::Provisioned(_) => "provisioned",
                    AgentState::Unchanged => "unchanged",
                    AgentState::Refused(_) => "refused",
                },
                "changed_files": match state {
                    AgentState::Provisioned(files) => Some(*files),
                    _ => None,
                },
                "reason": match state {
                    AgentState::Refused(reason) => Some(reason.clone()),
                    _ => None,
                },
            })).collect::<Vec<_>>(),
            "notes": self.notes,
            "clean": self.is_clean(),
        })
    }
}

/// The agents the family has enabled: the latest event per agent in
/// `agent_enablement`, folded `ORDER BY changed_at DESC, change_id DESC`.
///
/// The tiebreak is not decoration. `changed_at` is epoch **milliseconds** and
/// two events can share one; `change_id` is the content address of the event,
/// so the order is total and every reader folds the same way.
///
/// Read through [`SqliteStore::open_read_only`], and that is load-bearing:
/// adoption runs from a git hook, `open_capture_projection` would MIGRATE the
/// file it opens, and a migration under a live hook is the failure this
/// repository has already produced once. A missing or unreadable projection is
/// [`Intent::Unknown`] — never an empty enabled set, which would read as "the
/// family wants nothing wired".
pub(crate) fn enabled_agents(projection_db: &Path) -> Intent {
    if !projection_db.is_file() {
        return Intent::Unknown(format!(
            "no projection at {} yet — `rift init` in the main work tree first",
            projection_db.display()
        ));
    }
    let store = match SqliteStore::open_read_only(projection_db) {
        Ok(store) => store,
        Err(error) => return Intent::Unknown(format!("projection unreadable: {error}")),
    };
    let runtime = match crate::runtime() {
        Ok(runtime) => runtime,
        Err(error) => return Intent::Unknown(format!("no runtime: {error}")),
    };
    let rows = runtime.block_on(store.query(
        "SELECT agent, action FROM agent_enablement \
         ORDER BY changed_at DESC, change_id DESC",
        &[],
    ));
    let rows = match rows {
        Ok(rows) => rows,
        Err(error) => return Intent::Unknown(format!("agent_enablement unreadable: {error}")),
    };
    let mut latest: Vec<(String, String)> = Vec::new();
    for row in &rows {
        let (Ok(agent), Ok(action)) = (row.get_text(0), row.get_text(1)) else {
            continue;
        };
        if !latest.iter().any(|(seen, _)| seen == &agent) {
            latest.push((agent, action));
        }
    }
    let mut enabled: Vec<String> = latest
        .into_iter()
        .filter(|(agent, action)| action == "enable" && native::is_alpha_supported(agent))
        .map(|(agent, _)| agent)
        .collect();
    enabled.sort();
    Intent::Enabled(enabled)
}

/// Provision one member: every enabled agent's untracked descriptor and native
/// fragment, written with the binary of record, idempotently. A tracked client
/// surface such as Antigravity's `.agents/hooks.json` is verified below and is
/// modified only by an explicit `rift enable` in that member.
///
/// Never touches git hooks, tracked files, the registry, or global client
/// configuration; a refusal leaves the offending file byte-identical and lands
/// in the report rather than aborting the rest.
pub(crate) fn adopt_member(
    member: &Path,
    machine: &Machine,
    intent: &Intent,
    now_ms: i64,
) -> AdoptionReport {
    let mut report = AdoptionReport {
        member: member.to_path_buf(),
        agents: Vec::new(),
        notes: Vec::new(),
        deferred: None,
    };
    let agents = match intent {
        Intent::Unknown(reason) => {
            report.deferred = Some(reason.clone());
            write_marker(member, now_ms, "deferred", reason);
            return report;
        }
        Intent::Enabled(agents) => agents,
    };
    for agent in agents {
        let state = match native::enable_for_adoption(member, &machine.rift_bin, agent) {
            Ok(installed) if installed.changed_files.is_empty() => AgentState::Unchanged,
            Ok(installed) => AgentState::Provisioned(installed.changed_files.len()),
            Err(error) => AgentState::Refused(error.to_string()),
        };
        verify_only_notes(member, machine, agent, &mut report.notes);
        report.agents.push((agent.clone(), state));
    }
    let refused = report.refusals();
    if refused.is_empty() {
        clear_marker(member);
    } else {
        write_marker(
            member,
            now_ms,
            "refused",
            &format!(
                "{} not adopted; run `rift worktree adopt`",
                refused.join(", ")
            ),
        );
    }
    report
}

/// The facts adoption reports and does not act on: a tracked hook file whose
/// binary has moved, a Recall skill region only the explicit verb may write,
/// and Codex's per-path hook trust.
fn verify_only_notes(member: &Path, machine: &Machine, agent: &str, notes: &mut Vec<String>) {
    if agent == "antigravity" {
        match native::installed_hook_binary(member, agent) {
            Some(binary) if binary != machine.rift_bin => notes.push(format!(
                ".agents/hooks.json: stale — it names {}, not the binary of record \
                 (tracked; verified here, never written by Rift)",
                binary.display()
            )),
            None => notes.push(
                ".agents/hooks.json: no Rift bundle (tracked; verified here, never written by Rift)"
                    .to_owned(),
            ),
            Some(_) => {}
        }
    }
    if agent == "codex" {
        let trust = native::codex_hook_trust(
            &member.join(".codex/hooks.json"),
            machine.codex_home.as_deref(),
        );
        notes.push(format!("codex trust: {}", trust.message()));
    }
    if native::recall_skill_present(member, agent) == Some(false) {
        notes.push(format!(
            "{agent}: the Recall skill region is missing (a tracked file; \
             `rift enable {agent}` writes it)"
        ));
    }
}

/// Why a previous adoption of this member stopped, if one did.
///
/// The `post-checkout` hook reads this to stay off a member that has already
/// refused: paying full adoption on every branch switch, to fail the same way,
/// is exactly the cost the 100 ms git-latency budget has no room for.
pub(crate) fn marker_reason(member: &Path) -> Option<String> {
    let bytes = std::fs::read(member.join(MARKER)).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let outcome = value.get("outcome")?.as_str()?;
    let reason = value.get("reason").and_then(serde_json::Value::as_str)?;
    Some(format!("{outcome}: {reason}"))
}

fn write_marker(member: &Path, now_ms: i64, outcome: &str, reason: &str) {
    let path = member.join(MARKER);
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let document = serde_json::json!({
        "schema_version": 1,
        "attempted_at": now_ms,
        "outcome": outcome,
        "reason": reason,
    });
    let Ok(mut bytes) = serde_json::to_vec_pretty(&document) else {
        return;
    };
    bytes.push(b'\n');
    // Best effort by contract: this runs inside a git hook, and a marker that
    // cannot be written is a retry, never a failed checkout.
    let _ = std::fs::write(&path, &bytes);
}

fn clear_marker(member: &Path) {
    let _ = std::fs::remove_file(member.join(MARKER));
}

/// The binary of record and the Codex home, resolved from this machine.
pub(crate) fn machine_for(paths: &rift_git::RepoPaths) -> Result<Machine> {
    let sites = crate::hooks::family_sites(&paths.common_dir);
    let rift_bin = rift_shadow::hooks::read_binary_of_record(&sites.dir)
        .map(|record| record.path().to_path_buf())
        .map_or_else(std::env::current_exe, Ok)?;
    Ok(Machine {
        rift_bin,
        codex_home: codex_home(),
    })
}

/// Codex's home: `$CODEX_HOME`, else `$HOME/.codex`. Read-only, always.
fn codex_home() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("CODEX_HOME") {
        return Some(PathBuf::from(explicit));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex"))
}

/// `rift worktree adopt [PATH]` — routed here by [`crate::worktree::run`],
/// which owns the group's other three verbs.
pub(crate) fn run(args: &AdoptArgs, root: &Path) -> Result<Outcome> {
    adopt_at(args.path.as_deref().unwrap_or(root), crate::hooks::now_ms())
}

fn adopt_at(start: &Path, now_ms: i64) -> Result<Outcome> {
    let Ok(paths) = rift_git::discover(start) else {
        return Ok(Outcome::coded(
            exit::NOT_INIT,
            format!(
                "not inside a git repository: {} — a worktree is adopted from within it",
                start.display()
            ),
            serde_json::json!({ "error": "not_a_repository" }),
        ));
    };
    let members = rift_git::family_members(&paths);
    let member = members
        .iter()
        .find(|member| same_tree(&member.work_tree, &paths.work_tree));
    if member.is_some_and(|member| member.is_main) {
        return Ok(Outcome::ok(
            format!(
                "{} is the family's main work tree — `rift init` adopts it; nothing to do here",
                paths.work_tree.display()
            ),
            serde_json::json!({ "member": false, "adopted": false }),
        ));
    }
    let machine = machine_for(&paths)?;
    // What stopped the last attempt, before this one clears or replaces it: a
    // member that has been refusing since its `post-checkout` fired deserves to
    // say so once, rather than only through a file the user has to know about.
    let previous = marker_reason(&paths.work_tree);
    let intent = enabled_agents(&crate::family::projection_db(&paths.work_tree)?);
    let report = adopt_member(&paths.work_tree, &machine, &intent, now_ms);
    let mut human = format!(
        "adopted worktree of family {}\n{}",
        main_of(&members).display(),
        report.human()
    );
    if let Some(previous) = &previous {
        human.push_str(&format!("\n  previous attempt — {previous}"));
    }
    let mut json = report.json();
    if let Some(object) = json.as_object_mut() {
        object.insert("previous_attempt".to_owned(), serde_json::json!(previous));
    }
    let code = if report.is_clean() {
        exit::OK
    } else {
        exit::PRECONDITION
    };
    Ok(Outcome::coded(code, human, json))
}

/// The family's main work tree, for the "of family X" line; the member's own
/// path when the main tree is unprovable (which is legal).
fn main_of(members: &[rift_git::FamilyMember]) -> PathBuf {
    members
        .iter()
        .find(|member| member.is_main)
        .map_or_else(PathBuf::new, |member| member.work_tree.clone())
}

/// Two paths naming the same checkout, compared canonically where they exist.
pub(crate) fn same_tree(left: &Path, right: &Path) -> bool {
    use rift_git::family::canonical_or_lexical;
    canonical_or_lexical(left) == canonical_or_lexical(right)
}

#[cfg(test)]
mod tests {
    use rift_git::test_support::{fake_repo, fake_worktree};

    use super::*;

    /// A family fixture: a main work tree and one linked member, built from the
    /// exact files `git init` and `git worktree add` write.
    fn family(tmp: &Path) -> (PathBuf, PathBuf) {
        let main = tmp.join("main");
        std::fs::create_dir_all(&main).unwrap();
        let main_git = fake_repo(&main).unwrap();
        let member = tmp.join("member");
        fake_worktree(&main_git, &member, "member").unwrap();
        (main, member)
    }

    fn machine(rift_bin: &str) -> Machine {
        Machine {
            rift_bin: PathBuf::from(rift_bin),
            // No Codex home: a unit test reads this user's real `~/.codex`
            // never, and the trust line stays the honest "pending".
            codex_home: None,
        }
    }

    /// A projection with `agent_enablement` history, opened through the
    /// migration engine exactly as the product's writer does.
    fn projection_with(db: &Path, events: &[(&str, &str, i64)]) {
        let store = rift_store::open_capture_projection(db).unwrap();
        let runtime = crate::runtime().unwrap();
        for (index, (agent, action, at)) in events.iter().enumerate() {
            runtime
                .block_on(rift_store::record_agent_enablement(
                    &store,
                    &rift_store::AgentEnablementRow {
                        change_id: format!("{index:064}"),
                        agent: (*agent).to_owned(),
                        action: (*action).to_owned(),
                        changed_at: *at,
                        wired_hooks_hash: "hash".to_owned(),
                        schema_version: 1,
                    },
                ))
                .unwrap();
        }
    }

    /// ★ The whole verb in one assertion set: the family's intent comes from
    /// `agent_enablement` (latest event per agent wins, disabled agents stay
    /// out), and every enabled agent is wired into the member with the BINARY
    /// OF RECORD — not with whatever binary happens to be running the adoption.
    #[test]
    fn adopt_provisions_every_enabled_agent_with_the_binary_of_record() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, member) = family(tmp.path());
        let db = tmp.path().join("projection.db");
        // The last two share a millisecond on purpose: `changed_at` is epoch
        // ms and a same-ms pair is the case `change_id DESC` exists for. The
        // enable carries the higher content address, so codex is enabled.
        projection_with(
            &db,
            &[
                ("claude-code", "enable", 10),
                ("antigravity", "enable", 30),
                ("antigravity", "disable", 40),
                ("codex", "disable", 20),
                ("codex", "enable", 20),
            ],
        );

        let intent = enabled_agents(&db);
        assert_eq!(
            intent,
            Intent::Enabled(vec!["claude-code".to_owned(), "codex".to_owned()]),
            "the latest event per agent decides; a disabled agent is not adopted"
        );

        let report = adopt_member(&member, &machine("/opt/rift/rift"), &intent, 1_700);
        assert!(report.is_clean(), "{report:?}");
        for (relative, needle) in [
            (".rift/agents/claude-code.json", "/opt/rift/rift"),
            // The quotes are escaped ON DISK: the command Rift writes quotes
            // the absolute binary, and the JSON encoding escapes those quotes.
            (
                ".claude/settings.local.json",
                r#"/opt/rift/rift\" hooks claude-code"#,
            ),
            (".rift/agents/codex.json", "/opt/rift/rift"),
            (".codex/hooks.json", r#"/opt/rift/rift\" hooks codex"#),
            (".rift/state/native-integrations.json", "codex"),
        ] {
            let text = std::fs::read_to_string(member.join(relative))
                .unwrap_or_else(|error| panic!("{relative}: {error}"));
            assert!(text.contains(needle), "{relative} must name {needle}");
        }
        assert!(
            !member.join(".rift/agents/antigravity.json").exists(),
            "a disabled agent is not wired"
        );
        assert!(
            report
                .notes
                .iter()
                .any(|note| note == "codex trust: pending (approve once in Codex)"),
            "the fragment Rift can write is not the trust only Codex can grant: {:?}",
            report.notes
        );
    }

    /// ★ Adoption never rewrites what it does not own. The member's Claude Code
    /// fragment carries somebody else's Rift command: it is refused, left
    /// byte-identical, and the marker records why so the `post-checkout` hook
    /// does not retry it on every branch switch.
    #[test]
    fn adopt_refuses_a_user_modified_fragment_as_a_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, member) = family(tmp.path());
        let fragment = member.join(".claude/settings.local.json");
        std::fs::create_dir_all(fragment.parent().unwrap()).unwrap();
        let theirs = br#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"\"/their/rift\" hooks claude-code SessionStart","timeout":90}]}]}}"#;
        std::fs::write(&fragment, theirs).unwrap();

        let report = adopt_member(
            &member,
            &machine("/opt/rift/rift"),
            &Intent::Enabled(vec!["claude-code".to_owned()]),
            1_700,
        );

        assert!(!report.is_clean());
        assert!(
            matches!(&report.agents[0].1, AgentState::Refused(reason) if reason.contains("ownership fingerprint")),
            "{report:?}"
        );
        assert_eq!(
            std::fs::read(&fragment).unwrap(),
            theirs,
            "a file Rift does not own comes back byte-identical"
        );
        assert!(
            !member.join(".rift/agents/claude-code.json").exists(),
            "a refused agent leaves no descriptor claiming it is wired"
        );
        let marker = marker_reason(&member).expect("a refusal is recorded");
        assert!(marker.starts_with("refused: claude-code"), "{marker}");
    }

    /// ★ The two things adoption must never do, asserted as two byte
    /// comparisons rather than as an absence of code.
    ///
    /// **Git hooks** are the family's, installed by `rift init`; an adoption
    /// that re-pointed them is the D2 defect wearing a new hat. **Tracked
    /// files** — `.agents/hooks.json`, the Recall skill regions — belong to the
    /// repository, and a git hook that dirties the tree the user just created
    /// is not a repair. Both are planted STALE here, so a writer would show up
    /// as a changed byte, and both are reported instead.
    #[test]
    fn adopt_never_touches_git_hooks_or_tracked_files() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, member) = family(tmp.path());
        let hooks = main.join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let hook_bytes = b"#!/bin/sh\nexec '/old/rift' hooks git post-commit \"$@\"\n";
        for name in ["post-commit", "pre-push"] {
            std::fs::write(hooks.join(name), hook_bytes).unwrap();
        }
        let tracked = member.join(".agents/hooks.json");
        std::fs::create_dir_all(tracked.parent().unwrap()).unwrap();
        let tracked_bytes = br#"{"rift-recall":{"enabled":true,"Stop":[{"type":"command","command":"\"/old/rift\" hooks antigravity Stop","timeout":10}]}}"#;
        std::fs::write(&tracked, tracked_bytes).unwrap();
        let agents_md = member.join("AGENTS.md");
        std::fs::write(&agents_md, "# member instructions\n").unwrap();

        let report = adopt_member(
            &member,
            &machine("/opt/rift/rift"),
            &Intent::Enabled(vec!["antigravity".to_owned(), "codex".to_owned()]),
            1_700,
        );

        assert!(report.is_clean(), "{report:?}");
        for name in ["post-commit", "pre-push"] {
            assert_eq!(
                std::fs::read(hooks.join(name)).unwrap(),
                hook_bytes,
                "{name}: adoption never re-points a git hook"
            );
        }
        assert_eq!(
            std::fs::read(&tracked).unwrap(),
            tracked_bytes,
            "a tracked hook file is verified, never written"
        );
        assert_eq!(
            std::fs::read_to_string(&agents_md).unwrap(),
            "# member instructions\n",
            "a tracked skill file is verified, never written"
        );
        assert!(
            report.notes.iter().any(
                |note| note.contains(".agents/hooks.json: stale") && note.contains("/old/rift")
            ),
            "the stale tracked bundle is REPORTED: {:?}",
            report.notes
        );
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.contains("codex: the Recall skill region is missing")),
            "the missing tracked region is REPORTED: {:?}",
            report.notes
        );
    }

    /// ★ Twice is once. `post-checkout` can fire on every branch switch and
    /// `rift doctor --fix` on every run, so a second adoption must write no
    /// byte — and must leave no marker behind after a clean one.
    #[test]
    fn adopt_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, member) = family(tmp.path());
        let intent = Intent::Enabled(vec!["claude-code".to_owned(), "codex".to_owned()]);
        let first = adopt_member(&member, &machine("/opt/rift/rift"), &intent, 1_700);
        assert!(first.is_clean(), "{first:?}");
        assert!(
            first
                .agents
                .iter()
                .all(|(_, state)| matches!(state, AgentState::Provisioned(_))),
            "{first:?}"
        );
        let before = tree_snapshot(&member);

        let second = adopt_member(&member, &machine("/opt/rift/rift"), &intent, 9_900);
        assert!(second
            .agents
            .iter()
            .all(|(_, state)| state == &AgentState::Unchanged));
        assert_eq!(before, tree_snapshot(&member), "a second adoption wrote");
        assert_eq!(marker_reason(&member), None, "a clean adoption leaves none");
    }

    /// No projection is not an empty enabled set. Adoption writes nothing, says
    /// why, and leaves the marker the hook reads before it pays for a retry.
    #[test]
    fn an_unreadable_projection_adopts_nothing_and_defers_to_doctor() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, member) = family(tmp.path());
        let intent = enabled_agents(&tmp.path().join("absent.db"));
        assert!(matches!(intent, Intent::Unknown(_)), "{intent:?}");

        let report = adopt_member(&member, &machine("/opt/rift/rift"), &intent, 4_242);
        assert!(!report.is_clean());
        assert!(report.agents.is_empty(), "nothing was wired");
        assert!(!member.join(".rift/agents").exists(), "and nothing written");
        let marker: serde_json::Value =
            serde_json::from_slice(&std::fs::read(member.join(MARKER)).unwrap()).unwrap();
        assert_eq!(marker["outcome"], "deferred");
        assert_eq!(marker["attempted_at"], 4_242);
        assert!(marker["reason"].as_str().unwrap().contains("no projection"));
    }

    /// The main work tree is `rift init`'s to adopt, and the verb says so
    /// rather than provisioning it a second time.
    #[test]
    fn adopting_the_main_work_tree_is_a_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, _) = family(tmp.path());
        let outcome = adopt_at(&main, 1_700).unwrap();
        assert_eq!(outcome.code, exit::OK);
        assert!(
            outcome.human.contains("main work tree"),
            "{}",
            outcome.human
        );
        assert!(!main.join(".rift/agents").exists());
    }

    fn tree_snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(bytes) = std::fs::read(&path) {
                    out.push((path, bytes));
                }
            }
        }
        out.sort();
        out
    }
}
