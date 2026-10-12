//! Pure Project Settings contracts shared by the Canopy renderer and its data
//! and lifecycle adapters.

/// A repository policy setting exposed by Project Settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProjectSettingId {
    /// Enable Total Recall ingestion and managed skills.
    TotalRecall,
    /// Inject a bounded Recall delta at the start of each agent turn.
    TurnInjection,
    /// Read documents as Markdown sidecars (`[doc] mode`).
    Documents,
    /// Answer name searches from the index and bound content searches
    /// (`[search] mode`).
    Search,
    /// Capture native tool-call events.
    ToolCallCapture,
    /// Capture provider-reported token usage.
    TokenUsageCapture,
    /// Permit retention of raw transcripts after explicit acknowledgement.
    RawTranscripts,
}

impl ProjectSettingId {
    /// Stable display order across the Recall and Capture sections.
    pub const ALL: [Self; 7] = [
        Self::TotalRecall,
        Self::TurnInjection,
        Self::Documents,
        Self::Search,
        Self::ToolCallCapture,
        Self::TokenUsageCapture,
        Self::RawTranscripts,
    ];

    /// Human-facing row label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::TotalRecall => "Total Recall",
            Self::TurnInjection => "per-turn delta injection",
            Self::Documents => "documents as Markdown",
            Self::Search => "native search",
            Self::ToolCallCapture => "tool-call capture",
            Self::TokenUsageCapture => "token-usage capture",
            Self::RawTranscripts => "raw transcripts",
        }
    }

    /// One-line explanation shown for the focused Project Settings row.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::TotalRecall => {
                "Build and serve git-native project memory; disabling keeps captured data and hooks."
            }
            Self::TurnInjection => {
                "Inject up to 400 tokens of new Recall context per agent turn; Off uses zero."
            }
            Self::Documents => {
                "Hand an agent that opens a .docx, .pdf or spreadsheet the family's Markdown sidecar instead; Off reads the file as it is."
            }
            Self::Search => {
                "Answer a file-name search from Rift's index and bound an uncapped content search; Off leaves every search as written."
            }
            Self::ToolCallCapture => {
                "Record native agent tool calls in the immutable project capture ledger."
            }
            Self::TokenUsageCapture => {
                "Record token counts when the agent reports input, output, or cache usage."
            }
            Self::RawTranscripts => {
                "Retain raw transcripts locally; enabling requires a sensitive-data warning acknowledgement."
            }
        }
    }
}

/// Initialization state of the selected repository **on this machine**.
///
/// "Initialized" is the family's projection existing under this machine's
/// state root — never the presence of `.rift/`, which is committed
/// configuration that travels with a clone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectInitialization {
    /// The family's projection exists here, its hook install is on record,
    /// and no setup plan is pending.
    Initialized,
    /// Neither `.rift/` nor a projection exists.
    NotInitialized,
    /// `.rift/` is committed by the repository but this machine holds no
    /// projection for the family: configured elsewhere, not set up here. The
    /// wizard stages only machine-local steps and leaves the committed
    /// configuration untouched.
    NotProvisioned,
    /// Not inside a git repository: there is no family to provision, so no
    /// wizard is offered.
    NotRepository,
    /// Setup is unfinished: a resumable plan under ignored `.rift/state/`,
    /// or a family carrying a projection that nothing ever wired hooks for.
    SetupIncomplete,
}

impl ProjectInitialization {
    /// Whether Project Settings offers the setup wizard (the `Initialize`
    /// act, and the pre-ticked detected agents that feed it).
    #[must_use]
    pub const fn offers_setup(self) -> bool {
        matches!(
            self,
            Self::NotInitialized | Self::NotProvisioned | Self::SetupIncomplete
        )
    }

