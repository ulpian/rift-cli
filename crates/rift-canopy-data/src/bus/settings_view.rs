//! The read-only Project Settings projection: whether a repository is set up
//! on this machine, its policy rows, and every native adapter's row.
//!
//! A separate module because this view has three readers with one meaning —
//! the CLI, the modal and the bus — and the "set up here" question it answers
//! is the one the project status must agree with.

use std::collections::BTreeSet;
use std::path::Path;

use rift_agent_install::health::{Remedy, Severity};
use rift_canopy::{
    NativeAgentDetection, NativeAgentHookWiring, NativeAgentIdeHost, NativeAgentInstallSource,
    NativeAgentInstallation, NativeAgentIntegrationRow, NativeAgentRemedy,
    NativeAgentRemedySeverity, NativeAgentWiring, ProjectInitialization, ProjectPolicyState,
    ProjectPolicySummary, ProjectSettingId, ProjectSettingRow, ProjectSettingsView,
};
use rift_config::{EffectiveConfig, PolicySource};
use rift_recall_policy::{load_serve_policy, ServePolicySource};
use rift_store::{projection_db_for_family_under, resolve_cache_root};

use crate::feeds::project_family;

use super::project_read::project_remedies;
use super::setup_plan::{pending_setup_plan, setup_left_pending};

/// The repository's effective configuration as the settings page reads it.
///
/// An unreadable file falls back to the secure default and reads `Invalid`:
/// the toggles still show what applies, while the detail says what to repair
/// and the actions lock.
struct ConfigRead {
    effective: Option<EffectiveConfig>,
    state: ProjectPolicyState,
    detail: Option<String>,
}

/// The serving-policy values the Recall rows are read from.
#[derive(Clone, Copy, Debug)]
struct ServeLevers {
    injection_budget: u32,
    doc_mode: rift_recall_policy::Mode,
    search_mode: rift_recall_policy::Mode,
}

impl ConfigRead {
    fn load(rift_dir: &Path) -> Self {
        match EffectiveConfig::load(rift_dir) {
            Ok(effective) => {
                let state = match effective.policy_source() {
                    PolicySource::Declared => ProjectPolicyState::Declared,
                    PolicySource::Default => ProjectPolicyState::Default,
                    PolicySource::Refused(_) => ProjectPolicyState::Refused,
                };
                let detail = effective.policy_source().diagnostic().map(str::to_owned);
                Self {
                    effective: Some(effective),
                    state,
                    detail,
                }
            }
            Err(_) => Self {
                effective: EffectiveConfig::secure_default().ok(),
                state: ProjectPolicyState::Invalid,
                detail: Some(
                    ".rift/config.toml is invalid; repair it before changing settings".to_owned(),
                ),
            },
        }
    }
}

/// Build the complete read-only Project Settings projection for a repository.
///
/// This function has no write path and never probes or launches an agent. It is
/// also the binary manager's verification read after a lifecycle operation, so
/// the CLI, modal, and data bus share one interpretation of policy and adapter
/// state.
#[must_use]
pub fn project_settings_view(dir: &Path) -> ProjectSettingsView {
    project_settings_view_under(&resolve_cache_root(|key| std::env::var(key).ok()), dir)
}

/// [`project_settings_view`] against an explicit state root — the bus's,
/// which it resolved once at spawn, and a test's temporary directory.
#[must_use]
pub fn project_settings_view_under(root: &Path, dir: &Path) -> ProjectSettingsView {
    let path = dir.to_string_lossy().into_owned();
    let project_name = dir
        .file_name()
        .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned());
    let initialization = project_initialization(root, dir);
    let rift_dir = dir.join(".rift");
    let config = ConfigRead::load(&rift_dir);
    let serve = load_serve_policy(&rift_dir);
    let state = combine_policy_state(config.state, serve_policy_state(serve.source));
    let detail = config.detail.or_else(|| serve_policy_detail(serve.source));
    let packs = rift_config::load_redactor_packs(&rift_config::redactor_pack_dir(&rift_dir));
    let editable = !matches!(
        state,
        ProjectPolicyState::Invalid | ProjectPolicyState::Refused
    );
    let mut settings = project_setting_rows(
        config.effective.as_ref(),
        initialization,
        editable,
        ServeLevers {
            injection_budget: serve.policy.injection_turn_budget,
            doc_mode: serve.policy.doc.mode,
            search_mode: serve.policy.search.mode,
        },
    );
    let mut integrations = native_settings_integrations(root, dir, initialization);
    // Only a setup the predicate says is unfinished resumes from its plan; a
    // plan left beside a set-up family pre-fills nothing.
    if initialization == ProjectInitialization::SetupIncomplete {
        if let Some(plan) = pending_setup_plan(dir) {
            restore_pending_choices(&plan, &mut settings, &mut integrations);
        }
    }
    ProjectSettingsView {
        path,
        project_name,
        initialization,
        settings,
        policy: ProjectPolicySummary {
            state,
            detail,
            sensitive_path_count: config
                .effective
                .as_ref()
                .map_or(0, |config| config.sensitive_paths().len()),
            loaded_redactor_packs: packs.packs.len(),
            failed_redactor_packs: packs.diagnostics.len(),
        },
        integrations,
    }
}

