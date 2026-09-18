//! The work each member still owes: idle sessions no hook will finalize,
//! the condenses and agent jobs queued in each gitdir, and the push queue
//! the family shares.
//!
//! Every family-wide leg compares gitdirs, and the vantage's own has to be
//! spelled the way a `FamilyMember`'s is — `vantage_git_dir` is that
//! spelling, and the reason both legs live in one file.

use std::path::Path;

use rift_shadow::hooks::session_cache::IDLE_TIMEOUT_MS;

use crate::{agent_queue, capture_queue, hooks, hooktime, push_queue, session_verbs, verify};

/// Where the push queue is, and whether any member still carries a queue an
/// older build left under its own gitdir.
///
/// The queue names refs, and refs belong to the family — so it lives under the
/// common dir, and this leg says so with the path, because "where is my queue"
/// stopped having a per-checkout answer and a developer who remembers the old
/// one deserves to be told rather than to guess.
///
/// **`--fix` adopts, it never ships.** Adoption moves entries between two local
/// directories; nothing leaves the machine and no remote is contacted, which is
/// what makes it a safe fix. Shipping is still `rift sync flush`'s, and still
/// refuses without a configured checkpoint remote.
pub(super) fn doctor_push_queue(repo: &Path, fix: bool) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let adopted = fix.then(|| push_queue::adopt_legacy(&paths));
    // After the fix, if one ran: a leg that reported the pre-fix state next to
    // a post-fix count would be describing two different moments as one.
    let legacy = push_queue::legacy_dirs(&paths);
    let dir = push_queue::queue_dir(&paths.common_dir);
    let entries = push_queue::pending(&paths.common_dir).len();
    let refs = push_queue::pending_refs(&paths.common_dir).len();
    let mut human = format!(
        "\n  push queue:     {entries} entry(ies), {refs} ref(s) at {} (the repository's, \
         shared by every worktree)",
        dir.display()
    );
    if let Some(report) = &adopted {
        if !report.is_empty() {
            human.push_str(&format!(
                "\n    adopted {} entry(ies) from {} per-worktree queue(s)",
                report.adopted,
                report.dirs.len()
            ));
        }
    }
    for stranded in &legacy {
        human.push_str(&format!(
            "\n    still per-worktree: {} (safe --fix adopts it)",
            stranded.display()
        ));
    }
    let json = serde_json::json!({
        "queue_dir": rift_git::display_path(&dir).display().to_string(),
        "pending": entries,
        "refs": refs,
        "legacy_dirs": legacy
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>(),
        "adopted": adopted.as_ref().map_or(0, |r| r.adopted),
        "refused": adopted.as_ref().map_or(0, |r| r.refused),
    });
    (human, json)
}

/// The deferred-condense leg. `post-commit` queues its condense and detaches, so
/// a machine that was interrupted mid-drain has work waiting that nothing will
/// mention until the next commit. Doctor is where a developer who is *not*
/// committing can see it — which is exactly the case where the next-hook sweep
/// never fires.
///
/// Report-only: it reports depth, never drains. A drain spawns nine git
/// processes per entry and `doctor` is a diagnosis, not an action. The safe
/// `--fix` is the worker's own drain, run now instead of on the next commit —
/// idempotent and lock-guarded like every drain — here and in the siblings',
/// which is where these queues genuinely strand: they stay per gitdir by
/// design (a checkout's private progress is what discriminates its work from
/// a sibling's), so the only process that ever drains a member's queue is one
/// running in that member, and a member whose checkout is gone has no such
/// process, ever again. Doctor is the pass that runs from somewhere else. A
/// sibling whose entries name a missing work tree condenses commit-only
/// (`hooks::WORKTREE_MISSING_COMMIT_ONLY`); nothing here decides that, the
/// worker does.
///
/// The said-once markers (a failed condense, a pack that will not compile, a
/// turn-boundary snapshot that failed) are OBSERVED without consumption: the
/// hook path is what says them once and clears them; doctor only reports, so
/// running it twice reports twice.
pub(super) fn doctor_capture_queue(repo: &Path, fix: bool) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    if fix && !capture_queue::entries(&paths.git_dir).is_empty() {
        let _ = hooks::drain_capture_queue(paths.git_dir.to_str());
    }
    let siblings = sibling_queues(&paths, fix);
    let files = capture_queue::entry_file_count(&paths.git_dir);
    let drainable = capture_queue::entries(&paths.git_dir).len();
    let unreadable = files.saturating_sub(drainable);
    let notes = capture_queue::peek_errors(&paths.git_dir);
    let agent_jobs = AgentJobs::at(&paths.git_dir);
    let mut human = queue_headline(drainable, unreadable);
    human.push_str(&agent_jobs.line());
    if !notes.is_empty() {
        human.push_str(&format!(
            "\n  capture notes:  {} (reported, not consumed — the next hook says each once)",
            notes.len()
        ));
    }
    for s in &siblings {
        human.push_str(&s.line());
    }
    (
        human,
        serde_json::json!({
            "pending": drainable,
            "unreadable": unreadable,
            "files": files,
            "agent_jobs": { "pending": agent_jobs.pending, "unreadable": agent_jobs.unreadable },
            "notes": notes,
            "siblings": siblings.iter().map(SiblingQueue::json).collect::<Vec<_>>(),
        }),
    )
}