    /// Whether the policy toggles edit the committed configuration in place.
    /// Only an initialized project does; during setup they stage a plan, and
    /// a configured-elsewhere project shows its committed values read-only so
    /// setup never dirties the shared file.
    #[must_use]
    pub const fn settings_editable(self) -> bool {
        matches!(
            self,
            Self::Initialized | Self::NotInitialized | Self::SetupIncomplete
        )
    }
}

/// Truthful combined validity of the project and Recall serve policies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectPolicyState {
    /// Both policy documents are valid repository-owned declarations.
    Declared,
    /// Valid secure defaults apply for every absent policy document.
    Default,
    /// At least one declared document is invalid and the affected policy is
    /// fail-closed.
    Invalid,
    /// At least one policy path was refused because it is not an owned regular
    /// file.
    Refused,
}

/// Safe policy facts displayed in the Security/Policy section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectPolicySummary {
    /// Combined source and validity.
    pub state: ProjectPolicyState,
    /// Sanitized remediation; never policy source bytes or sensitive globs.
    pub detail: Option<String>,
    /// Effective sensitive-path count.
    pub sensitive_path_count: usize,
    /// Successfully loaded repository redactor packs.
    pub loaded_redactor_packs: usize,
    /// Redactor packs skipped with a safe diagnostic.
    pub failed_redactor_packs: usize,
}

impl Default for ProjectPolicySummary {
    fn default() -> Self {
        Self {
            state: ProjectPolicyState::Default,
            detail: None,
            sensitive_path_count: 0,
            loaded_redactor_packs: 0,
            failed_redactor_packs: 0,
        }
    }
}

/// One render-ready policy setting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectSettingRow {
    /// Typed setting identity.
    pub id: ProjectSettingId,
    /// Current effective state.
    pub enabled: bool,
    /// Whether the current policy state permits editing.
    pub actionable: bool,
    /// Concise effective-value explanation.
    pub detail: Option<String>,
}

/// Whether the agent is installed on this machine by any evidence the data
/// plane reads (a `PATH` command, a vendor install, a verified IDE extension,
/// a JetBrains plugin), independent of managed wiring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeAgentDetection {
    /// At least one install was found.
    Detected,
    /// No install was found; the integration remains selectable.
    NotDetected,
}

/// Ownership/validity of one Rift native integration descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeAgentWiring {
    /// No descriptor exists.
    Missing,
    /// The descriptor matches Rift's ownership fingerprint.
    Managed,
    /// A descriptor exists but Rift cannot prove ownership.
    Conflict,
    /// The descriptor is malformed.
    Invalid,
}

/// State of an agent's native project-hook surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeAgentHookWiring {
    /// No Rift hook groups exist.
    Missing,
    /// Hook groups match Rift's current ownership fingerprint.
    Managed,
    /// A recognizable older Rift hook bundle can be refreshed safely.
    Stale,
    /// Rift-like hook groups exist but ownership cannot be proved.
    Conflict,
    /// The hook document is malformed.
    Invalid,
}

/// VS Code-compatible editor hosting an agent's extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeAgentIdeHost {
    /// Visual Studio Code stable.
    VsCode,
    /// Visual Studio Code Insiders.
    VsCodeInsiders,
    /// Cursor.
    Cursor,
    /// Windsurf.
    Windsurf,
}

impl NativeAgentIdeHost {
    /// Concise human-facing host name.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::VsCode => "VS Code",
            Self::VsCodeInsiders => "VS Code Insiders",
            Self::Cursor => "Cursor",
            Self::Windsurf => "Windsurf",
        }
    }
}

/// Where one piece of install evidence came from, in the data plane's
/// reporting order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeAgentInstallSource {
    /// A command of the agent's name on `PATH`.
    PathCommand,
    /// The vendor's standalone install (native installer binary or desktop
    /// application bundle).
    VendorInstall,
    /// A verified VS Code-compatible extension.
    IdeExtension(NativeAgentIdeHost),
    /// A JetBrains plugin directory; never carries a runnable binary.
    JetBrainsPlugin,
}

