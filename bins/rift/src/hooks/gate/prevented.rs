//! The read an announcement made unnecessary.
//!
//! Separate because it is the one booking that rests on something NOT
//! happening, and every clause of it is a way that booking could have been
//! wrong.

use std::path::Path;

use rift_shadow::hooks::AdapterRegistry;

use super::answer::AUDIT_IDS;
use super::file::{gate_file, gate_rel_path, payload_cwd};
use super::{gate_step, gate_turn, GateStep};

/// The announcement one act answered, with the bytes it was made about.
struct AnsweredHold {
    /// This checkout's own spelling of the file.
    rel_path: String,
    /// The same bytes as git names them, for the worker to price.
    blob_oid: String,
    /// The run of lines this window was told it holds.
    hold: rift_agent::window_journal::StandingHold,
}

/// **Book the read an announcement made unnecessary**, once per announcement.
///
/// Rift pushes statements about held state into a context window — a delta
/// manifest entry, a recovery map entry — and the value of one is the read it
/// replaces. Until now nothing observed that read *not* happening, so the
/// whole of what those frames are worth was outside every figure Rift
/// publishes. This is the observation, and it is deliberately narrow: after
/// Rift has said "you hold this file", the window **acts on that file** —
/// changes it, or searches inside it — without asking for it again.
///
/// Both halves of the evidence are things a hook sees. A citation of the file
/// in the model's own prose is not one of them: no family reports prose to a
/// hook, so nothing here can know it happened and nothing here pretends to.
///
/// Every clause below is a way the booking could have been wrong:
///
/// * the lever is off, or the hook names no window — nothing is decided;
/// * the call is not the moment a call is about to run, so an announcement
///   is not answered twice by one act;
/// * the payload names no file this checkout has a name for;
/// * **nothing has been announced about that file**, or something has
///   answered the announcement since — a read (the bytes came back), a
///   decline (the intercept already books that slice, and booking here too
///   would count one saving twice), an earlier booking, or an emptying;
/// * the file no longer hashes to the revision that was announced, so what
///   the window holds is not what is there and no read of it was avoided.
///
/// It opens no store and counts no tokens: the announced lines are named to
/// the detached worker, which reads exactly those bytes and prices them with
/// the real tokenizer, exactly as a decline's slice is priced.
///
/// Returns whether the worker was left a row.
pub(super) fn prevented_record(
    registry: &AdapterRegistry,
    raw: &rift_shadow::hooks::RawHook,
    git_dir: &Path,
    work_tree: &Path,
    effective: &rift_recall_policy::Effective,
) -> bool {
    if effective.read_deny == rift_recall_policy::Mode::Off || raw.session.is_empty() {
        return false;
    }
    let Some(adapter) = registry.get(&raw.agent) else {
        return false;
    };
    if !matches!(
        gate_step(&raw.agent, &raw.verb, &raw.payload),
        Some(GateStep::Decide)
    ) {
        return false;
    }
    // An edit of the file, or a content search inside it. Both are acts on a
    // file whose bytes the window is not asking for, and both are named by a
    // pinned surface rather than inferred from a shape.
    let Some(named) = adapter
        .edit_request(&raw.verb, &raw.payload)
        .map(|request| request.path)
        .or_else(|| adapter.searched_file(&raw.verb, &raw.payload))
    else {
        return false;
    };
    let facts = adapter.facts(&raw.verb, &raw.payload);
    let cwd = payload_cwd(&facts);
    // The context window, not the session — the same key everything else on
    // this path is written under.
    let journal = crate::window_journal::WindowJournal::open(
        work_tree,
        &raw.session,
        facts.child_session.as_deref(),
    );
    let Some(answered) =
        answered_announcement(work_tree, &named, cwd.as_deref(), &journal, effective)
    else {
        return false;
    };
    let turn = gate_turn(raw, git_dir);
    let at_ms = u64::try_from(raw.occurred_at_ms).unwrap_or_default();
    // The line first, and the row only if it landed. The line is what bounds
    // the booking to one per announcement, so a row written beside a line that
    // could not be written is a figure a second act would book again.
    if journal
        .append(&rift_agent::window_journal::Line::Prevented {
            turn,
            path: answered.rel_path.clone(),
            hash: answered.hold.hash,
            range: answered.hold.range,
            at_ms,
        })
        .is_err()
    {
        return false;
    }
    journal_prevented(
        git_dir,
        work_tree,
        raw,
        &answered.rel_path,
        &answered.blob_oid,
        answered.hold,
        effective,
    )
}

