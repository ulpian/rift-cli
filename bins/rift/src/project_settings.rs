//! Production adapter for Canopy's Project Settings lifecycle seam.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rift_canopy::{
    ProjectInitialization, ProjectSettingId, ProjectSettingsAction, ProjectSettingsActionKind,
    ProjectSettingsManager, ProjectSettingsView, ProjectSetupPlan, SetupProgress, SetupStep,
};
use serde::Serialize;

use crate::cli::{exit, InitArgs};

/// Repository-owned Project Settings manager used by the live Canopy loop.
pub(crate) struct RiftProjectSettingsManager;

impl ProjectSettingsManager for RiftProjectSettingsManager {
    fn load(&mut self, path: &str) -> Result<ProjectSettingsView, String> {
        Ok(rift_canopy_data::project_settings_view(Path::new(path)))
    }

    fn apply(&mut self, action: &ProjectSettingsAction) -> Result<ProjectSettingsView, String> {
        // The one implementation, with the steps dropped: a settings-page act
        // reports its result inline, not step by step.
        self.initialize(action, &mut |_| {})
    }

    fn initialize(
        &mut self,
        action: &ProjectSettingsAction,
        progress: &mut dyn FnMut(SetupProgress),
    ) -> Result<ProjectSettingsView, String> {
        let repo = Path::new(&action.path);
        match &action.kind {
            ProjectSettingsActionKind::SetPermissions(choices) => apply_permissions(repo, choices)?,
            ProjectSettingsActionKind::Set { setting, enabled } => {
                apply_setting(repo, *setting, *enabled)?;
            }
            ProjectSettingsActionKind::SetIntegration { agent, enabled } => {
                set_integration(repo, agent, *enabled)?;
            }
            ProjectSettingsActionKind::Initialize(plan) => initialize(repo, plan, progress)?,
            ProjectSettingsActionKind::Init => init_detected(repo, progress)?,
            ProjectSettingsActionKind::Retry => {}
        }
        Ok(rift_canopy_data::project_settings_view(repo))
    }
}

/// Name a failure on the step it belongs to before it propagates; a success
/// passes through unreported, since the step's own `done` line says more
/// than a generic one could.
fn or_fail<T>(
    progress: &mut dyn FnMut(SetupProgress),
    step: SetupStep,
    result: Result<T, String>,
) -> Result<T, String> {
    if let Err(reason) = &result {
        progress(SetupProgress::failed(step, reason.clone()));
    }
    result
}

/// The add flow's `rift init`: the plan the setup page would have opened
/// with — Recall on, the levers at their defaults, every supported agent
/// detected here pre-ticked — read from the same projection the page reads,
/// so the dialog and the page can never provision different things. A
/// project already set up here is left exactly as it is, and said so; a
/// directory that is not a repository has no family to provision.
fn init_detected(repo: &Path, progress: &mut dyn FnMut(SetupProgress)) -> Result<(), String> {
    let view = rift_canopy_data::project_settings_view(repo);
    match view.initialization {
        ProjectInitialization::Initialized => {
            progress(SetupProgress::skipped(
                SetupStep::Projection,
                "already set up on this machine — nothing changed",
            ));
            return Ok(());
        }
        ProjectInitialization::NotRepository => {
            return Err("not a git repository — there is no family to set up".to_owned());
        }
        ProjectInitialization::NotInitialized
        | ProjectInitialization::NotProvisioned
        | ProjectInitialization::SetupIncomplete => {}
    }
    initialize(repo, &ProjectSetupPlan::from_view(&view), progress)
}

