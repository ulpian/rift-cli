//! `rift chain` — the checkpoint order, recorded in `refs/rift/chain/v1`.
//!
//! Two verbs, and the split between them is the point.
//!
//! `status` reads. `init` **creates** the one sequence a repository can only
//! ever create once: [`rift_git::chain::init_from_branch`] derives the order
//! from the checkpoint branch's own first-parent history — the only
//! append-stable witness that exists — and refuses outright if the ref is
//! already there. That refusal is why this is a verb a person types rather than
//! a step some routine command performs on the way to something else: creating
//! an append-only artifact is a decision, and a decision needs an author.
//!
//! Neither verb linearizes anything. A checkpoint branch carrying a merge has
//! no single write order, and *inventing* one is precisely the class of defect
//! (`prev` derived from a sort of content hashes) this ref exists to end. Such a
//! branch is reported and refused.

use std::path::Path;

use rift_core::{Result, RiftError};
use rift_git::chain;
use rift_shadow::backend::NATIVE_CHECKPOINT_REF as CHECKPOINT_BRANCH;

use crate::cli::{exit, Outcome};

/// The first position at which two orders disagree, and what each claims there.
///
/// Positions are **1-based**, the same spelling `chain_seq` uses in
/// `docs/sync-format.md`, so a diagnostic and a cursor talk about the same
/// number.
pub(crate) struct Disagreement {
    /// The 1-based position.
    pub(crate) position: usize,
    /// What the chain ref records at that position.
    pub(crate) chain: String,
    /// What the checkpoint branch's own history records there.
    pub(crate) branch: String,
}

impl std::fmt::Display for Disagreement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "position {}: the chain records {}, the checkpoint branch records {}",
            self.position, self.chain, self.branch
        )
    }
}

/// Where the recorded order and the branch's own history first part company,
/// over the positions they SHARE.
///
/// A length difference is deliberately not a disagreement: a chain shorter than
/// the branch is a gap that can still be healed at the tail, and a chain longer
/// than the branch is an orphan — two different findings with two different
/// answers, and collapsing them here would hide both.
pub(crate) fn first_disagreement(chain: &[String], branch: &[String]) -> Option<Disagreement> {
    chain
        .iter()
        .zip(branch.iter())
        .enumerate()
        .find(|(_, (c, b))| c != b)
        .map(|(i, (c, b))| Disagreement {
            position: i + 1,
            chain: c.clone(),
            branch: b.clone(),
        })
}

/// What the worker's chain maintenance did for one archived checkpoint.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// The checkpoint is already at a position; nothing to do. A re-drained
    /// queue entry lands here, which is what makes the step idempotent.
    AlreadyRecorded,
    /// Appended `entries` positions: the archived checkpoint, plus any tail gap
    /// left by an earlier archive whose entry never landed.
    Appended {
        /// How many entries this call wrote.
        entries: usize,
    },
    /// Refused: there is no chain ref and the branch already carries history.
    /// Creating one here would be an initializer running unattended over a
    /// backfill decision — `rift chain init` is where that is typed.
    NeedsInit {
        /// Checkpoints already on the branch.
        checkpoints: usize,
    },
    /// Refused: the recorded order and the branch's history do not agree, so
    /// appending would record a position that is not the write order.
    Diverged(String),
}