/// What this window was told it holds of the file an act just named, or
/// `None` where there is nothing for the act to have answered.
///
/// The journal is consulted BEFORE the file is hashed: the overwhelming
/// majority of edits are of files nothing was announced about, and those must
/// cost one small file read and no hash at all. The hash is the last clause,
/// and the strictest — a file that no longer hashes to the revision that was
/// announced is not what the window holds, so no read of it was avoided.
fn answered_announcement(
    work_tree: &Path,
    named: &str,
    cwd: Option<&Path>,
    journal: &crate::window_journal::WindowJournal,
    effective: &rift_recall_policy::Effective,
) -> Option<AnsweredHold> {
    let root = work_tree.canonicalize().ok()?;
    let rel_path = gate_rel_path(&root, named, cwd)?;
    let hold = journal.load().journal.standing_hold(&rel_path)?;
    let file = gate_file(work_tree, named, cwd, effective.max_inline_hash_bytes)?;
    if file.hash != hold.hash {
        return None;
    }
    Some(AnsweredHold {
        rel_path,
        blob_oid: file.blob_oid,
        hold,
    })
}

/// Journal what one prevented read owes the worker, and say whether it left
/// the worker anything.
///
/// `lines` is the announced run, or `None` where the announcement was about
/// the whole file — the two are the same distinction
/// [`rift_agent::window_journal::Interval::is_whole_file`] draws, and the whole
/// file is priced as the whole file rather than as four billion lines.
fn journal_prevented(
    git_dir: &Path,
    work_tree: &Path,
    raw: &rift_shadow::hooks::RawHook,
    rel_path: &str,
    blob_oid: &str,
    hold: rift_agent::window_journal::StandingHold,
    effective: &rift_recall_policy::Effective,
) -> bool {
    use crate::agent_queue::{AuditBasis, AuditLines, ServeAudit, ServeAuditKind};
    let decision = if effective.read_deny == rift_recall_policy::Mode::Enforce {
        ServeAuditKind::ReadPrevented
    } else {
        ServeAuditKind::ReadPreventedObserved
    };
    crate::agent_queue::enqueue(
        git_dir,
        &crate::agent_queue::AgentJob {
            schema_version: 1,
            work_tree: work_tree.to_path_buf(),
            queued_at_ms: raw.occurred_at_ms,
            attempts: 0,
            kind: crate::agent_queue::JobKind::ServeAudit(ServeAudit {
                call_id: {
                    use rift_core::UlidSource as _;
                    AUDIT_IDS.next_ulid().to_hex()
                },
                decision,
                agent: raw.agent.as_str().to_owned(),
                basis: Some(AuditBasis {
                    rel_path: rel_path.to_owned(),
                    content_sha256: hold.hash.to_hex(),
                    blob_oid: blob_oid.to_owned(),
                    lines: (!hold.range.is_whole_file()).then_some(AuditLines {
                        lo: hold.range.lo,
                        hi: hold.range.hi,
                    }),
                }),
                // Nothing was put in front of the model: the frame that made
                // this read unnecessary is charged where it was injected.
                frame: String::new(),
                tag: None,
                total: None,
                declared_avoided: None,
                shape: None,
                session: Some(raw.session.clone()),
            }),
        },
    )
    .is_ok()
}