/// The leg's first line. Two counts, never one: files this build cannot read
/// are exactly what doctor exists to surface, and folding them into the
/// drainable count makes them invisible — "2 awaiting condense" reads
/// identically whether both will drain or one will sit there forever. The
/// unreadable count is named in EVERY branch that has one, including the
/// branch where healthy entries are present too, which is the branch a real
/// repository is actually in.
fn queue_headline(drainable: usize, unreadable: usize) -> String {
    match (drainable, unreadable) {
        (0, 0) => "\n  capture queue:  empty".to_owned(),
        (0, u) => format!(
            "\n  capture queue:  {u} entry file(s) this build cannot read \
             (left in place, never deleted)"
        ),
        (d, 0) => format!(
            "\n  capture queue:  {d} commit(s) awaiting condense \
             (the next commit's worker drains them)"
        ),
        (d, u) => format!(
            "\n  capture queue:  {d} commit(s) awaiting condense, and {u} entry \
             file(s) this build cannot read (left in place, never deleted)"
        ),
    }
}

/// The agent-hook journal (the session-end condense and the read token counts
/// a hook handed to the detached worker): a job still here is one no worker
/// finished, and `--fix`'s sweep drains it.
struct AgentJobs {
    files: usize,
    pending: usize,
    unreadable: usize,
}

impl AgentJobs {
    fn at(git_dir: &Path) -> Self {
        let files = agent_queue::job_file_count(git_dir);
        let pending = agent_queue::jobs(git_dir).len();
        Self {
            files,
            pending,
            unreadable: files.saturating_sub(pending),
        }
    }

    /// The journal's line, or nothing when it holds no file at all.
    fn line(&self) -> String {
        if self.files == 0 {
            return String::new();
        }
        let mut line = format!(
            "\n  agent jobs:     {} awaiting the worker (safe --fix drains them)",
            self.pending
        );
        if self.unreadable > 0 {
            line.push_str(&format!(
                ", and {} job file(s) this build cannot read (left in place)",
                self.unreadable
            ));
        }
        line
    }
}

/// One other member's queue depth, as the capture-queue leg reports it.
struct SiblingQueue {
    name: String,
    pending: usize,
    agent_jobs: usize,
    prunable: bool,
}

impl SiblingQueue {
    fn line(&self) -> String {
        format!(
            "\n  sibling queue:  {} awaiting condense in '{}'{}{}",
            self.pending,
            self.name,
            if self.agent_jobs > 0 {
                format!(", {} agent job(s)", self.agent_jobs)
            } else {
                String::new()
            },
            if self.prunable {
                " — its checkout is gone, so these condense commit-only"
            } else {
                ""
            }
        )
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "pending": self.pending,
            "agent_jobs": self.agent_jobs,
            "prunable": self.prunable,
        })
    }
}