impl Step {
    /// The line to leave beside the queue entry, or `None` when there is
    /// nothing a human has to act on.
    ///
    /// Loud, never fatal: a chain-maintenance problem must not strand capture
    /// input, and it must not be swallowed either — an un-recorded checkpoint
    /// is exactly what `verify`'s 1:1 check will refuse on the next run.
    pub(crate) fn marker(&self) -> Option<String> {
        match self {
            Self::AlreadyRecorded | Self::Appended { entries: 0 | 1 } => None,
            // A tail gap was HEALED, and healing quietly is what let the gap
            // exist unnoticed in the first place. One entry is this archive's
            // own position and says nothing; more than one means an earlier
            // archive's entry never landed and this call caught up for it —
            // which is a fact about a checkpoint that spent time outside the
            // order, and the operator gets to know it happened.
            //
            // Said once per commit through the overwrite-by-key marker channel,
            // not once per healed position: the note is a state, not a log.
            Self::Appended { entries } => Some(format!(
                "the checkpoint order had a tail gap and it is now closed: {entries} positions \
                 were appended, {} of them for earlier checkpoint(s) whose entry never landed. \
                 Nothing was rewritten — the gap was at the END of the order, so the checkpoint \
                 branch's own history is what filled it.",
                entries.saturating_sub(1)
            )),
            Self::NeedsInit { checkpoints } => Some(format!(
                "the checkpoint order is not recorded: {checkpoints} checkpoint(s) on the branch \
                 and no {}. Run `rift chain init`, then `rift rebuild` — this worker will not \
                 create it, because backfilling an order from history is a decision with an \
                 author, and a projection that has fallen behind reads as TAMPER until the \
                 rebuild catches it up.",
                chain::CHAIN_REF
            )),
            Self::Diverged(detail) => Some(format!(
                "the chain entry was NOT recorded: {detail}. Nothing was appended; appending \
                 would have recorded a position that is not the write order."
            )),
        }
    }
}

/// Record an archived checkpoint's position in the chain, in the same worker op
/// as the archive.
///
/// Heals a **tail** gap on the way: if an earlier archive's entry never landed,
/// its checkpoint is appended here, in branch order, before this one. An
/// INTERIOR gap is refused instead — a checkpoint that belongs between two
/// recorded ones cannot be appended after them without recording a false order,
/// and append-only means that lie would be permanent.
///
/// Two directories, because they are two different facts. `git_dir` is what git
/// reads the chain through — the caller's own gitdir, an admin dir in a linked
/// worktree, which git resolves to the shared ref store for us. `common_dir` is
/// where the **push queue** lives: the refs this enqueues are the
/// family's, so the queue that names them is too. For a main work tree they are
/// the same directory, which is why one parameter hid the difference for as
/// long as it did.
///
/// # Errors
/// [`RiftError::Integrity`] if the checkpoint branch is not linear (it then has
/// no write order to record); substrate failures propagate.
pub(crate) fn record_archived(
    git_dir: &Path,
    common_dir: &Path,
    checkpoint_id: &str,
    now_ms: i64,
) -> Result<Step> {
    let order = chain::read_order(git_dir)?;
    if order.iter().any(|id| id == checkpoint_id) {
        return Ok(Step::AlreadyRecorded);
    }
    let branch = chain::branch_order(git_dir, CHECKPOINT_BRANCH)?;
    if let Some(d) = first_disagreement(&order, &branch) {
        return Ok(Step::Diverged(d.to_string()));
    }
    if order.len() > branch.len() {
        return Ok(Step::Diverged(format!(
            "the chain records {} position(s) and the branch carries {} checkpoint(s), so an \
             entry names a checkpoint the branch does not have",
            order.len(),
            branch.len()
        )));
    }
    let missing: Vec<String> = branch.get(order.len()..).unwrap_or_default().to_vec();
    if !missing.iter().any(|id| id == checkpoint_id) {
        return Ok(Step::Diverged(format!(
            "the archived checkpoint {checkpoint_id} is not on {CHECKPOINT_BRANCH}"
        )));
    }
    // No ref yet AND history behind it: that is a backfill, not a genesis.
    if order.is_empty() && missing.len() > 1 {
        return Ok(Step::NeedsInit {
            checkpoints: branch.len(),
        });
    }
    for id in &missing {
        chain::append(git_dir, id, now_ms / 1000)?;
    }
    enqueue_for_push(common_dir, order.len() + missing.len(), now_ms);
    Ok(Step::Appended {
        entries: missing.len(),
    })
}