/// Whether this machine installed `family`'s git hooks: its binary of record
/// names an executable that is still there.
///
/// The record is the install's own note, written beside the family manifest,
/// so this is one small JSON file read during a status — never on a hook's
/// path, which decides nothing from here.
pub(super) fn hooks_installed_here(family: &rift_git::Family) -> bool {
    rift_shadow_hooks::read_binary_of_record(&family_dir(family))
        .is_some_and(|record| record.is_live())
}

/// Where `family` keeps its manifest and its binary of record.
pub(super) fn family_dir(family: &rift_git::Family) -> std::path::PathBuf {
    rift_git::family::family_dir_under(|key| std::env::var(key).ok(), &family.common_dir)
}

/// The serve overlay's source, in the settings page's vocabulary.
const fn serve_policy_state(source: ServePolicySource) -> ProjectPolicyState {
    match source {
        ServePolicySource::Declared => ProjectPolicyState::Declared,
        ServePolicySource::Default => ProjectPolicyState::Default,
        ServePolicySource::Invalid => ProjectPolicyState::Invalid,
        ServePolicySource::Refused => ProjectPolicyState::Refused,
    }
}

/// What a serve overlay that cannot be used says about itself.
fn serve_policy_detail(source: ServePolicySource) -> Option<String> {
    match source {
        ServePolicySource::Invalid => {
            Some(".rift/policies/serve.toml is invalid; Recall serving is fail-closed".to_owned())
        }
        ServePolicySource::Refused => Some(
            ".rift/policies/serve.toml is refused; repair its ownership or file type".to_owned(),
        ),
        ServePolicySource::Declared | ServePolicySource::Default => None,
    }
}

/// A setup in progress shows the choices the wizard was left with rather than
/// the repository's defaults, so a resumed wizard picks up where it stopped.
/// A key the plan does not carry keeps the row's own reading.
fn restore_pending_choices(
    plan: &serde_json::Value,
    settings: &mut [ProjectSettingRow],
    integrations: &mut [NativeAgentIntegrationRow],
) {
    for row in settings.iter_mut() {
        let key = setting_plan_key(row.id);
        if let Some(enabled) = plan.get(key).and_then(serde_json::Value::as_bool) {
            row.enabled = enabled;
        }
    }
    let selected = plan
        .get("integrations")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect::<BTreeSet<_>>();
    for row in integrations.iter_mut() {
        row.selected = selected.contains(row.agent.as_str());
    }
}

/// The setup plan's key for a settings row.
const fn setting_plan_key(id: ProjectSettingId) -> &'static str {
    match id {
        ProjectSettingId::TotalRecall => "total_recall",
        ProjectSettingId::TurnInjection => "turn_injection",
        ProjectSettingId::Documents => "documents",
        ProjectSettingId::Search => "search",
        ProjectSettingId::ToolCallCapture => "tool_call_capture",
        ProjectSettingId::TokenUsageCapture => "token_usage_capture",
        ProjectSettingId::RawTranscripts => "raw_transcripts",
    }
}

