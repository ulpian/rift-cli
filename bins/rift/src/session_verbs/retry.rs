//! `rift session retry [--session <id> | --all]`: the per-session retry of
//! parked work.
//!
//! A job that failed `PARK_AFTER` times is **parked**: kept in the journal
//! with its backoff and its note, run by no ordinary worker pass, and named
//! by report-only `rift doctor`. Two roads retry it — `rift doctor --fix`,
//! the whole-family repair, and this verb, which names one session (or every
//! parked job with `--all`): it resets `parked` and `next_due_ms` on the
//! session's jobs under the member owner, **keeping `attempts`** so the
//! history of failures is not erased, then runs one unconditional drain.

use std::path::Path;

use rift_core::Result;

use super::safe;
use crate::cli::{exit, Outcome};

/// What one retry did: the jobs it unparked and the condenses that landed.
#[derive(Debug, Default, PartialEq, Eq)]
struct Retried {
    unparked: Vec<String>,
    landed: Vec<(String, String)>,
}

/// The verb. `session` names the one session whose jobs are retried; `all`
/// retries every parked job of the member. Neither is a refusal, because a
/// retry that names nothing would otherwise silently retry everything.
pub(crate) fn retry(root: &Path, session: Option<&str>, all: bool) -> Result<Outcome> {
    let repo = rift_git::discover(root)?;
    let Some(target) = RetryTarget::of(session, all) else {
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            "rift session retry needs --session <id> or --all",
            serde_json::json!({ "error": "no_target" }),
        ));
    };
    let unparked = match unpark(&repo.git_dir, target) {
        Ok(unparked) => unparked,
        Err(block) => {
            return Ok(Outcome::coded(
                exit::PRECONDITION,
                format!("rift session retry: the member's queue is {block}"),
                serde_json::json!({ "error": "queue_unavailable", "reason": block }),
            ))
        }
    };
    let landed = crate::hooks::drain_everything(&repo.git_dir);
    Ok(render(&Retried { unparked, landed }, target))
}

/// Whose jobs a retry unparks.
#[derive(Clone, Copy)]
enum RetryTarget<'a> {
    Session(&'a str),
    All,
}

impl<'a> RetryTarget<'a> {
    fn of(session: Option<&'a str>, all: bool) -> Option<Self> {
        match (session, all) {
            (Some(id), _) => Some(Self::Session(id)),
            (None, true) => Some(Self::All),
            (None, false) => None,
        }
    }

    fn admits(self, job_session: Option<&str>) -> bool {
        match self {
            Self::All => true,
            Self::Session(id) => job_session == Some(id),
        }
    }
}

/// Reset the parked jobs the target names, under the member owner; the
/// sessions whose jobs were reset. A held or legacy-blocked queue is reported,
/// not waited on: a worker holding it is retrying already.
fn unpark(git_dir: &Path, target: RetryTarget<'_>) -> std::result::Result<Vec<String>, String> {
    let owner = crate::capture_queue::acquire_owner(git_dir).map_err(|block| match block {
        crate::capture_queue::OwnerBlock::ActiveOwner => "held by a running worker".to_owned(),
        crate::capture_queue::OwnerBlock::LegacyBarrier => "blocked by a legacy marker".to_owned(),
        crate::capture_queue::OwnerBlock::Unavailable => "unavailable".to_owned(),
    })?;
    let mut unparked = Vec::new();
    for (path, job, state) in crate::agent_queue::retry_states(git_dir) {
        if !state.parked || !target.admits(job.kind.session_id()) {
            continue;
        }
        if crate::agent_queue::unpark_owned(&path, &job, &owner).is_ok() {
            unparked.push(job.kind.session_id().unwrap_or("agent-job").to_owned());
        }
    }
    unparked.sort();
    unparked.dedup();
    Ok(unparked)
}

fn render(retried: &Retried, target: RetryTarget<'_>) -> Outcome {
    let mut human = String::new();
    for session in &retried.unparked {
        human.push_str(&format!("unparked the jobs of session {}\n", safe(session)));
    }
    for (session, checkpoint) in &retried.landed {
        human.push_str(&format!(
            "condensed session {} to checkpoint {}\n",
            safe(session),
            super::short(checkpoint)
        ));
    }
    if human.is_empty() {
        human = match target {
            RetryTarget::Session(id) => {
                format!("session {} has no parked job; nothing to retry", safe(id))
            }
            RetryTarget::All => "no parked job; nothing to retry".to_owned(),
        };
    }
    Outcome::ok(
        human.trim_end().to_owned(),
        serde_json::json!({
            "unparked": retried.unparked,
            "landed": retried
                .landed
                .iter()
                .map(|(session, checkpoint)| serde_json::json!({ "session": session, "checkpoint": checkpoint }))
                .collect::<Vec<_>>(),
        }),
    )
}