impl NativeAgentInstallSource {
    /// The words a row uses for this source. Spelled exactly as the data
    /// plane spells it, so the dashboard and the CLI name one reason.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::PathCommand => "PATH command".to_owned(),
            Self::VendorInstall => "vendor install".to_owned(),
            Self::IdeExtension(host) => format!("{} extension", host.display_name()),
            Self::JetBrainsPlugin => "JetBrains plugin".to_owned(),
        }
    }
}

/// One verified install of an agent on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeAgentInstallation {
    /// Where the evidence came from.
    pub source: NativeAgentInstallSource,
    /// The version its manifest declares, when it declares one.
    pub version: Option<String>,
}

impl NativeAgentInstallation {
    /// `VS Code extension 2.1.267`, `PATH command`: the source, then the
    /// version when known.
    #[must_use]
    pub fn describe(&self) -> String {
        match &self.version {
            Some(version) => format!("{} {version}", self.source.label()),
            None => self.source.label(),
        }
    }
}

/// One adapter-registry row shown by Project Settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeAgentIntegrationRow {
    /// Stable registry token.
    pub agent: String,
    /// Human-facing adapter name.
    pub display_name: String,
    /// Detection fact.
    pub detection: NativeAgentDetection,
    /// Basename of the first runnable binary any install provides.
    pub executable_name: Option<String>,
    /// Every install found, in the data plane's reporting order.
    pub installations: Vec<NativeAgentInstallation>,
    /// Managed wiring fact.
    pub wiring: NativeAgentWiring,
    /// Separately managed native-hook fact, when applicable.
    pub hook_wiring: Option<NativeAgentHookWiring>,
    /// Whether this alpha's capture scope includes the agent. Provided by the
    /// data plane — rift-canopy never learns the agent list itself, only the
    /// per-row fact, so the scope's single source stays beside the registry.
    pub alpha_supported: bool,
    /// Whether a staged initialization currently selects this integration.
    pub selected: bool,
    /// Every integration-health finding about this agent, as its remedy.
    /// Empty when the agent is healthy, and then the row draws nothing extra.
    pub remedies: Vec<NativeAgentRemedy>,
}

/// One integration-health finding about one agent, as Project Settings prints
/// it under the adapter row.
///
/// The data plane translates the installer's findings into this, so the
/// dashboard never learns the installer's types; the sentence is the one the
/// command line prints for the same finding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeAgentRemedy {
    /// Whether capture is being lost (an error) or the fact is only worth
    /// knowing (a warning), which decides the colour.
    pub severity: NativeAgentRemedySeverity,
    /// One short line: the agent's name and what is wrong.
    pub headline: String,
    /// What to do and where.
    pub sentence: String,
}

/// How much an integration finding matters, as the row colours it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeAgentRemedySeverity {
    /// Capture is being lost, or Rift's own state is wrong.
    Error,
    /// Worth knowing; nothing is being lost.
    Warning,
}

impl NativeAgentIntegrationRow {
    /// Whether this row is currently enabled through Rift-managed wiring.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.wiring == NativeAgentWiring::Managed
            && self
                .hook_wiring
                .is_none_or(|hooks| hooks == NativeAgentHookWiring::Managed)
    }

    /// Whether a compatible local client surface was detected.
    #[must_use]
    pub fn detected(&self) -> bool {
        self.detection == NativeAgentDetection::Detected || !self.installations.is_empty()
    }

    /// The one sentence for install evidence — `absent`, or `present via
    /// <source> [<version>]` per install joined by ` + ` — spelled as the CLI
    /// spells it, so a row and `rift recall status` name the same reason.
    #[must_use]
    pub fn availability(&self) -> String {
        if self.installations.is_empty() {
            return "absent".to_owned();
        }
        format!(
            "present via {}",
            self.installations
                .iter()
                .map(NativeAgentInstallation::describe)
                .collect::<Vec<_>>()
                .join(" + ")
        )
    }
}