fn project_setting_rows(
    effective: Option<&rift_config::EffectiveConfig>,
    initialization: ProjectInitialization,
    editable: bool,
    levers: ServeLevers,
) -> Vec<ProjectSettingRow> {
    let injection_budget = levers.injection_budget;
    // Recall is on unless the repository's configuration writes it off — the
    // setup default and the declared value are one answer, so a repository
    // that committed `total_recall = false` is not talked out of it by a
    // wizard. An unreadable configuration falls back to the secure default,
    // which is on.
    let recall = effective.is_none_or(rift_config::EffectiveConfig::recall_enabled);
    // A configured-elsewhere project's toggles are read-only facts: setup on
    // this machine must never rewrite the repository's committed file.
    let actionable = editable && initialization.settings_editable();
    vec![
        ProjectSettingRow {
            id: ProjectSettingId::TotalRecall,
            enabled: recall,
            actionable,
            detail: None,
        },
        ProjectSettingRow {
            id: ProjectSettingId::TurnInjection,
            enabled: injection_budget > 0,
            actionable,
            detail: (injection_budget > 0
                && injection_budget != rift_recall_policy::DEFAULT_INJECTION_TURN_BUDGET)
                .then(|| format!("custom effective budget: {injection_budget}")),
        },
        lever_row(ProjectSettingId::Documents, levers.doc_mode, actionable),
        lever_row(ProjectSettingId::Search, levers.search_mode, actionable),
        ProjectSettingRow {
            id: ProjectSettingId::ToolCallCapture,
            enabled: effective.is_some_and(rift_config::EffectiveConfig::captures_tool_calls),
            actionable,
            detail: None,
        },
        ProjectSettingRow {
            id: ProjectSettingId::TokenUsageCapture,
            enabled: effective.is_some_and(rift_config::EffectiveConfig::captures_token_usage),
            actionable,
            detail: None,
        },
        ProjectSettingRow {
            id: ProjectSettingId::RawTranscripts,
            enabled: effective.is_some_and(rift_config::EffectiveConfig::raw_transcripts_enabled),
            actionable,
            detail: Some("warning confirmation required when enabling".to_owned()),
        },
    ]
}

/// A lever table as a binary row: `off` is off, and both `enforce` and an
/// explicit `observe` are on — the latter with the detail that says so, so
/// the toggle never overwrites a declaration nobody meant to flip.
fn lever_row(
    id: ProjectSettingId,
    mode: rift_recall_policy::Mode,
    actionable: bool,
) -> ProjectSettingRow {
    ProjectSettingRow {
        id,
        enabled: mode != rift_recall_policy::Mode::Off,
        actionable,
        detail: (mode == rift_recall_policy::Mode::Observe)
            .then(|| "custom effective mode: observe".to_owned()),
    }
}

/// Whether the project is set up **on this machine**: the family's projection
/// exists under `root` **and** its git hooks were installed here.
///
/// Neither half is enough on its own. `.rift/` is committed configuration and
/// travels with a clone, so it proves nothing about this machine; reading it
/// as "initialized" is what kept a fresh clone's setup wizard (and its
/// pre-ticked detected agents) from ever appearing on a second machine. The
/// projection proves little more: an agent hook opens the store on first
/// contact and creates it, so a family nothing ever installed hooks for
/// carries one — and read as initialized, it offered no setup to the one
/// repository that needed it most.
fn project_initialization(root: &Path, dir: &Path) -> ProjectInitialization {
    let family = project_family(dir);
    if setup_left_pending(root, dir, family.as_ref()) {
        return ProjectInitialization::SetupIncomplete;
    }
    let Some(family) = family else {
        return if rift_git::discover(dir).is_ok() {
            // A repository whose identity cannot be resolved from here reads
            // like one with no projection, because it has none to key.
            ProjectInitialization::NotInitialized
        } else {
            ProjectInitialization::NotRepository
        };
    };
    if !projection_db_for_family_under(root, &family).exists() {
        return if dir.join(".rift").is_dir() {
            ProjectInitialization::NotProvisioned
        } else {
            ProjectInitialization::NotInitialized
        };
    }
    if hooks_installed_here(&family) {
        ProjectInitialization::Initialized
    } else {
        ProjectInitialization::SetupIncomplete
    }
}

