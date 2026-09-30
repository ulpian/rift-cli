//! The native-integration table of `rift recall status`, and the support
//! tier each row may be relied on for.
//!
//! Separate from the policy report beside it because it answers a different
//! question: this one is about what is wired and observed, that one about
//! what the repository asked for and what its families' pins allow.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use serde_json::Value;

use super::policy::family_capability;

/// The table itself, which `rift doctor`'s integrations leg renders too, so
/// the two surfaces cannot word a row differently.
pub(crate) fn render_status(
    out: &mut String,
    statuses: &[rift_agent_install::native::NativeIntegrationStatus],
) {
    let _ = writeln!(
        out,
        "  {:<16} {:<12} {:<12} installed",
        "agent", "descriptor", "hooks"
    );
    for status in statuses {
        // The availability sentence is the installer's own, so this table,
        // doctor and Project Settings name the same evidence for the same row.
        let _ = writeln!(
            out,
            "  {:<16} {:<12} {:<12} {}",
            status.display_name,
            state_name(&status.wiring),
            hooks_word(status),
            status.availability(),
        );
    }
    render_codex_trust(out, statuses);
}

/// Whether the row reads wired in the table: a managed descriptor, and hooks
/// that read `managed` or do not apply. A Codex hook file Codex is skipping
/// reads `error`, so it is written and still not counted.
pub(crate) fn is_wired(status: &rift_agent_install::native::NativeIntegrationStatus) -> bool {
    status.wiring == rift_agent_install::native::NativeWiring::Managed
        && matches!(hooks_word(status).as_str(), "managed" | "-")
}

/// The rows `rift recall status --json` emits under `integrations`, for a
/// caller that renders the table without the support block beside it.
pub(crate) fn integration_rows(
    repo: &Path,
    statuses: &[rift_agent_install::native::NativeIntegrationStatus],
) -> Vec<Value> {
    let clients = rift_agent_install::diagnose(repo, false);
    let boundary_responses = observed_boundary_responses(repo);
    let policy = rift_recall_policy::load_serve_policy(&repo.join(".rift"));
    let assessments = support_assessments(repo, statuses, &clients, &boundary_responses, &policy);
    integration_status_json(statuses, &assessments)
}

/// The hooks column: the installer's state, except that a Codex hook file
/// whose hooks Codex is skipping reads `error` — written correctly and still
/// not running is not `managed` in any sense a reader can use.
fn hooks_word(status: &rift_agent_install::native::NativeIntegrationStatus) -> String {
    let skipped = status
        .codex_trust
        .as_ref()
        .is_some_and(|trust| trust.remedy().is_some());
    if skipped {
        return "error".to_owned();
    }
    status
        .hook_wiring
        .as_ref()
        .map_or_else(|| "-".to_owned(), state_name)
}

/// Codex's per-hook trust under the table, and the remedy when it is needed.
fn render_codex_trust(
    out: &mut String,
    statuses: &[rift_agent_install::native::NativeIntegrationStatus],
) {
    for trust in statuses
        .iter()
        .filter_map(|status| status.codex_trust.as_ref())
    {
        let _ = writeln!(out, "  codex trust: {}", trust.summary());
        if let Some(remedy) = trust.remedy() {
            let _ = writeln!(out, "  {remedy}");
        }
    }
}

/// The support tier an operator may rely on. `observe` is deliberately not a
/// tier: it is a requested policy mode, reported separately below.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
enum Support {
    Full,
    ConfiguredUnverified,
    Degraded,
    ComingSoon,
}

/// Facts added to each native integration row. The descriptor's state remains
/// in the existing `wiring` field; these fields add the other independent
/// axes without breaking callers that already consume the native diagnostic.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(super) struct SupportAssessment {
    agent: String,
    support: Support,
    hook_configuration: String,
    runtime_connection: &'static str,
    context_delivery: &'static str,
    requested_mode: &'static str,
    effective_mode: &'static str,
    degraded_reason: Option<String>,
}

