//! `rift recall …`: the agent-facing serving plane plus Recall's
//! terminal-owned lifecycle (`init`/`status`/`disable`).
//!
//! The skill verbs are the **CLI spelling of the one bounded surface**:
//! argv is rendered into the dispatcher's params object and decoded by the same
//! [`rift_recall_serve::decode_verb`] → [`decode_request`] path, then dispatched
//! through the same policy gates, per-turn budget, and accounting seam. Each
//! call keys its own neutral serve fact by a per-invocation id, so two serves
//! returning identical figures are still two rows. The audit is best-effort: a
//! serve already made is never retracted because its row could not be written.
//!
//! Division of labour with `rift memory …`: that surface answers for a *human*
//! and supplies the known state itself (it hashes the worktree for you); this
//! one answers for an *agent*, which states what it already holds via
//! `--known-hash`. Nothing is inferred on the agent's behalf.
//!
//! [`decode_request`]: rift_recall_serve::decode_request
//!
//! The four concerns are four modules: [`verb`] renders argv into the
//! dispatcher's params object and decides what the caller asked to widen;
//! [`document`] and [`search`] resolve the two verbs whose answer is not in
//! the projection; [`serve`](mod@serve) runs one verb through the bounded
//! surface and books it; and [`status`] and [`policy`] render the report this
//! file's three terminal-owned subcommands print.

mod document;
mod policy;
mod search;
mod serve;
mod status;
mod verb;

use std::path::Path;

use crate::cli::{Outcome, RecallCmd};

pub(crate) use self::policy::family_capability;

use self::policy::{policy_json, render_policy, render_savings_shares};
pub(crate) use self::status::{integration_rows, is_wired, render_status};

use self::serve::serve;
use self::status::{
    integration_status_json, observed_boundary_responses, render_support, support_assessments,
};

pub(crate) fn run(cmd: &RecallCmd, repo: &Path) -> rift_core::Result<Outcome> {
    match cmd {
        RecallCmd::Init => {
            let report =
                crate::features::recall_install(repo).map_err(rift_core::RiftError::storage)?;
            let statuses = rift_agent_install::native::diagnose_all(repo);
            let mut human = format!(
                "Recall initialized: {} native integrations, {} files changed\n",
                report.integrations.len(),
                report.changed_files.len()
            );
            render_status(&mut human, &statuses);
            Ok(Outcome::ok(
                human.trim_end().to_owned(),
                serde_json::json!({
                    "initialized": true,
                    "changed_files": report.changed_files,
                    "integrations": statuses,
                }),
            ))
        }
        RecallCmd::Status => {
            let statuses = rift_agent_install::native::diagnose_all(repo);
            let clients = rift_agent_install::diagnose(repo, false);
            let boundary_responses = observed_boundary_responses(repo);
            let mut human = String::from("Recall native integrations\n");
            render_status(&mut human, &statuses);
            let policy = rift_recall_policy::load_serve_policy(&repo.join(".rift"));
            let assessments =
                support_assessments(repo, &statuses, &clients, &boundary_responses, &policy);
            render_support(&mut human, &assessments);
            // The same block `rift doctor` prints, between the tables it is
            // about and the policy the repository asked for.
            let (health, health_json) = crate::doctor::health::status_health_block(repo);
            human.push_str(&health);
            human.push('\n');
            render_policy(&mut human, &policy);
            // The figure this plane is judged by, on the table that says
            // whether it is switched on: a mode line with no measurement
            // beside it is the reason the two were ever read apart.
            let savings = crate::memory::savings_report(repo)?;
            render_savings_shares(&mut human, &savings);
            Ok(Outcome::ok(
                human.trim_end().to_owned(),
                serde_json::json!({
                    "integrations": integration_status_json(&statuses, &assessments),
                    "health": health_json,
                    "policy": policy_json(&policy),
                    "savings": crate::memory::savings_json(&savings),
                }),
            ))
        }
        RecallCmd::Disable => {
            let changed =
                crate::features::recall_remove(repo).map_err(rift_core::RiftError::storage)?;
            Ok(Outcome::ok(
                format!(
                    "Recall disabled; removed Rift-owned entries from {} files (data retained)",
                    changed.len()
                ),
                serde_json::json!({
                    "disabled": true,
                    "changed_files": changed,
                    "data_retained": true,
                }),
            ))
        }
        skill => serve(skill, repo),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;
    use rift_recall_serve::ToolName;

    /// Minimal satisfying argv per verb: ranked verbs need the query-aware
    /// params, target verbs a target, retrieve-exact its mandatory reason.
    pub(super) fn argv_for(t: ToolName) -> Vec<&'static str> {
        match t {
            ToolName::KnownState | ToolName::ContextPack => {
                vec![t.verb_name(), "--task", "t", "--file", "a.rs"]
            }
            // The shape the manifest's hint teaches: a path and the revision
            // the caller holds, and no ranking params at all.
            ToolName::ChangedHunks => {
                vec![t.verb_name(), "a.rs", "--known-hash", "aaaaaaaaaaaaaaaa"]
            }
            ToolName::DiffSince | ToolName::CostHotspots => vec![t.verb_name()],
            ToolName::FileIfChanged | ToolName::ReadSummary => vec![t.verb_name(), "a.rs"],
            ToolName::RetrieveExact => {
                vec![t.verb_name(), "file:a.rs@sha256:aa", "--reason", "why"]
            }
            // A path, and nothing else: the document verb takes no flags, and
            // what it needs beyond the path is resolved rather than typed.
            ToolName::DocRead => vec![t.verb_name(), "spec.docx"],
            // A query, and nothing else that is required: which paths matched
            // and what the lines in them say is resolved, not typed.
            ToolName::Search => vec![t.verb_name(), "read gate"],
        }
    }

    /// Parse a full `rift recall …` argv and hand back the `RecallCmd`.
    pub(super) fn parse(argv: &[&str]) -> RecallCmd {
        let mut full = vec!["rift", "recall"];
        full.extend_from_slice(argv);
        match crate::cli::Cli::try_parse_from(full)
            .expect("argv parses")
            .cmd
        {
            Some(crate::cli::Command::Recall(r)) => r,
            other => panic!("expected a recall subcommand, got {other:?}"),
        }
    }
}