/// Every member of the family except the one being diagnosed, with what its
/// gitdir still owes — drained first when `fix` is set, so the numbers reported
/// are the ones that remain.
fn sibling_queues(paths: &rift_git::RepoPaths, fix: bool) -> Vec<SiblingQueue> {
    let here = vantage_git_dir(paths);
    let mut out = Vec::new();
    for m in rift_git::family_members(paths) {
        if m.git_dir == here {
            continue; // the leg above already reported this one
        }
        let owed = |g: &Path| (capture_queue::entries(g).len(), agent_queue::jobs(g).len());
        if fix && owed(&m.git_dir) != (0, 0) {
            let _ = hooks::drain_capture_queue(m.git_dir.to_str());
        }
        let (pending, agent_jobs) = owed(&m.git_dir);
        if pending == 0 && agent_jobs == 0 {
            continue;
        }
        out.push(SiblingQueue {
            name: m.name,
            pending,
            agent_jobs,
            prunable: m.prunable,
        });
    }
    out
}

/// The vantage's own gitdir, spelled the way a [`rift_git::FamilyMember`]'s is,
/// so "is this member the one I am standing in?" can be asked at all.
///
/// `rift_git::discover` returns the gitdir as the upward walk found it;
/// `family_members` canonicalizes. Under any symlinked prefix — `/tmp` and
/// `/var` on macOS, a home on a linked volume — those are two spellings of one
/// directory and every raw `==` between them answers no. The consequences are
/// not cosmetic: the capture-queue leg would report the vantage's OWN queue as
/// a sibling's (counting it twice, once as "capture queue" and once as
/// "sibling queue"), and the session sweep would treat its own unreadable
/// checkout as a neighbour's and carry on instead of saying so.
fn vantage_git_dir(paths: &rift_git::RepoPaths) -> std::path::PathBuf {
    std::fs::canonicalize(&paths.git_dir).unwrap_or_else(|_| paths.git_dir.clone())
}

/// The report-only half of the session leg: what every member's journal
/// holds, and which of it is stuck. Nothing is finalized and nothing is
/// opened for writing — this is the half a developer runs to look first.
fn doctor_session_report(paths: &rift_git::RepoPaths) -> (String, serde_json::Value) {
    use rift_shadow::hooks::session_cache::is_idle_past;

    let now = hooks::now_ms();
    let here = vantage_git_dir(paths);
    let mut live_count = 0_usize;
    let mut stuck: Vec<String> = Vec::new();
    let mut stranded = 0_usize;
    let mut members = Vec::new();
    let mut elsewhere = String::new();
    for journal in session_verbs::family_journal(paths) {
        let mut member_live = 0_usize;
        let mut member_stuck = 0_usize;
        for s in journal.sessions.iter().filter(|s| s.state != "ended") {
            member_live += 1;
            if is_idle_past(s, now, IDLE_TIMEOUT_MS) {
                member_stuck += 1;
                stuck.push(s.session_id.clone());
            }
        }
        if journal.member.prunable {
            // Counted apart, and said apart below. The sweep condenses a
            // session against the tree it was worked in, and this one has no
            // tree left — a subagent's worktree, removed when it finished,
            // whose journal outlived it. Folding these into the headline would
            // have `--fix` promise a repair it will not perform.
            stranded += member_stuck;
        }
        live_count += member_live;
        // Which member holds them, but only where it is news: the vantage's
        // own count is already the headline, and a line per member would
        // turn a healthy seven-worktree family into seven lines of zero.
        if member_live > 0 && journal.member.git_dir != here {
            elsewhere.push_str(&format!(
                "\n    in \'{}\':  {member_live} live, {member_stuck} stuck",
                journal.member.name
            ));
        }
        members.push(serde_json::json!({
            "name": journal.member.name,
            "live": member_live,
            "stuck": member_stuck,
            "prunable": journal.member.prunable,
        }));
    }
    let mut report = if stuck.is_empty() {
        format!("\n  sessions:       {live_count} live, none stuck")
    } else {
        format!(
            "\n  sessions:       {live_count} live, {} STUCK past {}h \
             (safe --fix finalizes those whose checkout still exists, in \
             whichever member they are)",
            stuck.len(),
            IDLE_TIMEOUT_MS / 3_600_000
        )
    };
    report.push_str(&elsewhere);
    if stranded > 0 {
        report.push_str(&format!(
            "\n    stranded:     {stranded} of them in a member whose checkout is gone \
             — nothing can condense them; `git worktree prune` retires the entry \
             (the capture-queue leg above says what it still owes)"
        ));
    }
    (
        report,
        serde_json::json!({
            "live": live_count,
            "stuck": stuck,
            "stranded": stranded,
            "members": members,
        }),
    )
}