#[derive(Clone)]
struct SupportFacts {
    alpha: bool,
    descriptor: rift_agent_install::native::NativeWiring,
    hooks: Option<rift_agent_install::native::NativeHookWiring>,
    runtime_receipt: bool,
    boundary_response: bool,
    codex_trust: Option<rift_agent_install::native::CodexTrust>,
    hook_failure: Option<String>,
}

fn derive_support(facts: SupportFacts) -> (Support, Option<String>) {
    use rift_agent_install::native::{NativeHookWiring, NativeWiring};

    if !facts.alpha {
        return (Support::ComingSoon, None);
    }
    let descriptor_problem = match facts.descriptor {
        NativeWiring::Managed => None,
        NativeWiring::Missing => Some("Rift descriptor is missing"),
        NativeWiring::Conflict => Some("Rift descriptor conflicts with an existing file"),
        NativeWiring::Invalid => Some("Rift descriptor is malformed"),
    };
    if let Some(reason) = descriptor_problem {
        return (Support::Degraded, Some(reason.to_owned()));
    }
    let hook_problem = match facts.hooks {
        Some(NativeHookWiring::Managed) => None,
        Some(NativeHookWiring::Missing) | None => Some("required native hook is missing"),
        Some(NativeHookWiring::Stale) => Some("Rift native hook bundle is stale"),
        Some(NativeHookWiring::Conflict) => Some("Rift native hook bundle conflicts"),
        Some(NativeHookWiring::Invalid) => Some("native hook configuration is malformed"),
    };
    if let Some(reason) = hook_problem {
        return (Support::Degraded, Some(reason.to_owned()));
    }
    if let Some(reason) = facts.hook_failure {
        return (Support::Degraded, Some(reason));
    }
    if let Some(trust) = &facts.codex_trust {
        match trust.state() {
            "error" => return (Support::Degraded, Some(trust.summary())),
            "unverifiable" => return (Support::ConfiguredUnverified, Some(trust.summary())),
            _ => (),
        }
    }
    if !facts.runtime_receipt {
        return (
            Support::ConfiguredUnverified,
            Some("no runtime hook receipt observed".to_owned()),
        );
    }
    if !facts.boundary_response {
        return (
            Support::ConfiguredUnverified,
            Some("no boundary response emission observed".to_owned()),
        );
    }
    (Support::Full, None)
}

pub(super) fn support_assessments(
    repo: &Path,
    statuses: &[rift_agent_install::native::NativeIntegrationStatus],
    clients: &[rift_agent_install::ClientStatus],
    boundary_responses: &BTreeSet<String>,
    policy: &rift_recall_policy::ServePolicySnapshot,
) -> Vec<SupportAssessment> {
    let receipts: BTreeMap<&str, bool> = clients
        .iter()
        .map(|client| (client.client.token(), client.last_hook_at_ms.is_some()))
        .collect();
    statuses
        .iter()
        .map(|status| {
            let alpha = rift_agent_install::native::is_alpha_supported(&status.agent);
            let runtime_receipt = receipts
                .get(status.agent.as_str())
                .copied()
                .unwrap_or(false);
            let boundary_response = boundary_responses.contains(&status.agent);
            let facts = SupportFacts {
                alpha,
                descriptor: status.wiring,
                hooks: status.hook_wiring,
                runtime_receipt,
                boundary_response,
                codex_trust: status.codex_trust.clone(),
                hook_failure: crate::hooks::hook_degradation(repo, &status.agent),
            };
            let (support, degraded_reason) = derive_support(facts);
            let (requested_mode, effective_mode) = if alpha {
                let agent = rift_core::AgentId::new(status.agent.clone());
                let effective = rift_recall_policy::resolve(
                    &policy.policy,
                    &status.agent,
                    family_capability(&agent),
                );
                (
                    effective.read_deny_requested.as_str(),
                    effective.read_deny.as_str(),
                )
            } else {
                ("off", "off")
            };
            SupportAssessment {
                agent: status.agent.clone(),
                support,
                hook_configuration: status
                    .hook_wiring
                    .as_ref()
                    .map_or_else(|| "not_applicable".to_owned(), state_name),
                runtime_connection: if runtime_receipt {
                    "observed"
                } else {
                    "unobserved"
                },
                context_delivery: if boundary_response {
                    "observed"
                } else {
                    "unobserved"
                },
                requested_mode,
                effective_mode,
                degraded_reason,
            }
        })
        .collect()
}