/// Complete read-only Project Settings projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectSettingsView {
    /// Canonical selected-project path.
    pub path: String,
    /// Sanitized-at-render project label.
    pub project_name: String,
    /// Initialization/setup state.
    pub initialization: ProjectInitialization,
    /// Effective project policy rows.
    pub settings: Vec<ProjectSettingRow>,
    /// Safe policy diagnostics.
    pub policy: ProjectPolicySummary,
    /// Every native adapter in stable registry order.
    pub integrations: Vec<NativeAgentIntegrationRow>,
}

impl ProjectSettingsView {
    /// Empty fallback used before the data bus supplies a rich view.
    #[must_use]
    pub fn thin(path: String, project_name: String) -> Self {
        Self {
            path,
            project_name,
            initialization: ProjectInitialization::NotInitialized,
            settings: Vec::new(),
            policy: ProjectPolicySummary::default(),
            integrations: Vec::new(),
        }
    }
}

/// Editable choices for one native initialization attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectSetupPlan {
    /// Recall starts enabled by default.
    pub total_recall: bool,
    /// Injection starts at the native 400-token default.
    pub turn_injection: bool,
    /// Documents are read as Markdown by default.
    pub documents: bool,
    /// Native search answers and bounds searches by default.
    pub search: bool,
    /// Native tool-call capture starts enabled.
    pub tool_call_capture: bool,
    /// Provider token-usage capture starts enabled.
    pub token_usage_capture: bool,
    /// Raw transcript retrieval starts on.
    pub raw_transcripts: bool,
    /// Selected adapter tokens.
    pub integrations: Vec<String>,
}

impl ProjectSetupPlan {
    /// The plan a view stands for: each policy row as it is shown, and every
    /// integration row that is ticked. The **one** reading of a view as a
    /// plan — the setup page's `initialize` act and the add flow's `rift
    /// init` both take it from here, so the dialog and the page can never
    /// provision different things from the same rows.
    #[must_use]
    pub fn from_view(view: &ProjectSettingsView) -> Self {
        let enabled = |id| {
            view.settings
                .iter()
                .find(|row| row.id == id)
                .is_some_and(|row| row.enabled)
        };
        Self {
            total_recall: enabled(ProjectSettingId::TotalRecall),
            turn_injection: enabled(ProjectSettingId::TurnInjection),
            documents: enabled(ProjectSettingId::Documents),
            search: enabled(ProjectSettingId::Search),
            tool_call_capture: enabled(ProjectSettingId::ToolCallCapture),
            token_usage_capture: enabled(ProjectSettingId::TokenUsageCapture),
            raw_transcripts: enabled(ProjectSettingId::RawTranscripts),
            integrations: view
                .integrations
                .iter()
                .filter(|row| row.selected)
                .map(|row| row.agent.clone())
                .collect(),
        }
    }
}

/// One stage of a project's setup, as the progress dialog names it. The
/// steps are the ones `rift init` prints, in the order it performs them; an
/// agent is a step of its own so a refusal names the agent and not "agents".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupStep {
    /// The machine-global registry row.
    Registry,
    /// `.rift/config.toml`: scaffolded, or kept as the repository committed it.
    Configuration,
    /// The family's capture projection under the machine's cache root.
    Projection,
    /// The git hooks in the family's common dir.
    GitHooks,
    /// One native agent, by display name.
    Agent(String),
    /// A linked worktree's adoption into its family.
    Worktree,
    /// The committed policy and the serving overlay.
    Policy,
    /// Recall: the skills for the wired agents and the initial ingest.
    Recall,
}

impl SetupStep {
    /// The step's label in the dialog.
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::Registry => "registry",
            Self::Configuration => "configuration",
            Self::Projection => "projection",
            Self::GitHooks => "git hooks",
            Self::Agent(name) => name,
            Self::Worktree => "worktree",
            Self::Policy => "policy",
            Self::Recall => "recall",
        }
    }
}