fn apply_setting(repo: &Path, setting: ProjectSettingId, enabled: bool) -> Result<(), String> {
    match setting {
        ProjectSettingId::TotalRecall if enabled => {
            crate::features::recall_install(repo).map(|_| ())
        }
        ProjectSettingId::TotalRecall => crate::features::recall_remove(repo).map(|_| ()),
        ProjectSettingId::TurnInjection => {
            let rift_dir = repo.join(".rift");
            let current = rift_recall_policy::load_serve_policy(&rift_dir);
            let budget = if enabled {
                rift_recall_policy::DEFAULT_INJECTION_TURN_BUDGET
            } else {
                0
            };
            rift_recall_policy::patch_injection_turn_budget(&rift_dir, &current.revision, budget)
                .map(|_| ())
                .map_err(|error| error.to_string())
        }
        ProjectSettingId::Documents | ProjectSettingId::Search => {
            let rift_dir = repo.join(".rift");
            let current = rift_recall_policy::load_serve_policy(&rift_dir);
            let table = if setting == ProjectSettingId::Documents {
                rift_recall_policy::LeverTable::Doc
            } else {
                rift_recall_policy::LeverTable::Search
            };
            rift_recall_policy::patch_lever_mode(
                &rift_dir,
                &current.revision,
                table,
                lever_mode(enabled),
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
        }
        ProjectSettingId::ToolCallCapture
        | ProjectSettingId::TokenUsageCapture
        | ProjectSettingId::RawTranscripts => {
            let current = rift_config::load_project_policy(repo).map_err(|e| e.to_string())?;
            let mut patch = rift_config::ProjectPolicyPatch::default();
            match setting {
                ProjectSettingId::ToolCallCapture => patch.tool_calls = Some(enabled),
                ProjectSettingId::TokenUsageCapture => patch.token_usage = Some(enabled),
                ProjectSettingId::RawTranscripts => patch.raw_transcripts = Some(enabled),
                ProjectSettingId::TotalRecall
                | ProjectSettingId::TurnInjection
                | ProjectSettingId::Documents
                | ProjectSettingId::Search => {}
            }
            rift_config::patch_project_policy(repo, &current.revision, patch)
                .map(|_| ())
                .map_err(|error| error.to_string())
        }
    }
}

/// The binary Project Settings control over a lever table: on is `enforce`,
/// the default every table ships with, and off is `off`.
const fn lever_mode(enabled: bool) -> rift_recall_policy::Mode {
    if enabled {
        rift_recall_policy::Mode::Enforce
    } else {
        rift_recall_policy::Mode::Off
    }
}

fn set_integration(repo: &Path, agent: &str, enabled: bool) -> Result<(), String> {
    let status = rift_agent_install::native::diagnose_all(repo)
        .into_iter()
        .find(|row| row.agent == agent)
        .ok_or_else(|| format!("unknown native adapter {agent:?}"))?;
    if matches!(
        status.wiring,
        rift_agent_install::native::NativeWiring::Conflict
            | rift_agent_install::native::NativeWiring::Invalid
    ) {
        return Err(format!(
            "{} integration is not Rift-managed; repair the conflicting descriptor and retry",
            status.display_name
        ));
    }
    if matches!(
        status.hook_wiring,
        Some(
            rift_agent_install::native::NativeHookWiring::Conflict
                | rift_agent_install::native::NativeHookWiring::Invalid
        )
    ) {
        // The file to repair is the one this agent's hooks live in — named from
        // the installer's own table. It was hard-coded to `.codex/hooks.json`
        // while Codex was the only family with a hook surface Rift wrote; since
        // Claude Code has one too, and a message that named the wrong
        // file would send the user to repair a file that is already fine.
        return Err(format!(
            "{} hooks are not safely Rift-managed; repair {} and retry",
            status.display_name,
            rift_agent_install::native::hook_file(agent).unwrap_or("its hook file"),
        ));
    }
    if enabled == status.enabled() {
        return Ok(());
    }
    // The same pre-emption `initialize` performs, for the same reason:
    // a coming-soon agent's enable is refused as a plain
    // precondition BEFORE the projection opens and a runtime spins up —
    // `native::enable` would only refuse it later, wrapped in a storage-error
    // spelling that names a descriptor path nothing ever wrote. Enable-only:
    // disabling a leftover pre-demotion artifact stays permitted.
    if enabled && !rift_agent_install::native::is_alpha_supported(agent) {
        return Err(format!(
            "capture for {agent} is not supported yet — this alpha captures {}",
            rift_agent_install::native::ALPHA_AGENTS.join(", ")
        ));
    }

    let store = crate::open_projection(repo).map_err(|error| error.to_string())?;
    let runtime = crate::runtime().map_err(|error| error.to_string())?;
    let outcome = if enabled {
        runtime.block_on(crate::agents::enable(
            &store,
            repo,
            agent,
            crate::hooks::now_ms(),
        ))
    } else {
        runtime.block_on(crate::agents::disable(
            &store,
            repo,
            agent,
            crate::hooks::now_ms(),
        ))
    }
    .map_err(|error| error.to_string())?;
    if outcome.code != exit::OK {
        return Err(outcome.human);
    }
    Ok(())
}

fn initialize(
    repo: &Path,
    plan: &ProjectSetupPlan,
    progress: &mut dyn FnMut(SetupProgress),
) -> Result<(), String> {
    let selected: BTreeSet<String> = plan.integrations.iter().cloned().collect();
    refuse_unrunnable_selection(repo, &selected)?;
    // Read BEFORE init scaffolds anything: a `.rift/config.toml` that is
    // already here arrived with the clone, and it is the repository's to
    // keep. Setup on this machine then provisions only what is machine-local
    // — projection, git hooks, agents, skills, ingest — and rewrites no
    // committed policy, not even to a value it already holds.
    let committed_config = repo.join(".rift/config.toml").is_file();
    persist_setup_plan(repo, plan)?;
    apply_permissions(repo, &plan.permissions)?;
    let outcome = crate::cmd_init_with_integrations(
        repo,
        &InitArgs { no_recall: true },
        Some(&selected),
        progress,
    )
    .map_err(|error| error.to_string())?;
    if outcome.code != exit::OK {
        return Err(outcome.human);
    }
    or_fail(
        progress,
        SetupStep::Projection,
        crate::open_projection(repo).map_err(|error| error.to_string()),
    )?;
    install_git_hooks(repo, progress)?;
    reconcile_integrations(repo, &selected, progress)?;

    if committed_config {
        // The committed policy is authoritative. The plan carried its values
        // read-only, so the only act left is the machine-local half of
        // Recall when the repository declares it on.
        progress(SetupProgress::skipped(
            SetupStep::Policy,
            "kept — committed by the repository",
        ));
        let current = rift_config::load_project_policy(repo).map_err(|e| e.to_string())?;
        provision_recall(
            repo,
            current.effective.recall_enabled(),
            false,
            "off in the committed configuration",
            progress,
        )?;
        remove_setup_plan(repo)?;
        return Ok(());
    }

    write_plan_policy(repo, plan, progress)?;
    provision_recall(repo, plan.total_recall, true, "off in the plan", progress)?;
    remove_setup_plan(repo)?;
    Ok(())
}

/// Refuse a selection naming an adapter this build does not know, or one the
/// alpha does not capture, before anything is written.
///
/// A selection that will be refused is refused before the plan is persisted.
/// The reconcile step calls `set_integration` for every selected row, so
/// without this an unsupported agent errored there — after the plan file, the
/// `.rift/` scaffold and the git hooks were on disk. Every offender is named
/// at once.
fn refuse_unrunnable_selection(repo: &Path, selected: &BTreeSet<String>) -> Result<(), String> {
    let known: BTreeSet<String> = rift_agent_install::native::diagnose_all(repo)
        .into_iter()
        .map(|row| row.agent)
        .collect();
    if let Some(agent) = selected.iter().find(|agent| !known.contains(*agent)) {
        return Err(format!("unknown native adapter {agent:?}"));
    }
    let unsupported =
        rift_agent_install::native::unsupported_alpha_agents(selected.iter().map(String::as_str));
    if !unsupported.is_empty() {
        return Err(format!(
            "capture for {} is not supported yet — this alpha captures {}",
            unsupported.join(", "),
            rift_agent_install::native::ALPHA_AGENTS.join(", ")
        ));
    }
    Ok(())
}

/// The GitHooks step, inside a repository only.
///
/// Project Settings provisions the project, which is `init`'s act through
/// another surface — so it records `init` and, like `init`, never re-points a
/// family whose hooks already work. A refused binary is not a setup failure:
/// it leaves the working hooks alone, and `rift doctor`'s `hooks-target` leg
/// is where it is named.
fn install_git_hooks(repo: &Path, progress: &mut dyn FnMut(SetupProgress)) -> Result<(), String> {
    let Ok(paths) = rift_git::discover(repo) else {
        return Ok(());
    };
    let rift_bin = std::env::current_exe().map_err(|error| error.to_string())?;
    or_fail(
        progress,
        SetupStep::GitHooks,
        crate::hooks::install_family_hooks(
            &paths.common_dir,
            repo,
            None,
            rift_bin,
            rift_shadow::hooks::HookInstallPolicy::KeepIfHealthy {
                allow_transient: false,
            },
            rift_shadow::hooks::RecordedBy::Init,
        )
        .map_err(|error| format!("git hook setup failed: {error}")),
    )?;
    Ok(())
}

/// Reconcile revised selections on every attempt.
///
/// Unselected managed artifacts are removed; conflicts are left untouched. A
/// selected row was wired (or refused, and said) by `init` before this runs,
/// so only a failure is news here; a removal is a step of its own.
fn reconcile_integrations(
    repo: &Path,
    selected: &BTreeSet<String>,
    progress: &mut dyn FnMut(SetupProgress),
) -> Result<(), String> {
    for row in rift_agent_install::native::diagnose_all(repo) {
        if selected.contains(&row.agent) {
            or_fail(
                progress,
                SetupStep::Agent(row.display_name.clone()),
                set_integration(repo, &row.agent, true),
            )?;
        } else if row.enabled() {
            or_fail(
                progress,
                SetupStep::Agent(row.display_name.clone()),
                set_integration(repo, &row.agent, false),
            )?;
            progress(SetupProgress::done(
                SetupStep::Agent(row.display_name.clone()),
                "removed — not selected",
            ));
        }
    }
    Ok(())
}

/// The Policy step: the plan's values into the committed file and the
/// serving overlay, each patch naming the revision the one before it wrote.
fn write_plan_policy(
    repo: &Path,
    plan: &ProjectSetupPlan,
    progress: &mut dyn FnMut(SetupProgress),
) -> Result<(), String> {
    let current = rift_config::load_project_policy(repo).map_err(|e| e.to_string())?;
    or_fail(
        progress,
        SetupStep::Policy,
        rift_config::patch_project_policy(
            repo,
            &current.revision,
            rift_config::ProjectPolicyPatch {
                total_recall: Some(plan.total_recall),
                tool_calls: Some(plan.tool_call_capture),
                token_usage: Some(plan.token_usage_capture),
                raw_transcripts: Some(plan.raw_transcripts),
            },
        )
        .map_err(|error| error.to_string()),
    )?;

    let rift_dir = repo.join(".rift");
    let serve = rift_recall_policy::load_serve_policy(&rift_dir);
    let budget = if plan.turn_injection {
        rift_recall_policy::DEFAULT_INJECTION_TURN_BUDGET
    } else {
        0
    };
    let serve = or_fail(
        progress,
        SetupStep::Policy,
        rift_recall_policy::patch_injection_turn_budget(&rift_dir, &serve.revision, budget)
            .map_err(|error| error.to_string()),
    )?;
    // Each patch replaces the file and so the revision the next one must
    // name: the snapshot a patch returns is the one the following patch reads.
    let serve = or_fail(
        progress,
        SetupStep::Policy,
        rift_recall_policy::patch_lever_mode(
            &rift_dir,
            &serve.revision,
            rift_recall_policy::LeverTable::Doc,
            lever_mode(plan.documents),
        )
        .map_err(|error| error.to_string()),
    )?;
    or_fail(
        progress,
        SetupStep::Policy,
        rift_recall_policy::patch_lever_mode(
            &rift_dir,
            &serve.revision,
            rift_recall_policy::LeverTable::Search,
            lever_mode(plan.search),
        )
        .map_err(|error| error.to_string()),
    )?;
    progress(SetupProgress::done(
        SetupStep::Policy,
        ".rift/config.toml and .rift/policies/serve.toml written",
    ));

    Ok(())
}

/// The Recall step. `on` is what the plan — or, for a repository whose
/// configuration is committed, that configuration — says; `declare` is
/// whether this setup may write the key, which the committed case never
/// does. Off with `declare` removes the skills and writes the key false, so
/// the file says what the machine does.
fn provision_recall(
    repo: &Path,
    on: bool,
    declare: bool,
    off_reason: &str,
    progress: &mut dyn FnMut(SetupProgress),
) -> Result<(), String> {
    if on {
        progress(SetupProgress::running(
            SetupStep::Recall,
            "skills and initial ingest",
        ));
        let report = or_fail(
            progress,
            SetupStep::Recall,
            if declare {
                crate::features::recall_install(repo)
            } else {
                crate::features::recall_provision(repo, false)
            },
        )?;
        progress(SetupProgress::done(
            SetupStep::Recall,
            recall_detail(report.integrations.len()),
        ));
    } else {
        if declare {
            or_fail(
                progress,
                SetupStep::Recall,
                crate::features::recall_remove(repo),
            )?;
        }
        progress(SetupProgress::skipped(SetupStep::Recall, off_reason));
    }
    Ok(())
}

/// The Recall step's landed line.
fn recall_detail(agents: usize) -> String {
    format!("skills for {agents} agent(s), initial ingest ran")
}

/// The staged plan on disk. Schema 1 grew the two lever rows after it shipped;
/// a plan written before them reads as their default (on), which is what the
/// bus falls back to for any key a plan does not carry.
#[derive(Serialize)]
struct PersistedSetupPlan<'a> {
    permissions: Vec<serde_json::Value>,
    schema_version: u16,
    total_recall: bool,
    turn_injection: bool,
    documents: bool,
    search: bool,
    tool_call_capture: bool,
    token_usage_capture: bool,
    raw_transcripts: bool,
    integrations: &'a [String],
}