/// Enqueue the two refs a checkpoint write moves, for a later shipper
/// **This is the enqueue-on-checkpoint-write site**, and it
/// is deliberately the chain append rather than the shard write: the chain
/// entry is what makes a checkpoint canonical, and enqueueing here means the
/// queue can never name a stream position the chain does not hold.
///
/// Best-effort and silent. Nothing about capture depends on it: the refs are
/// already durable in git, an entry carries no bytes, and the next checkpoint —
/// or `rift sync flush` — enqueues the same refs again. A queue that could fail
/// an archive would be a worse trade than a queue that occasionally misses a
/// line, so every error here is dropped on purpose.
fn enqueue_for_push(common_dir: &Path, chain_seq: usize, now_ms: i64) {
    let seq = u64::try_from(chain_seq).unwrap_or(u64::MAX);
    for ref_name in [chain::CHAIN_REF, CHECKPOINT_BRANCH] {
        let _ = crate::push_queue::enqueue(
            common_dir,
            &crate::push_queue::PushEntry {
                schema_version: crate::push_queue::PUSH_QUEUE_SCHEMA_VERSION,
                ref_name: ref_name.to_owned(),
                chain_seq: seq,
                enqueued_at_ms: now_ms,
            },
        );
    }
}

/// Resolve `root`'s gitdir, or the "not a repository" outcome.
fn git_dir_of(root: &Path, verb: &str) -> std::result::Result<std::path::PathBuf, Outcome> {
    match rift_git::discover(root) {
        Ok(repo) => Ok(repo.git_dir),
        Err(_) => Err(Outcome::coded(
            exit::NOT_INIT,
            format!("{verb}: not a git repository"),
            serde_json::json!({ "error": "not_a_repo" }),
        )),
    }
}

/// The head of the recorded order: `(entry oid, checkpoint id)`, both `None`
/// when the ref does not exist yet.
fn head(git_dir: &Path) -> Result<(Option<String>, Option<String>)> {
    let tip = chain::tip(git_dir)?;
    let last = chain::read_order(git_dir)?.pop();
    Ok((tip, last))
}

/// `rift chain init` — record the canonical order, once.
///
/// # Errors
/// Substrate failures propagate; the two refusals (ref present, non-linear
/// branch) are **outcomes**, not errors: each is a fact about the repository
/// the operator has to act on, and a rendered explanation with a non-zero exit
/// says more than a propagated error string.
pub(crate) fn cmd_init(root: &Path, now_ms: i64) -> Result<Outcome> {
    let git_dir = match git_dir_of(root, "chain init") {
        Ok(d) => d,
        Err(o) => return Ok(o),
    };
    // Whole-second git raw dates; one stamp for the whole backfill, so a run
    // repeated over the same branch produces byte-identical entries.
    let date = now_ms / 1000;
    match chain::init_from_branch(&git_dir, CHECKPOINT_BRANCH, date) {
        Ok(0) => Ok(nothing_to_record()),
        Ok(n) => recorded(&git_dir, n),
        // The ref is already there. Say what that means rather than echoing an
        // error type: a second init would have to move entries that are already
        // published, which is a rewrite wearing an initializer's name.
        Err(RiftError::Immutability(_)) => chain_exists(&git_dir),
        // A non-linear graph is reported, never linearized around.
        Err(RiftError::Integrity(detail)) => Ok(branch_not_linear(&detail)),
        Err(e) => Err(e),
    }
}

/// `chain init` over a checkpoint branch with no checkpoints: nothing written.
fn nothing_to_record() -> Outcome {
    Outcome::ok(
        format!(
            "chain init: no checkpoints on {CHECKPOINT_BRANCH} — nothing to record.\n  \
             The chain begins with the first archived checkpoint."
        ),
        serde_json::json!({
            "ref": chain::CHAIN_REF,
            "branch": CHECKPOINT_BRANCH,
            "entries": 0,
            "head_entry": serde_json::Value::Null,
            "head_checkpoint": serde_json::Value::Null,
        }),
    )
}

/// `chain init` wrote `n` entries: report the head it now records.
///
/// The follow-up is named, because init alone is not the end of the repair.
/// `rift verify` compares the projection against the order this just wrote; a
/// projection that is BEHIND the branch reads as TAMPER, not as "incomplete" —
/// so an operator who runs init on a repository whose projection was levelled,
/// and stops there, is handed a tamper verdict for a repository that is fine.
fn recorded(git_dir: &Path, n: usize) -> Result<Outcome> {
    let (entry, checkpoint) = head(git_dir)?;
    let human = format!(
        "chain init: {n} entries written to {}\n  head entry:      {}\n  head checkpoint: \
         {}\n  Next: run `rift rebuild`. Until the projection is caught up with the \
         branch, `rift verify` reports TAMPER — a projection behind its git is not \
         distinguishable from a rewritten one.",
        chain::CHAIN_REF,
        entry.clone().unwrap_or_else(|| "(unreadable)".into()),
        checkpoint.clone().unwrap_or_else(|| "(unreadable)".into()),
    );
    Ok(Outcome::ok(
        human,
        serde_json::json!({
            "ref": chain::CHAIN_REF,
            "branch": CHECKPOINT_BRANCH,
            "entries": n,
            "head_entry": entry,
            "head_checkpoint": checkpoint,
        }),
    ))
}