fn native_settings_integrations(
    root: &Path,
    dir: &Path,
    initialization: ProjectInitialization,
) -> Vec<NativeAgentIntegrationRow> {
    native_settings_rows(
        rift_agent_install::native::diagnose_all(dir),
        &project_remedies(root, dir),
        initialization,
    )
}

/// The Project Settings rows for one diagnostic — the pure half of
/// [`native_settings_integrations`], so the projection of the installer's
/// vocabulary (and the pre-selection rule it feeds) is testable without a
/// machine's real `PATH`, home or application roots behind `diagnose_all`.
///
/// Each row carries the remedies whose finding names its agent, in the order
/// the assessment sorted them.
fn native_settings_rows(
    statuses: Vec<rift_agent_install::native::NativeIntegrationStatus>,
    remedies: &[Remedy],
    initialization: ProjectInitialization,
) -> Vec<NativeAgentIntegrationRow> {
    use rift_agent_install::native::{NativeDetection, NativeHookWiring, NativeWiring};

    statuses
        .into_iter()
        .map(|row| {
            let detection = match row.detection {
                NativeDetection::Detected => NativeAgentDetection::Detected,
                NativeDetection::NotDetected => NativeAgentDetection::NotDetected,
            };
            let wiring = match row.wiring {
                NativeWiring::Missing => NativeAgentWiring::Missing,
                NativeWiring::Managed => NativeAgentWiring::Managed,
                NativeWiring::Conflict => NativeAgentWiring::Conflict,
                NativeWiring::Invalid => NativeAgentWiring::Invalid,
            };
            let hook_wiring = row.hook_wiring.map(|hooks| match hooks {
                NativeHookWiring::Missing => NativeAgentHookWiring::Missing,
                NativeHookWiring::Managed => NativeAgentHookWiring::Managed,
                NativeHookWiring::Stale => NativeAgentHookWiring::Stale,
                NativeHookWiring::Conflict => NativeAgentHookWiring::Conflict,
                NativeHookWiring::Invalid => NativeAgentHookWiring::Invalid,
            });
            // The one predicate. `detected()` is the same question `rift
            // init` asks before it wires a row, so the pre-tick can never
            // offer what init would not have wired, or the reverse.
            let detected = row.detected();
            let installations = row
                .installations
                .into_iter()
                .map(installation_row)
                .collect::<Vec<_>>();
            let alpha_supported = rift_agent_install::native::is_alpha_supported(&row.agent);
            NativeAgentIntegrationRow {
                remedies: agent_remedies(&row.agent, remedies),
                agent: row.agent,
                display_name: row.display_name,
                detection,
                executable_name: row.executable.and_then(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                }),
                installations,
                wiring,
                hook_wiring,
                alpha_supported,
                selected: default_selection(wiring, initialization, detected, alpha_supported),
            }
        })
        .collect()
}

/// One install as the dashboard names it.
fn installation_row(
    installation: rift_agent_install::native::NativeInstallation,
) -> NativeAgentInstallation {
    use rift_agent_install::native::{NativeIdeHost, NativeInstallSource};
    let source = match installation.source {
        NativeInstallSource::PathCommand => NativeAgentInstallSource::PathCommand,
        NativeInstallSource::VendorInstall => NativeAgentInstallSource::VendorInstall,
        NativeInstallSource::IdeExtension(host) => {
            NativeAgentInstallSource::IdeExtension(match host {
                NativeIdeHost::VsCode => NativeAgentIdeHost::VsCode,
                NativeIdeHost::VsCodeInsiders => NativeAgentIdeHost::VsCodeInsiders,
                NativeIdeHost::Cursor => NativeAgentIdeHost::Cursor,
                NativeIdeHost::Windsurf => NativeAgentIdeHost::Windsurf,
            })
        }
        NativeInstallSource::JetBrainsPlugin => NativeAgentInstallSource::JetBrainsPlugin,
    };
    NativeAgentInstallation {
        source,
        version: installation.version,
    }
}

/// `agent`'s remedies, in the dashboard's words for them: the severity, the
/// headline and the sentence the command line prints for the same finding.
fn agent_remedies(agent: &str, remedies: &[Remedy]) -> Vec<NativeAgentRemedy> {
    remedies
        .iter()
        .filter(|remedy| remedy.agent == agent)
        .map(|remedy| NativeAgentRemedy {
            severity: match remedy.severity {
                Severity::Error => NativeAgentRemedySeverity::Error,
                Severity::Warning => NativeAgentRemedySeverity::Warning,
            },
            headline: remedy.headline.clone(),
            sentence: remedy.sentence.clone(),
        })
        .collect()
}