/// Where one setup step stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupStepStatus {
    /// In flight.
    Running,
    /// Landed.
    Done,
    /// Deliberately not performed, for the reason the detail gives.
    Skipped,
    /// Refused or broken, for the reason the detail gives.
    Failed,
}

/// One report from a setup in flight: a step, where it stands, and the
/// sentence that says so. The manager sends these as the work proceeds; the
/// dialog shows the latest per step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupProgress {
    /// The step reported on.
    pub step: SetupStep,
    /// Where it stands.
    pub status: SetupStepStatus,
    /// What happened, sanitized at draw like every modal line.
    pub detail: String,
}

impl SetupProgress {
    /// A step that has started.
    #[must_use]
    pub fn running(step: SetupStep, detail: impl Into<String>) -> Self {
        Self::at(step, SetupStepStatus::Running, detail)
    }

    /// A step that landed.
    #[must_use]
    pub fn done(step: SetupStep, detail: impl Into<String>) -> Self {
        Self::at(step, SetupStepStatus::Done, detail)
    }

    /// A step deliberately not performed.
    #[must_use]
    pub fn skipped(step: SetupStep, detail: impl Into<String>) -> Self {
        Self::at(step, SetupStepStatus::Skipped, detail)
    }

    /// A step that was refused or broke.
    #[must_use]
    pub fn failed(step: SetupStep, detail: impl Into<String>) -> Self {
        Self::at(step, SetupStepStatus::Failed, detail)
    }

    fn at(step: SetupStep, status: SetupStepStatus, detail: impl Into<String>) -> Self {
        Self {
            step,
            status,
            detail: detail.into(),
        }
    }

    /// The dialog's line for this report: a two-letter mark that renders the
    /// same on every tier (no glyph the ASCII backend would have to stand
    /// in for), the label padded to one column, then the detail.
    #[must_use]
    pub fn line(&self) -> String {
        let mark = match self.status {
            SetupStepStatus::Running => "..",
            SetupStepStatus::Done => "ok",
            SetupStepStatus::Skipped => "--",
            SetupStepStatus::Failed => "!!",
        };
        let label = self.step.label();
        if self.detail.is_empty() {
            format!(" {mark}  {label}")
        } else {
            format!(" {mark}  {label:<14} {}", self.detail)
        }
    }
}

/// The dialog's log of a setup: one entry per step in first-seen order, the
/// latest report replacing the earlier one for the same step — so a step
/// that started and then landed is one line, not two.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetupLog {
    entries: Vec<SetupProgress>,
}

impl SetupLog {
    /// Record one report.
    pub fn record(&mut self, progress: SetupProgress) {
        match self
            .entries
            .iter_mut()
            .find(|entry| entry.step == progress.step)
        {
            Some(entry) => *entry = progress,
            None => self.entries.push(progress),
        }
    }

    /// The lines the dialog shows, one per step.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.entries.iter().map(SetupProgress::line).collect()
    }

    /// Whether any step failed.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.status == SetupStepStatus::Failed)
    }
}

/// One requested Project Settings operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectSettingsAction {
    /// Canonical selected-project path.
    pub path: String,
    /// Requested operation.
    pub kind: ProjectSettingsActionKind,
}

/// Project Settings lifecycle operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectSettingsActionKind {
    /// Set a policy-backed row.
    Set {
        /// Setting to update.
        setting: ProjectSettingId,
        /// Desired state.
        enabled: bool,
    },
    /// Enable or disable one native adapter.
    SetIntegration {
        /// Stable registry token.
        agent: String,
        /// Desired managed-wiring state.
        enabled: bool,
    },
    /// Initialize or reconcile an incomplete project setup.
    Initialize(ProjectSetupPlan),
    /// Set the project up as `rift init` would: the default plan — Recall
    /// on, the levers at their defaults, every supported agent detected on
    /// this machine — read by the manager from the same projection the
    /// setup page opens with, at apply time. The add flow's act; a plan the
    /// developer edited on the page travels as [`Self::Initialize`].
    Init,
    /// Retry the last failed operation after external repair.
    Retry,
}