/// `chain init` refused because the chain ref already exists; nothing was
/// written, and the report says where the existing chain stands.
fn chain_exists(git_dir: &Path) -> Result<Outcome> {
    let order = chain::read_order(git_dir)?;
    let (entry, checkpoint) = head(git_dir)?;
    let human = format!(
        "chain init: refused — the chain ref already exists; a second init is a rewrite \
         wearing an initializer's name.\n  {} already records {} position(s); head \
         checkpoint {}.\n  Nothing was written. Newly archived checkpoints extend it on \
         their own; `rift chain status` shows where it stands.",
        chain::CHAIN_REF,
        order.len(),
        checkpoint.clone().unwrap_or_else(|| "(unreadable)".into()),
    );
    Ok(Outcome::coded(
        exit::INTEGRITY,
        human,
        serde_json::json!({
            "error": "chain_exists",
            "ref": chain::CHAIN_REF,
            "positions": order.len(),
            "head_entry": entry,
            "head_checkpoint": checkpoint,
        }),
    ))
}

/// `chain init` refused a checkpoint branch that forked and merged.
fn branch_not_linear(detail: &str) -> Outcome {
    Outcome::coded(
        exit::INTEGRITY,
        format!(
            "chain init: refused — {detail}.\n  The checkpoint branch's first-parent history \
             is the only witness to the write order, and a branch that forked and merged has \
             no single one. Nothing was written."
        ),
        serde_json::json!({
            "error": "branch_not_linear",
            "ref": chain::CHAIN_REF,
            "branch": CHECKPOINT_BRANCH,
            "detail": detail,
        }),
    )
}

