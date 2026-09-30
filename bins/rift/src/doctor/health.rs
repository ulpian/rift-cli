//! Integration health as the command line prints it: the one gathering both
//! `rift doctor` and `rift status` call, so the two can never disagree about
//! what is wrong or what to do about it.
//!
//! The facts are read here, at the edge, with the wall clock; the findings and
//! their sentences come from `rift_agent_install::health`. Nothing here writes:
//! the projection is opened read-only and only when it exists, and every other
//! input is a file parsed in place. `--fix` does not reach this leg.

use std::path::Path;

use rift_agent_install::detect::DetectionEnvironment;
use rift_agent_install::health::{self, Facts, Reason, Remedy};
use rift_shadow::hooks::session_cache::SessionCache;

use crate::cli::Outcome;
use crate::{hooks, session_verbs};

/// The doctor leg: a line per finding, or `none`; the `--json` array is the
/// one `rift status` emits under `health`. `null` outside a repository, where
/// no fact can be read, so a script does not read "healthy" into it.
pub(super) fn doctor_integration_health(root: &Path) -> (String, serde_json::Value) {
    let Some(remedies) = integration_remedies(root) else {
        return (
            "\n  integration health: unreadable (not a repository)".to_owned(),
            serde_json::Value::Null,
        );
    };
    let human = health_lines(&remedies, |remedy| remedy.sentence.clone());
    (human, remedies_json(&remedies))
}

/// `outcome` with the health block appended, for `rift status`: the lines
/// after the Codex trust lines and the array under `health`.
pub(crate) fn with_integration_health(root: &Path, mut outcome: Outcome) -> Outcome {
    let (human, json) = status_health_block(root);
    outcome.human.push_str(&human);
    if let Some(object) = outcome.json.as_object_mut() {
        object.insert("health".to_owned(), json);
    }
    outcome
}

/// The block a surface that already printed the Codex trust lines shows —
/// `rift status` and `rift recall status` — and the array it carries under
/// `health`, identical to the doctor's.
///
/// Codex's skipped-hooks sentence is already printed with the trust lines
/// above, so its entry here points at them rather than printing it twice; the
/// `--json` element still carries the full sentence.
pub(crate) fn status_health_block(root: &Path) -> (String, serde_json::Value) {
    let remedies = integration_remedies(root).unwrap_or_default();
    (
        health_lines(&remedies, status_sentence),
        remedies_json(&remedies),
    )
}

/// Every finding for the repository at `root`, each with its remedy on the
/// surface its agent runs on. `None` when `root` is not in a repository.
fn integration_remedies(root: &Path) -> Option<Vec<Remedy>> {
    let paths = rift_git::discover(root).ok()?;
    let sessions: Vec<SessionCache> = session_verbs::family_journal(&paths)
        .into_iter()
        .flat_map(|journal| journal.sessions)
        .collect();
    let projection = crate::projection::open_projection_read_only(root).ok();
    let family_dir = hooks::family_sites(&paths.common_dir).dir;
    let codex_home = rift_agent_install::native::codex_home_from_process();
    let facts = Facts::gather(
        root,
        &family_dir,
        codex_home.as_deref(),
        &sessions,
        projection.as_ref(),
    );
    let env = DetectionEnvironment::from_process();
    let remedies = health::assess(&facts, hooks::now_ms())
        .iter()
        .map(|finding| {
            let (surface, _) = health::client_surface(&finding.agent, &sessions, &env);
            health::remedy(finding, surface, facts.codex_trust.as_ref())
        })
        .collect();
    Some(remedies)
}

/// The block: `none`, or a count and, per finding, its severity and headline
/// with the sentence `sentence_of` gives indented under it.
fn health_lines(remedies: &[Remedy], sentence_of: impl Fn(&Remedy) -> String) -> String {
    if remedies.is_empty() {
        return "\n  integration health: none".to_owned();
    }
    let mut lines = format!("\n  integration health: {} finding(s)", remedies.len());
    for remedy in remedies {
        lines.push_str(&format!(
            "\n    {}: {}\n      {}",
            remedy.severity.as_str(),
            remedy.headline,
            sentence_of(remedy)
        ));
    }
    lines
}

/// The sentence `rift status` prints: the remedy's own, but for Codex's
/// skipped hooks, whose sentence the trust lines above already carry.
fn status_sentence(remedy: &Remedy) -> String {
    match remedy.reason {
        Reason::CodexHooksSkipped => "what to do is under the codex trust line above".to_owned(),
        _ => remedy.sentence.clone(),
    }
}

/// The `--json` array both commands emit.
fn remedies_json(remedies: &[Remedy]) -> serde_json::Value {
    serde_json::to_value(remedies).unwrap_or_else(|_| serde_json::json!([]))
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;
    use rift_agent_install::detect::NativeIdeHost;
    use rift_agent_install::health::{Finding, Surface};
    use rift_agent_install::native::{CodexHookTrust, CodexHookVerdict, CodexTrust};

    use super::*;
    use crate::cli::Cli;

    /// Every `rift …` command a sentence names in backticks, split into argv.
    fn rift_commands(sentence: &str) -> Vec<Vec<String>> {
        sentence
            .split('`')
            .skip(1)
            .step_by(2)
            .filter(|span| span.starts_with("rift "))
            .map(|span| span.split_whitespace().map(str::to_owned).collect())
            .collect()
    }

    #[test]
    fn every_command_a_remedy_names_parses_as_a_rift_command() {
        let trust = CodexTrust {
            hooks: vec![CodexHookTrust {
                event: "SessionStart".to_owned(),
                verdict: CodexHookVerdict::Modified,
            }],
            client: None,
        };
        let surfaces = [
            Surface::Ide(NativeIdeHost::VsCode),
            Surface::Terminal,
            Surface::JetBrains,
            Surface::Unknown,
        ];
        let mut named = 0;
        for reason in Reason::ALL {
            for agent in ["claude-code", "codex", "antigravity"] {
                for surface in surfaces {
                    let finding = Finding::new(reason, agent, None, "detail".to_owned());
                    let remedy = health::remedy(&finding, surface, Some(&trust));
                    for argv in rift_commands(&remedy.sentence) {
                        named += 1;
                        assert!(Cli::try_parse_from(&argv).is_ok(), "{argv:?}");
                    }
                }
            }
        }
        assert!(named > 0, "the remedies name at least one command");
    }

    #[test]
    fn status_points_at_the_trust_line_instead_of_repeating_codexs_sentence() {
        let finding = Finding::new(Reason::CodexHooksSkipped, "codex", None, "Stop".to_owned());
        let remedy = health::remedy(&finding, Surface::Terminal, None);
        let status = health_lines(std::slice::from_ref(&remedy), status_sentence);
        assert!(!status.contains(&remedy.sentence), "{status}");
        assert!(status.contains("codex trust line above"), "{status}");
        let doctor = health_lines(std::slice::from_ref(&remedy), |r| r.sentence.clone());
        assert!(doctor.contains(&remedy.sentence), "{doctor}");
    }
}