/// Whether a fresh Project Settings view selects an integration row before the
/// user touches it.
///
/// A managed row is always selected (it reflects reality). Wherever the
/// setup wizard is offered — not initialized, configured elsewhere, or a
/// setup in progress — a DETECTED row is pre-selected as a suggestion, but
/// only when this alpha can actually capture it: pre-selecting a coming-soon
/// agent stages a selection `initialize()` is guaranteed to refuse, which
/// turned the default setup plan into an error. Pure so the rule is testable
/// without a machine's real `PATH` behind `diagnose_all`.
const fn default_selection(
    wiring: NativeAgentWiring,
    initialization: ProjectInitialization,
    detected: bool,
    alpha_supported: bool,
) -> bool {
    matches!(wiring, NativeAgentWiring::Managed)
        || initialization.offers_setup() && detected && alpha_supported
}

const fn combine_policy_state(
    config: ProjectPolicyState,
    serve: ProjectPolicyState,
) -> ProjectPolicyState {
    if matches!(config, ProjectPolicyState::Refused) || matches!(serve, ProjectPolicyState::Refused)
    {
        ProjectPolicyState::Refused
    } else if matches!(config, ProjectPolicyState::Invalid)
        || matches!(serve, ProjectPolicyState::Invalid)
    {
        ProjectPolicyState::Invalid
    } else if matches!(config, ProjectPolicyState::Declared)
        || matches!(serve, ProjectPolicyState::Declared)
    {
        ProjectPolicyState::Declared
    } else {
        ProjectPolicyState::Default
    }
}

#[cfg(test)]
mod tests {
    use rift_agent_install::native::{
        NativeDetection, NativeIdeHost, NativeInstallSource, NativeInstallation,
        NativeIntegrationStatus, NativeWiring,
    };
    use rift_canopy::ProjectStatus;

    use super::super::fixtures::record_hooks_installed;
    use super::super::project_read::read_project;
    use super::*;