/// Injected write seam. Canopy owns interaction; the binary owns files,
/// ingestion, hooks, and integration lifecycle work.
pub trait ProjectSettingsManager {
    /// Re-read effective state without mutating the repository.
    ///
    /// # Errors
    /// Returns a sanitized policy or filesystem failure.
    fn load(&mut self, path: &str) -> Result<ProjectSettingsView, String>;

    /// Apply one operation, verify it, and return the refreshed state.
    ///
    /// # Errors
    /// Returns a sanitized lifecycle, ownership, policy, or filesystem failure.
    fn apply(&mut self, action: &ProjectSettingsAction) -> Result<ProjectSettingsView, String>;

    /// Apply one operation as [`Self::apply`] does, reporting each setup
    /// step to `progress` as it starts and lands, so a surface can show a
    /// setup while it runs. The default reports nothing and applies as
    /// usual — an embedder without write authority has no steps to report.
    ///
    /// # Errors
    /// As [`Self::apply`].
    fn initialize(
        &mut self,
        action: &ProjectSettingsAction,
        progress: &mut dyn FnMut(SetupProgress),
    ) -> Result<ProjectSettingsView, String> {
        let _ = progress;
        self.apply(action)
    }
}

/// Inert manager for pure tests and embedders that have no write authority.
#[derive(Debug, Default)]
pub struct NullProjectSettingsManager;

impl ProjectSettingsManager for NullProjectSettingsManager {
    fn load(&mut self, path: &str) -> Result<ProjectSettingsView, String> {
        Ok(ProjectSettingsView::thin(
            path.to_owned(),
            path.rsplit('/').next().unwrap_or(path).to_owned(),
        ))
    }