/// The crashed-session leg. Doctor is where an abandoned
/// session gets finalized, because it is the only pass that runs without an
/// agent: a crashed session fires no more hooks, so nothing else would ever
/// reach it. A session inside its idle window is only reported.
///
/// **Every member of the family, from any member.** Session
/// caches stay per gitdir, so this leg read exactly one of them:
/// a session crashed in a side worktree was reachable only by a doctor run
/// *inside that worktree*, and a worktree nobody opens again is one nobody
/// ever runs doctor in. The enumeration is `session_verbs::family_journal`,
/// which credits each session to the member that owns it; the fix
/// sweeps each member from its own checkout, so a condensed checkpoint carries
/// the worktree token of the tree the work happened in and never a
/// neighbour's.
///
/// Report-only without `--fix`: the finding is the list of stuck sessions;
/// finalizing them is the fix, and a diagnosis command that repairs on sight
/// leaves the operator no way to look first.
///
/// A session it could not condense is reported as still owed, and this pass is
/// what will owe it again: re-running doctor retries. Nothing else can — see
/// `hooks::sweep_stale_sessions`.
///
/// Best-effort by construction — outside a repository, or with no projection
/// yet, this leg reports nothing rather than failing the diagnosis. A member
/// whose checkout is gone is not swept (there is no tree to archive against;
/// its queued work degrades commit-only in the capture-queue leg) and a member
/// that cannot be swept is counted rather than allowed to fail the pass.
pub(super) fn doctor_session_sweep(repo: &Path, fix: bool) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return sessions_unreadable();
    };
    if !fix {
        return doctor_session_report(&paths);
    }
    let Some(family) = sweep_family(&paths) else {
        return sessions_unreadable();
    };
    let mut human = sweep_lines(&family);
    if let Some(reason) = accrue_sweep_soak(repo, &family.sweep.condensed_checkpoints) {
        human.push_str(&format!("\n    soak verify:  {reason}"));
    }
    if !family.sweep.condense_deferred.is_empty() {
        human.push_str(&deferred_line(family.sweep.condense_deferred.len()));
    }
    let json = serde_json::json!({
        "readable": true,
        "live": family.sweep.live,
        "finalized": family.sweep.finalized,
        "condense_deferred": family.sweep.condense_deferred,
        "idle_timeout_ms": IDLE_TIMEOUT_MS,
        "members_swept": family.swept,
        "members_unswept": family.unswept,
    });
    (human, json)
}

/// The answer this leg has always given when the checkout it is standing in
/// cannot be read.
fn sessions_unreadable() -> (String, serde_json::Value) {
    (
        "\n  sessions:       unreadable (not a repository, or no projection yet)".to_owned(),
        serde_json::json!({ "readable": false }),
    )
}

/// One sweep over every member of the family, and how many members it could
/// and could not read.
struct FamilySweep {
    sweep: hooks::SessionSweep,
    swept: usize,
    unswept: usize,
}

/// Sweep each member from its own checkout, folding the results into one.
/// `None` when the vantage itself is unreadable — the answer this leg has
/// always given; another member being unreadable is a count, never a reason
/// to stop diagnosing the one the operator is standing in. A member whose
/// checkout is gone is skipped: there is no tree to condense against.
fn sweep_family(paths: &rift_git::RepoPaths) -> Option<FamilySweep> {
    let now = hooks::now_ms();
    let vantage = vantage_git_dir(paths);
    let mut family = FamilySweep {
        sweep: hooks::SessionSweep::default(),
        swept: 0,
        unswept: 0,
    };
    for journal in session_verbs::family_journal(paths) {
        if journal.member.prunable {
            continue;
        }
        let here = journal.member.git_dir == vantage;
        match hooks::sweep_stale_sessions(&journal.member.work_tree, now, IDLE_TIMEOUT_MS, None) {
            Ok(one) => {
                family.swept += 1;
                family.sweep.live.extend(one.live);
                family.sweep.finalized.extend(one.finalized);
                family.sweep.condense_deferred.extend(one.condense_deferred);
                family
                    .sweep
                    .condensed_checkpoints
                    .extend(one.condensed_checkpoints);
            }
            Err(_) if here => return None,
            Err(_) => family.unswept += 1,
        }
    }
    Some(family)
}