    #[test]
    fn uninitialized_settings_use_native_defaults_and_all_adapters() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(repo.path()).unwrap();
        let view = project_settings_view_under(root.path(), repo.path());
        assert_eq!(view.initialization, ProjectInitialization::NotInitialized);
        assert_eq!(view.integrations.len(), 9);
        assert_eq!(
            view.integrations
                .iter()
                .map(|row| row.agent.as_str())
                .collect::<Vec<_>>(),
            vec![
                "claude-code",
                "codex",
                "gemini",
                "antigravity",
                "cursor",
                "copilot-cli",
                "opencode",
                "factory-droid",
                "ollama",
            ]
        );
        assert!(view.settings[0].enabled, "Recall setup default is on");
        assert!(
            view.settings[1].enabled,
            "400-token injection default is on"
        );
        assert!(view.settings[2].enabled, "documents default is on");
        assert!(view.settings[3].enabled, "native search default is on");
        assert!(view.settings[4].enabled, "tool capture default is on");
        assert!(view.settings[5].enabled, "token capture default is on");
        assert!(view.settings[6].enabled, "raw transcripts default on");
        assert_eq!(
            view.integrations
                .iter()
                .filter(|row| row.alpha_supported)
                .map(|row| row.agent.as_str())
                .collect::<Vec<_>>(),
            vec!["claude-code", "codex", "antigravity"],
            "each row carries the alpha capture scope, single-sourced from \
             the registry-side list"
        );
    }

    /// The pre-selection rule, driven pure: `diagnose_all` reads the
    /// machine's real `PATH`, so the extracted predicate is the testable
    /// seam. The load-bearing arm is the third: a DETECTED coming-soon agent
    /// must not be staged into a default setup plan `initialize()` is
    /// guaranteed to refuse.
    #[test]
    fn a_detected_coming_soon_agent_is_not_preselected_for_setup() {
        use rift_canopy::NativeAgentWiring as Wiring;
        use rift_canopy::ProjectInitialization as Init;
        assert!(default_selection(
            Wiring::Missing,
            Init::NotInitialized,
            true,
            true
        ));
        assert!(!default_selection(
            Wiring::Missing,
            Init::NotInitialized,
            true,
            false
        ));
        assert!(default_selection(
            Wiring::Managed,
            Init::Initialized,
            false,
            false
        ));
        assert!(!default_selection(
            Wiring::Missing,
            Init::Initialized,
            true,
            true
        ));
    }

    /// The install evidence four rows are projected from: an extension-only
    /// install, three sources at once, a coming-soon agent on `PATH`, and an
    /// absent one.
    fn install_evidence() -> Vec<NativeIntegrationStatus> {
        let status =
            |agent: &str, installations: Vec<NativeInstallation>| NativeIntegrationStatus {
                agent: agent.to_owned(),
                display_name: agent.to_owned(),
                detection: if installations.is_empty() {
                    NativeDetection::NotDetected
                } else {
                    NativeDetection::Detected
                },
                wiring: NativeWiring::Missing,
                executable: None,
                installations,
                hook_wiring: None,
                codex_trust: None,
            };
        let install = |source, version: Option<&str>| NativeInstallation {
            source,
            version: version.map(str::to_owned),
            executable: None,
            path: std::path::PathBuf::from("x"),
        };
        vec![
            status(
                "claude-code",
                vec![
                    install(
                        NativeInstallSource::IdeExtension(NativeIdeHost::VsCode),
                        Some("2.1.267"),
                    ),
                    install(NativeInstallSource::JetBrainsPlugin, None),
                ],
            ),
            status(
                "codex",
                vec![
                    install(NativeInstallSource::PathCommand, None),
                    install(NativeInstallSource::VendorInstall, Some("26.9.1")),
                    install(
                        NativeInstallSource::IdeExtension(NativeIdeHost::Windsurf),
                        Some("26.9"),
                    ),
                ],
            ),
            status(
                "ollama",
                vec![install(NativeInstallSource::PathCommand, None)],
            ),
            status("antigravity", Vec::new()),
        ]
    }

    /// The row's availability sentence is the installer's own, so the
    /// dashboard and `rift recall status` cannot name different reasons.
    #[test]
    fn a_row_spells_the_installers_own_availability_sentence() {
        let statuses = install_evidence();
        let expected: Vec<String> = statuses
            .iter()
            .map(NativeIntegrationStatus::availability)
            .collect();
        let rows = native_settings_rows(statuses, &[], ProjectInitialization::NotInitialized);
        let actual: Vec<String> = rows
            .iter()
            .map(NativeAgentIntegrationRow::availability)
            .collect();
        assert_eq!(actual, expected, "the row spells the installer's sentence");
        assert_eq!(
            actual[0],
            "present via VS Code extension 2.1.267 + JetBrains plugin"
        );
        assert_eq!(
            actual[1],
            "present via PATH command + vendor install 26.9.1 + Windsurf extension 26.9"
        );
        assert_eq!(actual[3], "absent");
    }

    /// Every install source survives into the row with its host and version.
    #[test]
    fn an_install_source_survives_into_the_row_with_its_host() {
        let rows = native_settings_rows(
            install_evidence(),
            &[],
            ProjectInitialization::NotInitialized,
        );
        assert_eq!(
            rows[1].installations[2].source,
            NativeAgentInstallSource::IdeExtension(NativeAgentIdeHost::Windsurf)
        );
        assert_eq!(rows[1].installations[1].version.as_deref(), Some("26.9.1"));
    }

    /// An extension-only install pre-ticks a supported row before
    /// initialization exactly as a `PATH` command does — the gap where Canopy
    /// offered Codex and `rift init` skipped it — and a detected coming-soon
    /// agent is never staged.
    #[test]
    fn a_settings_row_carries_its_agents_remedy_and_a_healthy_agent_carries_none() {
        use rift_agent_install::health::{remedy, Finding, Reason, Surface};
        let findings = [
            Finding::new(
                Reason::HookBinaryMissing,
                "codex",
                None,
                "/gone/rift".to_owned(),
            ),
            Finding::new(
                Reason::HookBinaryDisagrees,
                "codex",
                None,
                "a vs b".to_owned(),
            ),
        ];
        let remedies: Vec<Remedy> = findings
            .iter()
            .map(|finding| remedy(finding, Surface::Terminal, None))
            .collect();
        let rows = native_settings_rows(
            install_evidence(),
            &remedies,
            ProjectInitialization::Initialized,
        );
        let codex = rows.iter().find(|row| row.agent == "codex").unwrap();
        let expected: Vec<NativeAgentRemedy> = remedies
            .iter()
            .zip([
                NativeAgentRemedySeverity::Error,
                NativeAgentRemedySeverity::Warning,
            ])
            .map(|(remedy, severity)| NativeAgentRemedy {
                severity,
                headline: remedy.headline.clone(),
                sentence: remedy.sentence.clone(),
            })
            .collect();
        assert_eq!(codex.remedies, expected);
        assert!(rows
            .iter()
            .filter(|row| row.agent != "codex")
            .all(|row| row.remedies.is_empty()));
    }

    #[test]
    fn install_evidence_pre_ticks_only_the_supported_detected_rows() {
        let rows = native_settings_rows(
            install_evidence(),
            &[],
            ProjectInitialization::NotInitialized,
        );
        assert!(
            rows[0].detected() && rows[0].selected,
            "extension-only pre-ticks"
        );
        assert!(rows[1].selected);
        assert!(
            rows[2].detected() && !rows[2].selected,
            "detected but coming soon is never staged"
        );
        assert!(!rows[3].detected() && !rows[3].selected);
    }

    /// Initialized means **this machine holds the family's projection AND
    /// installed its hooks** — never that `.rift/` exists, and never the
    /// projection alone. A clone whose `.rift/config.toml` travelled
    /// with the repository reads as configured elsewhere: its committed
    /// values are shown read-only, the wizard is offered, and detected
    /// supported agents are pre-ticked for it, exactly as on a fresh
    /// repository. A plain directory outside any repository has no family
    /// to provision and is offered no wizard.
    #[test]
    fn a_committed_rift_dir_without_a_projection_is_not_set_up_here() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(repo.path()).unwrap();
        std::fs::create_dir_all(repo.path().join(".rift")).unwrap();
        std::fs::write(
            repo.path().join(".rift/config.toml"),
            "schema_version = 1\n[project]\nname = \"demo\"\n[features]\ntotal_recall = false\n",
        )
        .unwrap();

        let view = project_settings_view_under(root.path(), repo.path());
        assert_eq!(view.initialization, ProjectInitialization::NotProvisioned);
        assert!(view.initialization.offers_setup());
        assert!(
            view.settings.iter().all(|row| !row.actionable),
            "committed values are facts, not toggles: setup never rewrites the shared file"
        );
        assert!(
            !view.settings[0].enabled,
            "the committed Recall value is shown, not the setup default"
        );
        assert_eq!(
            read_project(root.path(), &repo.path().to_string_lossy()).status,
            ProjectStatus::NotProvisioned
        );

        // A projection is not provisioning. An agent hook creates one on
        // first contact, so a family whose hooks were never installed can
        // carry a projection and nothing else.
        let family = project_family(repo.path()).unwrap();
        let db = projection_db_for_family_under(root.path(), &family);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, b"").unwrap();
        let view = project_settings_view_under(root.path(), repo.path());
        assert_eq!(
            view.initialization,
            ProjectInitialization::SetupIncomplete,
            "a projection with no recorded hook install is setup left half done"
        );
        assert!(
            !view.settings[0].enabled,
            "and the committed Recall value is still the one shown"
        );

        // The recorded hook install is the other half.
        record_hooks_installed(repo.path());
        assert_eq!(
            project_settings_view_under(root.path(), repo.path()).initialization,
            ProjectInitialization::Initialized
        );

        // Outside any repository there is nothing to provision.
        let plain = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(plain.path().join(".rift")).unwrap();
        let view = project_settings_view_under(root.path(), plain.path());
        assert_eq!(view.initialization, ProjectInitialization::NotRepository);
        assert!(!view.initialization.offers_setup());
        assert!(view.integrations.iter().all(|row| !row.selected));
        assert_eq!(
            read_project(root.path(), &plain.path().to_string_lossy()).status,
            ProjectStatus::NotRepository
        );
    }

    #[test]
    fn settings_report_custom_injection_and_safe_policy_counts() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(repo.path()).unwrap();
        let family = project_family(repo.path()).unwrap();
        let db = projection_db_for_family_under(root.path(), &family);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, b"").unwrap();
        record_hooks_installed(repo.path());
        std::fs::create_dir_all(repo.path().join(".rift/policies")).unwrap();
        std::fs::write(
            repo.path().join(".rift/config.toml"),
            "schema_version = 1\n[project]\nname = \"demo\"\n[features]\ntotal_recall = true\n[capture]\ntool_calls = false\ntoken_usage = true\nraw_transcripts = false\nacknowledge_risk = false\n",
        )
        .unwrap();
        std::fs::write(
            repo.path().join(".rift/policies/serve.toml"),
            "injection_turn_budget = 777\n[doc]\nmode = \"observe\"\n[search]\nmode = \"off\"\n",
        )
        .unwrap();
        let view = project_settings_view_under(root.path(), repo.path());
        assert_eq!(view.initialization, ProjectInitialization::Initialized);
        assert_eq!(view.policy.state, ProjectPolicyState::Declared);
        assert!(view.policy.sensitive_path_count >= 9);
        let row = |id| view.settings.iter().find(|row| row.id == id).unwrap();
        let injection = row(ProjectSettingId::TurnInjection);
        assert!(injection.enabled);
        assert_eq!(
            injection.detail.as_deref(),
            Some("custom effective budget: 777")
        );
        // The two lever rows read the tables: `observe` is on with its
        // detail, so a toggle never silently destroys it; `off` is off.
        let documents = row(ProjectSettingId::Documents);
        assert!(documents.enabled);
        assert_eq!(
            documents.detail.as_deref(),
            Some("custom effective mode: observe")
        );
        let search = row(ProjectSettingId::Search);
        assert!(!search.enabled);
        assert_eq!(search.detail, None);
        assert!(!row(ProjectSettingId::ToolCallCapture).enabled);
        assert!(
            !row(ProjectSettingId::RawTranscripts).enabled,
            "a config that writes raw transcripts off shows them off"
        );
    }

    #[test]
    fn invalid_policy_is_truthful_and_locks_actions() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".rift")).unwrap();
        std::fs::write(repo.path().join(".rift/config.toml"), "not = [valid").unwrap();
        let view = project_settings_view(repo.path());
        assert_eq!(view.policy.state, ProjectPolicyState::Invalid);
        assert!(view.settings.iter().all(|row| !row.actionable));
        assert!(view
            .policy
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("repair")));
    }

    #[test]
    fn a_pending_setup_plan_owns_the_distinct_project_status() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(repo.join(".rift/state")).unwrap();
        std::fs::write(repo.join(".rift/state/project-setup.json"), "{}\n").unwrap();
        assert_eq!(
            read_project(root.path(), &repo.to_string_lossy()).status,
            ProjectStatus::SetupIncomplete
        );
        assert_eq!(
            project_settings_view(&repo).initialization,
            ProjectInitialization::SetupIncomplete
        );
    }

    #[test]
    fn a_pending_setup_plan_restores_revised_choices_for_resume() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".rift/state")).unwrap();
        std::fs::write(
            repo.path().join(".rift/state/project-setup.json"),
            r#"{
  "schema_version": 1,
  "total_recall": false,
  "turn_injection": false,
  "documents": false,
  "tool_call_capture": false,
  "token_usage_capture": true,
  "raw_transcripts": true,
  "integrations": ["ollama"]
}"#,
        )
        .unwrap();
        let view = project_settings_view(repo.path());
        assert_eq!(view.initialization, ProjectInitialization::SetupIncomplete);
        // Row order is `ProjectSettingId::ALL`. A key the plan does not carry
        // — `search` here, as in every plan written before the lever rows —
        // keeps the row's own default, which is on.
        assert_eq!(
            view.settings
                .iter()
                .map(|row| row.enabled)
                .collect::<Vec<_>>(),
            vec![false, false, false, true, false, true, true]
        );
        assert_eq!(
            view.integrations
                .iter()
                .filter(|row| row.selected)
                .map(|row| row.agent.as_str())
                .collect::<Vec<_>>(),
            vec!["ollama"]
        );
    }
}