    fn apply(&mut self, action: &ProjectSettingsAction) -> Result<ProjectSettingsView, String> {
        self.load(&action.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setup_log_keeps_one_line_per_step_in_first_seen_order() {
        let mut log = SetupLog::default();
        log.record(SetupProgress::running(SetupStep::Registry, ""));
        log.record(SetupProgress::running(
            SetupStep::Agent("Codex".to_owned()),
            "wiring",
        ));
        log.record(SetupProgress::done(SetupStep::Registry, "demo registered"));
        assert_eq!(
            log.lines(),
            vec![
                " ok  registry       demo registered".to_owned(),
                " ..  Codex          wiring".to_owned(),
            ]
        );
        assert!(!log.failed());
        log.record(SetupProgress::failed(
            SetupStep::Agent("Codex".to_owned()),
            "not Rift-managed",
        ));
        assert!(log.failed());
        assert_eq!(log.lines().len(), 2, "a failure replaces, never appends");
        assert_eq!(
            SetupProgress::skipped(SetupStep::Policy, "kept").line(),
            " --  policy         kept"
        );
        // Every mark is ASCII: the dialog renders the same on every tier.
        for line in log.lines() {
            assert!(line.is_ascii(), "{line}");
        }
    }

    #[test]
    fn a_plan_read_from_a_view_takes_the_shown_rows_and_the_ticked_agents() {
        let mut view = ProjectSettingsView::thin("/work/demo".to_owned(), "demo".to_owned());
        view.settings = ProjectSettingId::ALL
            .iter()
            .map(|id| ProjectSettingRow {
                id: *id,
                enabled: !matches!(
                    *id,
                    ProjectSettingId::RawTranscripts | ProjectSettingId::Search
                ),
                actionable: true,
                detail: None,
            })
            .collect();
        let row = |agent: &str, selected: bool| NativeAgentIntegrationRow {
            agent: agent.to_owned(),
            display_name: agent.to_owned(),
            detection: NativeAgentDetection::Detected,
            executable_name: None,
            installations: Vec::new(),
            wiring: NativeAgentWiring::Missing,
            hook_wiring: None,
            alpha_supported: true,
            selected,
            remedies: Vec::new(),
        };
        view.integrations = vec![
            row("claude-code", true),
            row("codex", false),
            row("antigravity", true),
        ];
        let plan = ProjectSetupPlan::from_view(&view);
        assert!(plan.total_recall && plan.turn_injection && plan.documents);
        assert!(plan.tool_call_capture && plan.token_usage_capture);
        assert!(!plan.raw_transcripts && !plan.search);
        assert_eq!(plan.integrations, vec!["claude-code", "antigravity"]);
        // A thin view (no rows yet) plans nothing on and wires nobody: the
        // manager, not the view, supplies the defaults in that case.
        let thin = ProjectSetupPlan::from_view(&ProjectSettingsView::thin(
            "/w".to_owned(),
            "w".to_owned(),
        ));
        assert!(!thin.total_recall && thin.integrations.is_empty());
    }

    #[test]
    fn the_null_manager_initializes_silently() {
        let mut manager = NullProjectSettingsManager;
        let mut reported = 0;
        let view = manager
            .initialize(
                &ProjectSettingsAction {
                    path: "/work/demo".to_owned(),
                    kind: ProjectSettingsActionKind::Init,
                },
                &mut |_| reported += 1,
            )
            .unwrap();
        assert_eq!(view.project_name, "demo");
        assert_eq!(reported, 0, "no write authority, no steps");
    }

    #[test]
    fn setting_order_and_labels_are_stable() {
        assert_eq!(ProjectSettingId::ALL.len(), 7);
        assert_eq!(ProjectSettingId::ALL[0].label(), "Total Recall");
        assert_eq!(ProjectSettingId::ALL[2].label(), "documents as Markdown");
        assert_eq!(ProjectSettingId::ALL[3].label(), "native search");
        assert_eq!(ProjectSettingId::ALL[6].label(), "raw transcripts");
    }

    #[test]
    fn the_null_manager_has_a_safe_uninitialized_view() {
        let mut manager = NullProjectSettingsManager;
        let view = manager.load("/work/demo").unwrap();
        assert_eq!(view.project_name, "demo");
        assert_eq!(view.initialization, ProjectInitialization::NotInitialized);
    }

    #[test]
    fn an_ide_extension_is_detection_without_claiming_a_cli() {
        let row = NativeAgentIntegrationRow {
            agent: "codex".to_owned(),
            display_name: "Codex".to_owned(),
            detection: NativeAgentDetection::Detected,
            executable_name: None,
            installations: vec![NativeAgentInstallation {
                source: NativeAgentInstallSource::IdeExtension(NativeAgentIdeHost::VsCode),
                version: Some("26.803.61601".to_owned()),
            }],
            wiring: NativeAgentWiring::Managed,
            hook_wiring: Some(NativeAgentHookWiring::Managed),
            alpha_supported: true,
            selected: true,
            remedies: Vec::new(),
        };
        assert!(row.detected());
        assert!(row.enabled());
        assert!(row.executable_name.is_none());
        assert_eq!(
            row.availability(),
            "present via VS Code extension 26.803.61601"
        );

        let absent = NativeAgentIntegrationRow {
            detection: NativeAgentDetection::NotDetected,
            installations: Vec::new(),
            ..row.clone()
        };
        assert!(!absent.detected());
        assert_eq!(absent.availability(), "absent");

        let several = NativeAgentIntegrationRow {
            installations: vec![
                NativeAgentInstallation {
                    source: NativeAgentInstallSource::PathCommand,
                    version: None,
                },
                NativeAgentInstallation {
                    source: NativeAgentInstallSource::JetBrainsPlugin,
                    version: Some("0.1.15".to_owned()),
                },
            ],
            ..row
        };
        assert_eq!(
            several.availability(),
            "present via PATH command + JetBrains plugin 0.1.15"
        );
        assert_eq!(
            NativeAgentInstallSource::VendorInstall.label(),
            "vendor install"
        );
    }
}