/// The headline and the two counts that follow it when they are non-zero.
fn sweep_lines(family: &FamilySweep) -> String {
    let mut human = format!(
        "\n  sessions:       {} live across {} member(s)",
        family.sweep.live.len(),
        family.swept
    );
    if family.unswept > 0 {
        human.push_str(&format!(
            "\n    unswept:      {} member(s) this pass could not read",
            family.unswept
        ));
    }
    if !family.sweep.finalized.is_empty() {
        human.push_str(&format!(
            "\n    finalized:    {} idle past {}h — condensed and closed",
            family.sweep.finalized.len(),
            IDLE_TIMEOUT_MS / 3_600_000
        ));
    }
    human
}

/// The sweep is a **successful condense**, so it accrues a soak observation
/// exactly like the detached worker's does; the failure text comes back for
/// the caller to place on doctor's own output.
///
/// Measured, not assumed: without this, running `rift doctor` on a repo with
/// an idle session condensed it, closed the session, and accrued NOTHING —
/// and because the session was then gone, the next commit had nothing to
/// archive and accrued nothing either. Doctor is the command that *displays*
/// the soak counter, so the counter was suppressed by the act of reading it.
/// `doctor_soak` runs after this, so the row it writes is visible in the same
/// output. A failed verify is not fatal here for the same reason it is not
/// fatal in the worker: the row is recorded either way, and doctor's job is
/// to report, never to refuse.
///
/// ONE verify for the whole sweep, witnessing every checkpoint it MINTED —
/// keyed on the minted ids rather than on `finalized`, because the two can
/// disagree: a session whose archive wrote a checkpoint and whose
/// `mark_condense_done` then failed lands in `condense_deferred`, and its
/// condensation is no less real for that. `Witness::of_all` returns `None` on
/// an empty slice and THAT ARM IS REACHABLE — a sweep that minted nothing
/// must record no run at all rather than an unwitnessed one. One witness for
/// the whole family, too: the checkpoints are the family's (the refs live in
/// the common dir) and so is the projection the row lands in, so a
/// per-member verify would record the same corpus once per worktree.
///
/// Through `accrue_soak` rather than `soak_verify` directly, for two reasons
/// that survive the levelling having moved inside `verify::run_at_with`: the
/// `HookTimer` phases, so the sweep's cost is measured like the worker's, and
/// the failure text coming back as a value.
fn accrue_sweep_soak(repo: &Path, condensed_checkpoints: &[String]) -> Option<String> {
    let witness = verify::Witness::of_all(condensed_checkpoints)?;
    let t = hooktime::HookTimer::from_env("doctor-sweep");
    hooks::accrue_soak(&t, repo, &witness)
}

/// Honest about all three: what was NOT done (nothing was archived, so the
/// session is still owed a condense), why, and who retries — this pass,
/// because nothing else ever will. There is exactly one cause left to name:
/// with the archive route and its binary both gone, an archive that produced
/// nothing had nothing to anchor to or failed outright.
fn deferred_line(deferred: usize) -> String {
    format!(
        "\n    deferred:     {deferred} idle, condense still owed — nothing archived \
         (no commit to anchor to, or the archive itself failed); re-run \
         `rift doctor` to retry"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vantage's gitdir has to be spelled the way a `FamilyMember`'s is,
    /// or every family-wide leg mistakes the checkout it is standing in for a
    /// neighbour. The two spellings are reachable in production: the palette
    /// dispatches against the SELECTED PROJECT'S registered path (never process
    /// cwd, which the kernel has already resolved), and a project registered
    /// through a symlink keeps the link's spelling for the life of its row.
    #[cfg(unix)]
    #[test]
    fn the_vantage_gitdir_is_spelled_the_way_a_members_is() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        rift_git::test_support::fake_repo(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let paths = rift_git::discover(&link).unwrap();
        let members = rift_git::family_members(&paths);
        assert_eq!(members.len(), 1, "one main work tree");
        assert_ne!(
            paths.git_dir, members[0].git_dir,
            "discover keeps the link's spelling and family_members canonicalizes — \
             if these ever agree by themselves this test proves nothing"
        );
        assert_eq!(
            vantage_git_dir(&paths),
            members[0].git_dir,
            "so the comparison every leg makes has to go through vantage_git_dir"
        );
    }
}