fn setup_plan_path(repo: &Path) -> PathBuf {
    repo.join(".rift/state/project-setup.json")
}

fn persist_setup_plan(repo: &Path, plan: &ProjectSetupPlan) -> Result<(), String> {
    let state_dir = repo.join(".rift/state");
    refuse_link(&repo.join(".rift"), "the .rift directory")?;
    refuse_link(&state_dir, "the setup state directory")?;
    std::fs::create_dir_all(&state_dir).map_err(|error| error.to_string())?;
    let path = setup_plan_path(repo);
    refuse_link(&path, "the setup plan")?;
    let bytes = serde_json::to_vec_pretty(&PersistedSetupPlan {
        schema_version: 1,
        permissions: plan
            .permissions
            .iter()
            .map(|choice| serde_json::json!({ "agent": choice.agent, "preset": choice.preset }))
            .collect(),
        total_recall: plan.total_recall,
        turn_injection: plan.turn_injection,
        documents: plan.documents,
        search: plan.search,
        tool_call_capture: plan.tool_call_capture,
        token_usage_capture: plan.token_usage_capture,
        raw_transcripts: plan.raw_transcripts,
        integrations: &plan.integrations,
    })
    .map_err(|error| error.to_string())?;
    let temporary = state_dir.join(".project-setup.json.tmp");
    refuse_link(&temporary, "the temporary setup plan")?;
    std::fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    #[cfg(windows)]
    if path.exists() {
        std::fs::remove_file(&path).map_err(|error| error.to_string())?;
    }
    std::fs::rename(&temporary, &path).map_err(|error| error.to_string())
}