/// `rift chain status` — read-only: how many positions the order records, and
/// its head.
///
/// # Errors
/// Substrate read failures propagate.
pub(crate) fn cmd_status(root: &Path) -> Result<Outcome> {
    let git_dir = match git_dir_of(root, "chain status") {
        Ok(d) => d,
        Err(o) => return Ok(o),
    };
    let order = chain::read_order(&git_dir)?;
    let (entry, checkpoint) = head(&git_dir)?;
    // Presence is the REF's, not the order's: a ref holding entries this reader
    // cannot parse exists and must not be reported as absent — "run init" would
    // then be advice that gets refused.
    let present = chain::exists(&git_dir);
    let human = if present {
        format!(
            "chain: {} positions in {}\n  head entry:      {}\n  head checkpoint: {}",
            order.len(),
            chain::CHAIN_REF,
            entry.clone().unwrap_or_else(|| "(unreadable)".into()),
            checkpoint.clone().unwrap_or_else(|| "(unreadable)".into()),
        )
    } else {
        format!(
            "chain: {} does not exist yet — no order is recorded.\n  `rift chain init` records it \
             from the checkpoint branch's own history.",
            chain::CHAIN_REF
        )
    };
    Ok(Outcome::ok(
        human,
        serde_json::json!({
            "ref": chain::CHAIN_REF,
            "exists": present,
            "positions": order.len(),
            "head_entry": entry,
            "head_checkpoint": checkpoint,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    /// Content-hash order `A < MID < B`; write order A, B, MID — the shape that
    /// forked the projection when a sort was mistaken for an order.
    const A: &str = "4c5c0a053e73b924663009e1602882b6073d3fdd10ad33aa92c988a643e040f3";
    const B: &str = "b970a014c1a3dd6508c5382679051a37f7cc1cd238b66543ecb1472a746d23c7";
    const MID: &str = "93ae336492a162b429cf27a5952d8d8f0a7a878eca243585d5bc33893d7d0d9a";

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// One checkpoint shard, one commit, on the checkpoint branch — the shape
    /// the native archive writes.
    fn archive(dir: &Path, id: &str) {
        let (hi, rest) = id.split_at(2);
        let shard = dir.join(hi).join(rest);
        std::fs::create_dir_all(&shard).unwrap();
        std::fs::write(shard.join("metadata.json"), r#"{"strategy":"x"}"#).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-qm", id]);
    }

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        git(d.path(), &["init", "-q", "-b", "rift/checkpoints/v1"]);
        d
    }

    const T: i64 = 1_700_000_000_000;

    #[test]
    fn the_first_checkpoint_of_a_repository_records_its_own_genesis() {
        let d = repo();
        let git_dir = d.path().join(".git");
        archive(d.path(), A);
        assert_eq!(
            record_archived(&git_dir, &git_dir, A, T).unwrap(),
            Step::Appended { entries: 1 }
        );
        assert_eq!(chain::read_order(&git_dir).unwrap(), vec![A.to_string()]);
    }

    /// TWO links, because one proves nothing about where the next one attaches.
    #[test]
    fn each_later_checkpoint_chains_on_the_head_rather_than_re_genesising() {
        let d = repo();
        let git_dir = d.path().join(".git");
        for id in [A, B, MID] {
            archive(d.path(), id);
            assert_eq!(
                record_archived(&git_dir, &git_dir, id, T).unwrap(),
                Step::Appended { entries: 1 }
            );
        }
        // Write order, not sorted order: MID sorts between A and B and is last.
        assert_eq!(
            chain::read_order(&git_dir).unwrap(),
            vec![A.to_string(), B.to_string(), MID.to_string()]
        );
        // Exactly one entry per checkpoint: no id can claim two positions.
        let mut seen = chain::read_order(&git_dir).unwrap();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 3);
    }

    /// A repository with history and no chain is a BACKFILL, and a backfill is
    /// typed by a person. The worker says so and writes nothing.
    #[test]
    fn history_with_no_chain_is_refused_rather_than_backfilled_unattended() {
        let d = repo();
        let git_dir = d.path().join(".git");
        for id in [A, B, MID] {
            archive(d.path(), id);
        }
        let step = record_archived(&git_dir, &git_dir, MID, T).unwrap();
        assert_eq!(step, Step::NeedsInit { checkpoints: 3 });
        assert!(step.marker().unwrap().contains("rift chain init"));
        assert!(!chain::exists(&git_dir), "nothing was created");
    }

    /// A gap at the TAIL is healed, in the branch's own order.
    #[test]
    fn a_tail_gap_is_healed_in_write_order_by_the_next_archive() {
        let d = repo();
        let git_dir = d.path().join(".git");
        archive(d.path(), A);
        record_archived(&git_dir, &git_dir, A, T).unwrap();
        archive(d.path(), B); // this archive's entry never landed
        archive(d.path(), MID);
        assert_eq!(
            record_archived(&git_dir, &git_dir, MID, T).unwrap(),
            Step::Appended { entries: 2 }
        );
        assert_eq!(
            chain::read_order(&git_dir).unwrap(),
            vec![A.to_string(), B.to_string(), MID.to_string()]
        );
    }

    /// **A heal SAYS it healed.** The tail gap closed silently, which is the
    /// same silence that let the gap open: a checkpoint spent time outside the
    /// order and the only trace was a number nobody printed. One appended
    /// position is this archive's own and says nothing; two or more means an
    /// earlier archive's entry never landed.
    #[test]
    fn a_tail_gap_heal_says_it_healed_and_an_ordinary_append_stays_quiet() {
        let d = repo();
        let git_dir = d.path().join(".git");

        // The ordinary case first: one position, nothing to report.
        archive(d.path(), A);
        let step = record_archived(&git_dir, &git_dir, A, T).unwrap();
        assert_eq!(step, Step::Appended { entries: 1 });
        assert!(
            step.marker().is_none(),
            "an ordinary append must not speak, or the note becomes noise on \
             every commit and stops meaning anything"
        );

        // Now the heal.
        archive(d.path(), B); // this archive's entry never landed
        archive(d.path(), MID);
        let step = record_archived(&git_dir, &git_dir, MID, T).unwrap();
        assert_eq!(step, Step::Appended { entries: 2 });
        let marker = step.marker().expect("a heal is said out loud");
        assert!(marker.contains("tail gap"), "names what happened: {marker}");
        assert!(
            marker.contains("1 of them for earlier"),
            "and how many positions it caught up: {marker}"
        );
        assert!(
            marker.contains("Nothing was rewritten"),
            "and that append-only held: {marker}"
        );
    }

    /// Both messages an operator acts on name `rift rebuild`, because `chain
    /// init` alone leaves a projection that `verify` calls TAMPER — see
    /// `a_projection_behind_its_branch_is_reported_as_tamper_not_incomplete`
    /// in `verify`, which is the behaviour these sentences describe.
    #[test]
    fn the_order_repair_messages_name_the_rebuild_that_finishes_the_job() {
        let d = repo();
        let git_dir = d.path().join(".git");
        for id in [A, B, MID] {
            archive(d.path(), id);
        }
        let marker = record_archived(&git_dir, &git_dir, MID, T)
            .unwrap()
            .marker()
            .expect("a repository with history and no chain is told what to do");
        assert!(marker.contains("rift chain init"), "{marker}");
        assert!(marker.contains("rift rebuild"), "the follow-up: {marker}");
        assert!(marker.contains("TAMPER"), "and why it matters: {marker}");

        let human = cmd_init(d.path(), T).unwrap().human;
        assert!(human.contains("rift rebuild"), "init says it too: {human}");
        assert!(human.contains("TAMPER"), "and why: {human}");
    }

    /// An INTERIOR gap cannot be healed: appending the missing checkpoint after
    /// the ones that followed it would record a false order, permanently.
    #[test]
    fn an_interior_gap_is_refused_and_names_both_claims() {
        let d = repo();
        let git_dir = d.path().join(".git");
        archive(d.path(), A);
        archive(d.path(), B);
        archive(d.path(), MID);
        // The chain skipped B: positions are A, MID; the branch says A, B, MID.
        chain::append(&git_dir, A, T / 1000).unwrap();
        chain::append(&git_dir, MID, T / 1000).unwrap();
        let tip_before = chain::tip(&git_dir).unwrap();

        let step = record_archived(&git_dir, &git_dir, B, T).unwrap();
        let Step::Diverged(detail) = &step else {
            panic!("expected a refusal, got {step:?}")
        };
        assert!(detail.contains("position 2"), "{detail}");
        assert!(detail.contains(MID), "what the chain claims: {detail}");
        assert!(detail.contains(B), "what the branch claims: {detail}");
        assert!(step.marker().unwrap().contains("NOT recorded"));
        assert_eq!(chain::tip(&git_dir).unwrap(), tip_before, "the ref moved");
    }

    #[test]
    fn a_checkpoint_already_at_a_position_is_a_no_op() {
        let d = repo();
        let git_dir = d.path().join(".git");
        archive(d.path(), A);
        record_archived(&git_dir, &git_dir, A, T).unwrap();
        let tip_before = chain::tip(&git_dir).unwrap();
        assert_eq!(
            record_archived(&git_dir, &git_dir, A, T).unwrap(),
            Step::AlreadyRecorded
        );
        assert_eq!(chain::tip(&git_dir).unwrap(), tip_before);
        assert!(record_archived(&git_dir, &git_dir, A, T)
            .unwrap()
            .marker()
            .is_none());
    }

    /// A chain longer than the branch means an entry names a checkpoint that is
    /// not there — reported, never healed by appending more.
    #[test]
    fn an_entry_the_branch_cannot_account_for_stops_the_append() {
        let d = repo();
        let git_dir = d.path().join(".git");
        archive(d.path(), A);
        chain::append(&git_dir, A, T / 1000).unwrap();
        chain::append(&git_dir, B, T / 1000).unwrap(); // never archived
        let step = record_archived(&git_dir, &git_dir, MID, T).unwrap();
        let Step::Diverged(detail) = &step else {
            panic!("expected a refusal, got {step:?}")
        };
        assert!(detail.contains("does not have"), "{detail}");
    }
}