/// Read only the append-only hook-channel rows that identify a per-agent
/// boundary response. Every dispatcher-owned plan now writes its family and
/// semantic kind into this existing relation after native encoding succeeds.
pub(super) fn observed_boundary_responses(repo: &Path) -> BTreeSet<String> {
    use rift_store::CanonicalStore as _;

    let Ok(db) = crate::family::recall_db(repo) else {
        return BTreeSet::new();
    };
    if !db.exists() {
        return BTreeSet::new();
    }
    let Ok(store) = rift_store::SqliteStore::open_read_only(&db) else {
        return BTreeSet::new();
    };
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().build() else {
        return BTreeSet::new();
    };
    runtime
        .block_on(store.query(
            "SELECT DISTINCT tool_name FROM serve_call_event \
             WHERE channel = 'hook_inject'",
            &[],
        ))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| row.get_text(0).ok())
        .filter_map(|tool| boundary_agent_from_tool_name(&tool))
        .collect()
}

fn boundary_agent_from_tool_name(tool: &str) -> Option<String> {
    let (agent, step) = tool.split_once(':')?;
    matches!(
        step,
        "recovery-map"
            | "session-pointer"
            | "delta-manifest"
            | "boundary-context"
            | "read-replacement"
            | "read-replacement-fallback"
    )
    .then(|| agent.to_owned())
}

pub(super) fn integration_status_json(
    statuses: &[rift_agent_install::native::NativeIntegrationStatus],
    assessments: &[SupportAssessment],
) -> Vec<Value> {
    statuses
        .iter()
        .zip(assessments)
        .map(|(status, assessment)| {
            let mut value = serde_json::to_value(status).unwrap_or(Value::Null);
            let extras = serde_json::to_value(assessment).unwrap_or(Value::Null);
            if let (Some(object), Some(extra)) = (value.as_object_mut(), extras.as_object()) {
                for (key, value) in extra {
                    if key != "agent" {
                        object.insert(key.clone(), value.clone());
                    }
                }
            }
            value
        })
        .collect()
}

pub(super) fn render_support(out: &mut String, assessments: &[SupportAssessment]) {
    let _ = writeln!(out, "\nSupport truth");
    let _ = writeln!(
        out,
        "  {:<16} {:<24} {:<20} {:<12} {:<12}",
        "agent", "support", "hooks", "runtime", "context"
    );
    for assessment in assessments {
        let _ = writeln!(
            out,
            "  {:<16} {:<24} {:<20} {:<12} {:<12}",
            assessment.agent,
            state_name(&assessment.support),
            assessment.hook_configuration,
            assessment.runtime_connection,
            assessment.context_delivery,
        );
    }
}