fn remove_setup_plan(repo: &Path) -> Result<(), String> {
    let path = setup_plan_path(repo);
    refuse_link(&path, "the setup plan")?;
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn refuse_link(path: &Path, label: &str) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "refused {label}: symbolic links are not owned policy state"
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn apply_permissions(
    repo: &Path,
    choices: &[rift_canopy::AgentPermissionChange],
) -> Result<(), String> {
    for choice in choices {
        rift_agent_install::native::permissions::validate_permission(
            repo,
            &choice.agent,
            &choice.preset,
            &choice.revision,
        )
        .map_err(|e| e.to_string())?;
    }
    for choice in choices {
        rift_agent_install::native::permissions::apply_permission(
            repo,
            &choice.agent,
            &choice.preset,
            &choice.revision,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_and_injection_settings_round_trip_through_safe_editors() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        std::fs::create_dir_all(repo.join(".rift")).unwrap();
        let mut manager = RiftProjectSettingsManager;
        for (setting, enabled) in [
            (ProjectSettingId::RawTranscripts, true),
            (ProjectSettingId::TurnInjection, false),
            (ProjectSettingId::TurnInjection, true),
            (ProjectSettingId::RawTranscripts, false),
        ] {
            let view = manager
                .apply(&ProjectSettingsAction {
                    path: repo.to_string_lossy().into_owned(),
                    kind: ProjectSettingsActionKind::Set { setting, enabled },
                })
                .unwrap();
            assert_eq!(
                view.settings
                    .iter()
                    .find(|row| row.id == setting)
                    .unwrap()
                    .enabled,
                enabled
            );
        }
        let config = rift_config::EffectiveConfig::load(&repo.join(".rift")).unwrap();
        assert!(!config.raw_transcripts_enabled());
        let source = std::fs::read_to_string(repo.join(".rift/config.toml")).unwrap();
        assert!(source.contains("acknowledge_risk = false"));
        assert_eq!(
            rift_recall_policy::load_serve_policy(&repo.join(".rift"))
                .policy
                .injection_turn_budget,
            rift_recall_policy::DEFAULT_INJECTION_TURN_BUDGET
        );
    }

    /// The two lever rows write the table's `mode` and nothing else, through
    /// the same conflict-checked editor as injection: off is `off`, on is
    /// `enforce`, the other table is untouched, and the view reads the file
    /// back rather than remembering the toggle.
    #[test]
    fn documents_and_search_round_trip_through_the_serve_editor() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        std::fs::create_dir_all(repo.join(".rift/policies")).unwrap();
        std::fs::write(
            repo.join(".rift/policies/serve.toml"),
            "# mine\n[search]\nmode = \"enforce\"\ngrep_head_limit = 9\n",
        )
        .unwrap();
        let mut manager = RiftProjectSettingsManager;
        for (setting, enabled) in [
            (ProjectSettingId::Documents, false),
            (ProjectSettingId::Search, false),
            (ProjectSettingId::Documents, true),
        ] {
            let view = manager
                .apply(&ProjectSettingsAction {
                    path: repo.to_string_lossy().into_owned(),
                    kind: ProjectSettingsActionKind::Set { setting, enabled },
                })
                .unwrap();
            assert_eq!(
                view.settings
                    .iter()
                    .find(|row| row.id == setting)
                    .unwrap()
                    .enabled,
                enabled,
                "{setting:?} -> {enabled}"
            );
        }
        let serve = rift_recall_policy::load_serve_policy(&repo.join(".rift")).policy;
        assert_eq!(serve.doc.mode, rift_recall_policy::Mode::Enforce);
        assert_eq!(serve.search.mode, rift_recall_policy::Mode::Off);
        assert_eq!(
            serve.search.grep_head_limit, 9,
            "the bound beside the mode survives"
        );
        let source = std::fs::read_to_string(repo.join(".rift/policies/serve.toml")).unwrap();
        assert!(source.starts_with("# mine\n"), "{source}");
    }

    #[test]
    fn pending_plan_is_atomic_and_refuses_a_symlink_target() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let plan = ProjectSetupPlan {
            permissions: Vec::new(),
            total_recall: true,
            turn_injection: true,
            documents: true,
            search: true,
            tool_call_capture: true,
            token_usage_capture: true,
            raw_transcripts: false,
            integrations: vec!["codex".into()],
        };
        persist_setup_plan(repo, &plan).unwrap();
        let text = std::fs::read_to_string(setup_plan_path(repo)).unwrap();
        assert!(text.contains("\"codex\""));
        assert!(!repo.join(".rift/state/.project-setup.json.tmp").exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let plan_path = setup_plan_path(repo);
            std::fs::remove_file(&plan_path).unwrap();
            let outside = repo.join("outside.json");
            std::fs::write(&outside, "keep").unwrap();
            symlink(&outside, &plan_path).unwrap();
            let err = persist_setup_plan(repo, &plan).unwrap_err();
            assert!(err.contains("symbolic links"), "{err}");
            assert_eq!(std::fs::read_to_string(outside).unwrap(), "keep");
        }
    }

    #[test]
    fn integration_conflicts_are_refused_before_projection_or_file_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        std::fs::create_dir_all(repo.join(".rift/agents")).unwrap();
        let descriptor = repo.join(".rift/agents/codex.json");
        std::fs::write(&descriptor, "{ not Rift managed }").unwrap();
        let before = std::fs::read(&descriptor).unwrap();
        let err = set_integration(repo, "codex", false).unwrap_err();
        assert!(
            err.contains("not Rift-managed") || err.contains("conflicting descriptor"),
            "{err}"
        );
        assert_eq!(std::fs::read(descriptor).unwrap(), before);
    }

    /// **The single-toggle twin of `initialize`'s pre-emption: a coming-soon
    /// agent refuses as a PRECONDITION, not a storage error.** Without the guard this path opened the projection and spun a
    /// runtime just to let `native::enable` refuse, and the refusal came back
    /// wrapped as `storage error: <descriptor path>: …` — a path nothing ever
    /// wrote, in an error class nothing storage-shaped produced. The absence
    /// assertions are the calibration: today's fall-through path cannot pass
    /// them, because `open_projection` scaffolds `.rift/` before the refusal.
    #[test]
    fn an_unsupported_integration_enable_is_refused_as_a_precondition_not_storage() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let err = set_integration(repo, "gemini", true).unwrap_err();
        assert!(err.contains("not supported yet"), "{err}");
        assert!(err.contains("gemini"), "{err}");
        assert!(
            err.contains("claude-code, codex, antigravity"),
            "the refusal names what the alpha DOES capture: {err}"
        );
        assert!(
            !err.contains("storage error"),
            "a scope refusal must not claim a storage failure: {err}"
        );
        assert!(
            !repo.join(".rift").exists(),
            "a refused enable must not scaffold .rift/ on its way to refusing"
        );
    }

    /// The guard is enable-only: switching an unsupported agent OFF stays a
    /// permitted no-op, so a leftover pre-demotion artifact can always be
    /// walked back.
    #[test]
    fn disabling_an_unsupported_agent_stays_a_permitted_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        set_integration(repo, "gemini", false).unwrap();
        assert!(!repo.join(".rift").exists());
    }

    /// **A selection this alpha cannot wire is refused before setup writes
    /// anything, and it names every offender at once.**
    ///
    /// The assertions are about the TREE, not about the `Err`. A refusal that
    /// arrives late is still a refusal — and still leaves a persisted setup
    /// plan and a scaffolded `.rift/` behind it, which is what `rift init` did
    /// before this: it refused inside its wiring loop, downstream of the git
    /// hooks. So the plan file and the scaffold are what this checks.
    ///
    /// `ollama` and `cursor` are both selected because `BTreeSet` orders
    /// `cursor` first: an implementation reporting only the first offender
    /// still names `cursor`, and only the `ollama` assertion catches it.
    #[test]
    fn an_unsupported_selection_is_refused_before_setup_writes_anything() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let plan = ProjectSetupPlan {
            permissions: Vec::new(),
            total_recall: true,
            turn_injection: true,
            documents: true,
            search: true,
            tool_call_capture: true,
            token_usage_capture: true,
            raw_transcripts: false,
            integrations: vec!["cursor".into(), "codex".into(), "ollama".into()],
        };
        let err = initialize(repo, &plan, &mut |_| {}).unwrap_err();
        assert!(
            err.contains("cursor"),
            "the refusal names the first offender: {err}"
        );
        assert!(
            err.contains("ollama"),
            "the refusal names EVERY offender, not the first: {err}"
        );
        assert!(
            !setup_plan_path(repo).exists(),
            "a refused selection persisted a setup plan"
        );
        assert!(
            !repo.join(".rift").exists(),
            "a refused selection scaffolded .rift/ before refusing"
        );
    }

    /// The symlink refusal, at every path the persisted setup plan touches.
    /// Owned policy state is a real file this process created; a link is
    /// somebody else's target and writing through it would put repository
    /// state wherever that target points.
    #[cfg(unix)] // creates symlinks, which needs a privilege on Windows
    #[test]
    fn a_symlink_anywhere_on_the_setup_plan_path_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let plan = ProjectSetupPlan {
            permissions: Vec::new(),
            total_recall: true,
            turn_injection: true,
            documents: true,
            search: true,
            tool_call_capture: true,
            token_usage_capture: true,
            raw_transcripts: false,
            integrations: vec!["claude-code".to_owned()],
        };

        // (a) `.rift` itself is a link.
        let repo = tmp.path().join("linked-rift");
        std::fs::create_dir_all(&repo).unwrap();
        std::os::unix::fs::symlink(&elsewhere, repo.join(".rift")).unwrap();
        let err = persist_setup_plan(&repo, &plan).unwrap_err();
        assert!(err.contains("the .rift directory"), "{err}");

        // (b) the state directory is a link.
        let repo = tmp.path().join("linked-state");
        std::fs::create_dir_all(repo.join(".rift")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, repo.join(".rift/state")).unwrap();
        let err = persist_setup_plan(&repo, &plan).unwrap_err();
        assert!(err.contains("the setup state directory"), "{err}");

        // (c) the plan file itself is a link.
        let repo = tmp.path().join("linked-plan");
        std::fs::create_dir_all(repo.join(".rift/state")).unwrap();
        std::os::unix::fs::symlink(
            elsewhere.join("target.json"),
            repo.join(".rift/state/project-setup.json"),
        )
        .unwrap();
        let err = persist_setup_plan(&repo, &plan).unwrap_err();
        assert!(err.contains("the setup plan"), "{err}");
        // Nothing was written through any of the three links.
        assert!(!elsewhere.join("target.json").exists());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);

        // (d) the temporary file is a link — the rename target is owned, but
        //     the bytes would still land through the link first.
        let repo = tmp.path().join("linked-tmp");
        std::fs::create_dir_all(repo.join(".rift/state")).unwrap();
        std::os::unix::fs::symlink(
            elsewhere.join("tmp-target"),
            repo.join(".rift/state/.project-setup.json.tmp"),
        )
        .unwrap();
        let err = persist_setup_plan(&repo, &plan).unwrap_err();
        assert!(err.contains("the temporary setup plan"), "{err}");
        assert!(!elsewhere.join("tmp-target").exists());
    }

    /// A clean repo writes the plan and reads it back; removing it is
    /// idempotent (absent is already removed), and a linked plan is refused
    /// on the removal path too rather than unlinking somebody else's file.
    #[cfg(unix)] // creates symlinks, which needs a privilege on Windows
    #[test]
    fn the_setup_plan_round_trips_and_its_removal_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".rift")).unwrap();
        let plan = ProjectSetupPlan {
            permissions: Vec::new(),
            total_recall: false,
            turn_injection: true,
            documents: true,
            search: true,
            tool_call_capture: false,
            token_usage_capture: true,
            raw_transcripts: true,
            integrations: vec!["codex".to_owned(), "claude-code".to_owned()],
        };

        // Removing a plan that was never written is not an error.
        remove_setup_plan(&repo).unwrap();

        persist_setup_plan(&repo, &plan).unwrap();
        let written = setup_plan_path(&repo);
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&written).unwrap()).unwrap();
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["total_recall"], false);
        assert_eq!(json["raw_transcripts"], true);
        assert_eq!(
            json["integrations"],
            serde_json::json!(["codex", "claude-code"])
        );
        // The temporary file was renamed, not left behind.
        assert!(!repo.join(".rift/state/.project-setup.json.tmp").exists());

        // A second write over an existing plan replaces it.
        persist_setup_plan(&repo, &plan).unwrap();
        assert!(written.exists());

        remove_setup_plan(&repo).unwrap();
        assert!(!written.exists());
        remove_setup_plan(&repo).unwrap();

        // A linked plan is refused rather than unlinked.
        let target = tmp.path().join("victim.json");
        std::fs::write(&target, b"{}").unwrap();
        std::os::unix::fs::symlink(&target, &written).unwrap();
        let err = remove_setup_plan(&repo).unwrap_err();
        assert!(err.contains("the setup plan"), "{err}");
        assert!(target.exists(), "the link target survived");
    }

    /// `refuse_link` answers about the path itself: an ordinary file or
    /// directory passes, an absent path passes (nothing to be linked yet),
    /// and a link — even a dangling one — is refused.
    #[cfg(unix)] // creates symlinks, which needs a privilege on Windows
    #[test]
    fn the_link_guard_refuses_links_and_passes_everything_else() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        refuse_link(&file, "a file").unwrap();
        refuse_link(tmp.path(), "a directory").unwrap();
        refuse_link(&tmp.path().join("absent"), "an absent path").unwrap();

        let dangling = tmp.path().join("dangling");
        std::os::unix::fs::symlink(tmp.path().join("nowhere"), &dangling).unwrap();
        let err = refuse_link(&dangling, "a dangling link").unwrap_err();
        assert!(
            err.contains("symbolic links are not owned policy state"),
            "{err}"
        );
    }
}