fn state_name<T: serde::Serialize>(state: &T) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn managed_facts() -> SupportFacts {
        SupportFacts {
            alpha: true,
            descriptor: rift_agent_install::native::NativeWiring::Managed,
            hooks: Some(rift_agent_install::native::NativeHookWiring::Managed),
            runtime_receipt: false,
            boundary_response: false,
            codex_trust: None,
            hook_failure: None,
        }
    }

    #[test]
    fn configured_without_a_receipt_is_not_full() {
        let (support, reason) = derive_support(managed_facts());
        assert_eq!(support, Support::ConfiguredUnverified);
        assert_eq!(reason.as_deref(), Some("no runtime hook receipt observed"));
    }

    #[test]
    fn a_receipt_without_a_boundary_response_is_not_full() {
        let (support, reason) = derive_support(SupportFacts {
            runtime_receipt: true,
            ..managed_facts()
        });
        assert_eq!(support, Support::ConfiguredUnverified);
        assert_eq!(
            reason.as_deref(),
            Some("no boundary response emission observed")
        );
    }

    #[test]
    fn all_four_support_facts_are_required_for_full() {
        let (support, reason) = derive_support(SupportFacts {
            runtime_receipt: true,
            boundary_response: true,
            ..managed_facts()
        });
        assert_eq!(support, Support::Full);
        assert_eq!(reason, None);
    }

    #[test]
    fn conflicting_or_missing_required_configuration_is_degraded() {
        for facts in [
            SupportFacts {
                descriptor: rift_agent_install::native::NativeWiring::Conflict,
                ..managed_facts()
            },
            SupportFacts {
                hooks: Some(rift_agent_install::native::NativeHookWiring::Invalid),
                ..managed_facts()
            },
            SupportFacts {
                hooks: Some(rift_agent_install::native::NativeHookWiring::Missing),
                ..managed_facts()
            },
        ] {
            let (support, reason) = derive_support(facts);
            assert_eq!(support, Support::Degraded);
            assert!(reason.is_some());
        }
    }

    fn codex_trust(
        verdict: rift_agent_install::native::CodexHookVerdict,
    ) -> rift_agent_install::native::CodexTrust {
        rift_agent_install::native::CodexTrust {
            hooks: vec![rift_agent_install::native::CodexHookTrust {
                event: "SessionStart".to_owned(),
                verdict,
            }],
            client: None,
        }
    }

    /// A Codex hook Codex is skipping degrades the row however much else was
    /// observed; one Rift cannot judge keeps it short of full.
    #[test]
    fn codex_trust_that_skips_a_hook_degrades_and_an_unverifiable_one_prevents_full() {
        use rift_agent_install::native::CodexHookVerdict;
        let judged = |verdict| {
            derive_support(SupportFacts {
                runtime_receipt: true,
                boundary_response: true,
                codex_trust: Some(codex_trust(verdict)),
                ..managed_facts()
            })
        };
        let (support, reason) = judged(CodexHookVerdict::Modified);
        assert_eq!(support, Support::Degraded);
        assert_eq!(
            reason.as_deref(),
            Some("error — Codex is skipping 1 of 1 Rift hooks: SessionStart modified")
        );
        assert_eq!(judged(CodexHookVerdict::Untrusted).0, Support::Degraded);
        assert_eq!(
            judged(CodexHookVerdict::Unverifiable).0,
            Support::ConfiguredUnverified
        );
        assert_eq!(judged(CodexHookVerdict::Trusted).0, Support::Full);
    }

    #[test]
    fn a_hook_failure_degrades_an_otherwise_full_integration() {
        let (support, reason) = derive_support(SupportFacts {
            runtime_receipt: true,
            boundary_response: true,
            hook_failure: Some("hook execution did not complete".to_owned()),
            ..managed_facts()
        });
        assert_eq!(support, Support::Degraded);
        assert_eq!(reason.as_deref(), Some("hook execution did not complete"));
    }

    #[test]
    fn every_non_alpha_adapter_is_always_coming_soon() {
        for agent in [
            "gemini",
            "cursor",
            "copilot-cli",
            "opencode",
            "factory-droid",
            "ollama",
        ] {
            assert!(!rift_agent_install::native::is_alpha_supported(agent));
            let facts = SupportFacts {
                alpha: false,
                descriptor: rift_agent_install::native::NativeWiring::Invalid,
                hooks: Some(rift_agent_install::native::NativeHookWiring::Conflict),
                runtime_receipt: true,
                boundary_response: true,
                ..managed_facts()
            };
            let (support, reason) = derive_support(facts);
            assert_eq!(support, Support::ComingSoon);
            assert_eq!(reason, None);
        }
    }

    #[test]
    fn support_fields_extend_the_existing_native_json_row() {
        let native = rift_agent_install::native::NativeIntegrationStatus {
            agent: "claude-code".to_owned(),
            display_name: "Claude Code".to_owned(),
            detection: rift_agent_install::native::NativeDetection::Detected,
            wiring: rift_agent_install::native::NativeWiring::Managed,
            executable: Some(std::path::PathBuf::from("/opt/claude")),
            installations: vec![rift_agent_install::native::NativeInstallation {
                source: rift_agent_install::native::NativeInstallSource::PathCommand,
                version: None,
                executable: Some(std::path::PathBuf::from("/opt/claude")),
                path: std::path::PathBuf::from("/opt/claude"),
            }],
            hook_wiring: Some(rift_agent_install::native::NativeHookWiring::Managed),
            codex_trust: None,
        };
        let assessment = SupportAssessment {
            agent: native.agent.clone(),
            support: Support::Full,
            hook_configuration: "managed".to_owned(),
            runtime_connection: "observed",
            context_delivery: "observed",
            requested_mode: "enforce",
            effective_mode: "enforce",
            degraded_reason: None,
        };
        let rows = integration_status_json(&[native], &[assessment]);
        let row = &rows[0];
        assert_eq!(row["display_name"], "Claude Code");
        assert_eq!(row["detection"], "detected");
        assert_eq!(row["wiring"], "managed");
        assert_eq!(row["support"], "full");
        assert_eq!(row["hook_configuration"], "managed");
        assert_eq!(row["runtime_connection"], "observed");
        assert_eq!(row["context_delivery"], "observed");
        assert_eq!(row["requested_mode"], "enforce");
        assert_eq!(row["effective_mode"], "enforce");
        assert_eq!(row["degraded_reason"], Value::Null);
    }

    #[test]
    fn only_attributable_boundary_rows_prove_context_delivery() {
        assert_eq!(
            boundary_agent_from_tool_name("antigravity:recovery-map").as_deref(),
            Some("antigravity")
        );
        assert_eq!(
            boundary_agent_from_tool_name("codex:session-pointer").as_deref(),
            Some("codex")
        );
        assert_eq!(
            boundary_agent_from_tool_name("antigravity:read-replacement-fallback").as_deref(),
            Some("antigravity")
        );
        assert_eq!(boundary_agent_from_tool_name("codex:read-gate"), None);
        assert_eq!(boundary_agent_from_tool_name("known-state"), None);
    }

    #[test]
    fn human_status_names_path_availability_without_implying_active_use() {
        let mut out = String::new();
        render_status(
            &mut out,
            &[rift_agent_install::native::NativeIntegrationStatus {
                agent: "codex".to_owned(),
                display_name: "Codex".to_owned(),
                detection: rift_agent_install::native::NativeDetection::Detected,
                wiring: rift_agent_install::native::NativeWiring::Missing,
                executable: Some(std::path::PathBuf::from("/usr/local/bin/codex")),
                installations: vec![
                    rift_agent_install::native::NativeInstallation {
                        source: rift_agent_install::native::NativeInstallSource::PathCommand,
                        version: None,
                        executable: Some(std::path::PathBuf::from("/usr/local/bin/codex")),
                        path: std::path::PathBuf::from("/usr/local/bin/codex"),
                    },
                    rift_agent_install::native::NativeInstallation {
                        source: rift_agent_install::native::NativeInstallSource::IdeExtension(
                            rift_agent_install::native::NativeIdeHost::VsCode,
                        ),
                        version: Some("26.803.61601".to_owned()),
                        executable: None,
                        path: std::path::PathBuf::from(
                            "/home/test/.vscode/extensions/openai.chatgpt-26.803.61601",
                        ),
                    },
                ],
                hook_wiring: Some(rift_agent_install::native::NativeHookWiring::Managed),
                codex_trust: None,
            }],
        );
        assert!(out.contains("present via PATH command + VS Code extension 26.803.61601"));
        assert!(out.contains("managed"));
        assert!(!out.contains("detected"));
    }
}
