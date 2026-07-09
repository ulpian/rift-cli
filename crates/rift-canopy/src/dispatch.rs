//! The injected command dispatcher (ADR-0021) — the palette's ONLY door to
//! the CLI surface.
//!
//! A typed [`DispatchRequest`] (mirroring exactly the two delegation tiers of
//! the closed grammar) goes in with an explicit repository path — the
//! **selected project's**, never process cwd; an Outcome-shaped
//! [`DispatchOutcome`] (exit-code class, human text, optional JSON) comes
//! back. The binary implements [`CommandDispatcher`] over its private
//! subcommand dispatch and injects it at launch exactly as it injects the
//! persist hook; loop tests use a mock. Nothing here can spawn a process:
//! the trait is synchronous, in-process, and takes no command strings.

use crate::command::{CheckpointVerb, ConfigureVerb, MemoryVerb, SessionVerb, Verb};

/// A delegated request: structured argv-shaped fields, never a command line.
/// Mirrors the grammar's two delegation tiers one-to-one (the drift guard
/// test in the binary maps every variant onto a real clap path).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchRequest {
    // ---- read-only ---------------------------------------------------------
    /// One-shot health snapshot.
    Status,
    /// The recall plane's human surface.
    Memory(MemoryVerb),
    /// Inspect checkpoints.
    Checkpoint(CheckpointVerb),
    /// Inspect sessions.
    Session(SessionVerb),
    /// Explain a checkpoint reference.
    Explain(String),
    /// Backend + memory-chain readiness.
    Doctor,
    /// Re-derive + cross-check the chain.
    Verify,
    /// gc, deleting nothing.
    GcDryRun,
    /// List registered projects.
    ProjectsList,
    /// Prune preview, writing nothing.
    ProjectsPruneDryRun,
    // ---- mutating ----------------------------------------------------------
    /// Scaffold + install + init.
    Init,
    /// Wire an agent's hooks.
    EnableAgent(String),
    /// Remove an agent's wiring.
    DisableAgent(String),
    /// Configure (currently: show, or set the splash mode).
    Configure(ConfigureVerb),
    /// Rebuild the projection from the checkpoint branch.
    Rebuild,
    /// Prune transient refs/objects.
    Gc,
    /// Sweep path-missing registry rows.
    ProjectsPrune,
}

impl DispatchRequest {
    /// Whether this request mutates (confirm-in-UI first).
    #[must_use]
    pub fn is_mutating(&self) -> bool {
        matches!(
            self,
            Self::Init
                | Self::EnableAgent(_)
                | Self::DisableAgent(_)
                | Self::Configure(_)
                | Self::Rebuild
                | Self::Gc
                | Self::ProjectsPrune
        )
    }

    /// The verb line shown as the overlay header / confirm title.
    #[must_use]
    pub fn verb_line(&self) -> String {
        match self {
            Self::Status => "rift status".into(),
            Self::Memory(m) => match m {
                MemoryVerb::DiffSince(p) => format!("rift memory diff-since {p}"),
                MemoryVerb::FileIfChanged(p) => format!("rift memory file-if-changed {p}"),
                MemoryVerb::CostHotspots(Some(n)) => {
                    format!("rift memory cost-hotspots --top {n}")
                }
                MemoryVerb::CostHotspots(None) => "rift memory cost-hotspots".into(),
                MemoryVerb::RereadReport => "rift memory reread-report".into(),
            },
            Self::Checkpoint(CheckpointVerb::List) => "rift checkpoint list".into(),
            Self::Checkpoint(CheckpointVerb::Show(id)) => format!("rift checkpoint show {id}"),
            Self::Session(SessionVerb::List) => "rift session list".into(),
            Self::Session(SessionVerb::Show(id)) => format!("rift session show {id}"),
            Self::Explain(r) => format!("rift explain {r}"),
            Self::Doctor => "rift doctor".into(),
            Self::Verify => "rift verify".into(),
            Self::GcDryRun => "rift gc --dry-run".into(),
            Self::ProjectsList => "rift projects list".into(),
            Self::ProjectsPruneDryRun => "rift projects prune --dry-run".into(),
            Self::Init => "rift init".into(),
            Self::EnableAgent(a) => format!("rift enable --agent {a}"),
            Self::DisableAgent(a) => format!("rift disable --agent {a}"),
            Self::Configure(ConfigureVerb::Show) => "rift configure".into(),
            Self::Configure(ConfigureVerb::Splash(m)) => {
                format!("rift configure splash {}", m.as_str())
            }
            Self::Rebuild => "rift rebuild".into(),
            Self::Gc => "rift gc".into(),
            Self::ProjectsPrune => "rift projects prune".into(),
        }
    }
}

/// Map a delegable verb onto its request. `None` for the dashboard tier
/// (those dispatch to typed panel effects, never the CLI).
#[must_use]
pub fn request_for(verb: &Verb) -> Option<DispatchRequest> {
    let req = match verb {
        Verb::Status => DispatchRequest::Status,
        Verb::Memory(m) => DispatchRequest::Memory(m.clone()),
        Verb::Checkpoint(c) => DispatchRequest::Checkpoint(c.clone()),
        Verb::Session(s) => DispatchRequest::Session(s.clone()),
        Verb::Explain(r) => DispatchRequest::Explain(r.clone()),
        Verb::Doctor => DispatchRequest::Doctor,
        Verb::Verify => DispatchRequest::Verify,
        Verb::GcDryRun => DispatchRequest::GcDryRun,
        Verb::ProjectsList => DispatchRequest::ProjectsList,
        Verb::ProjectsPruneDryRun => DispatchRequest::ProjectsPruneDryRun,
        Verb::Init => DispatchRequest::Init,
        Verb::EnableAgent(a) => DispatchRequest::EnableAgent(a.clone()),
        Verb::DisableAgent(a) => DispatchRequest::DisableAgent(a.clone()),
        Verb::Configure(c) => DispatchRequest::Configure(c.clone()),
        Verb::Rebuild => DispatchRequest::Rebuild,
        Verb::Gc => DispatchRequest::Gc,
        Verb::ProjectsPrune => DispatchRequest::ProjectsPrune,
        Verb::Select(_)
        | Verb::Add
        | Verb::Remove(_)
        | Verb::Relabel(..)
        | Verb::Toggle(_)
        | Verb::Reinstall(_)
        | Verb::Focus(_)
        | Verb::Help
        | Verb::Quit => return None,
    };
    Some(req)
}

/// The class of a completed dispatch, folded from the CLI's exit-code
/// contract so the overlay footer can state a failure plainly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutcomeClass {
    /// Exit 0.
    Ok,
    /// Usage error (2).
    Usage,
    /// Not initialized (3).
    NotInit,
    /// Precondition failed (4).
    Precondition,
    /// Integrity refusal (65).
    Integrity,
    /// Internal error (70).
    Internal,
    /// Transient, retryable (75).
    Retryable,
    /// Any other nonzero.
    Failure,
}

impl OutcomeClass {
    /// Fold an exit code into its class.
    #[must_use]
    pub fn from_code(code: i32) -> Self {
        match code {
            0 => Self::Ok,
            2 => Self::Usage,
            3 => Self::NotInit,
            4 => Self::Precondition,
            65 => Self::Integrity,
            70 => Self::Internal,
            75 => Self::Retryable,
            _ => Self::Failure,
        }
    }

    /// The lowercase token for the overlay footer.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Usage => "usage",
            Self::NotInit => "not-initialized",
            Self::Precondition => "precondition",
            Self::Integrity => "integrity",
            Self::Internal => "internal",
            Self::Retryable => "transient",
            Self::Failure => "failure",
        }
    }
}

/// What a dispatch produced: the exit code (class-foldable), the human text
/// the overlay renders (sanitized line-by-line at draw), and the optional
/// JSON document (carried for future scripting surfaces; unrendered).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchOutcome {
    /// The process-style exit code.
    pub code: i32,
    /// Human-readable text (the overlay body).
    pub human: String,
    /// The `--json` document, serialized (optional).
    pub json: Option<String>,
}

impl DispatchOutcome {
    /// The folded class.
    #[must_use]
    pub fn class(&self) -> OutcomeClass {
        OutcomeClass::from_code(self.code)
    }
}

/// The bound on overlay body lines — a runaway outcome is truncated with an
/// honest notice, never rendered unbounded.
pub const OVERLAY_MAX_LINES: usize = 400;

/// The severity of a modal message (ADR-0029): the one axis the view resolves
/// into a border/title colour, so styling is decided once at draw — not per call
/// site. Colour-only, so the mono/ASCII tiers drop it and the glyphs stand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// A failure — a red frame (an error the developer must read).
    Error,
    /// An actionable warning — an amber frame (a soft miss, e.g. "no match").
    Warning,
    /// A success — a green frame (auto-dismissing under `Notice`).
    Success,
    /// Neutral information — the accent frame (the first-run wizard).
    Info,
}

/// The action a message offers beyond dismissal (ADR-0029). A **closed** enum —
/// the modal grammar stays closed (no arbitrary callbacks leak in), so a message
/// is data, not a capability. `Dismiss` is Close/Esc only; `AddProject` adds the
/// first-run wizard's single *add a project* button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageAction {
    /// Dismiss-only: the universal Close X and Esc (an error alert, a notice).
    Dismiss,
    /// A single primary *add a project* button — the first-run wizard modal.
    AddProject,
}

impl MessageAction {
    /// The primary button's label, or `None` for a dismiss-only message.
    #[must_use]
    pub fn primary_label(self) -> Option<&'static str> {
        match self {
            MessageAction::AddProject => Some("Add a project"),
            MessageAction::Dismiss => None,
        }
    }
}

/// A modal message — the data every `Alert`/`Notice` renders from (ADR-0029):
/// `{ title, body, severity, action }`. Opening and closing route through the
/// one overlay path, so severity styling, focus defaults (ADR-0027), and
/// geometry (ADR-0026) are decided once, not re-implemented per error site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// The modal title (severity-styled, sanitized at draw).
    pub title: String,
    /// The body lines (sanitized at draw, like the results body).
    pub body: Vec<String>,
    /// The severity — the view's single styling axis.
    pub severity: Severity,
    /// The action beyond dismissal (`Dismiss` for a plain message).
    pub action: MessageAction,
}

impl Message {
    /// A dismiss-only error message (a red frame) — a failure to read.
    #[must_use]
    pub fn error(title: impl Into<String>, body: Vec<String>) -> Self {
        Self {
            title: title.into(),
            body,
            severity: Severity::Error,
            action: MessageAction::Dismiss,
        }
    }

    /// A dismiss-only warning message (an amber frame) — a soft miss.
    #[must_use]
    pub fn warning(title: impl Into<String>, body: Vec<String>) -> Self {
        Self {
            title: title.into(),
            body,
            severity: Severity::Warning,
            action: MessageAction::Dismiss,
        }
    }

    /// A dismiss-only success message (a green frame) — opened as a `Notice`.
    #[must_use]
    pub fn success(title: impl Into<String>, body: Vec<String>) -> Self {
        Self {
            title: title.into(),
            body,
            severity: Severity::Success,
            action: MessageAction::Dismiss,
        }
    }
}

/// The modal surface over the body (ADR-0021 "Dispatch posture"): read-only
/// outcomes render in the Results variant; mutating requests confirm first;
/// Running is the frame shown while a synchronous dispatch holds the loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Overlay {
    /// A completed outcome: header (the verb line), sanitized bounded body,
    /// a footer naming a nonzero class plainly, scroll offset.
    Results {
        /// The verb line.
        title: String,
        /// Sanitized, bounded body lines.
        lines: Vec<String>,
        /// Top visible line.
        scroll: usize,
        /// `verify: FAILED — integrity (65)`-style footer for nonzero exits.
        footer: Option<String>,
        /// Whether the body was truncated at [`OVERLAY_MAX_LINES`].
        truncated: bool,
    },
    /// A mutating request awaiting Enter/Confirm (Esc/Cancel dismisses;
    /// every other key is INERT so a buffered keystroke can never confirm).
    Confirm {
        /// The request to run on confirm.
        request: DispatchRequest,
        /// The resolved target repository it will run against.
        repo: String,
    },
    /// The frame shown while the dispatch holds the loop.
    Running {
        /// The verb line being run.
        title: String,
    },
    /// A blocking message (ADR-0029): an error, or a message with a primary
    /// action such as the first-run wizard. Dismiss-only unless its action
    /// offers more; it holds the surface until the developer dismisses it.
    Alert(Message),
    /// A brief success/info message (ADR-0029) that auto-dismisses by `dt` under
    /// the ADR-0022 animator contract — a bounded settle, never a timer/thread.
    Notice(Message),
}

impl Overlay {
    /// A blocking [`Overlay::Alert`] from a title, body lines, and severity —
    /// dismiss-only (Close/Esc). The single door error sites open, so severity
    /// styling and focus/geometry are decided once (ADR-0029).
    #[must_use]
    pub fn alert(title: impl Into<String>, body: Vec<String>, severity: Severity) -> Self {
        Overlay::Alert(Message {
            title: title.into(),
            body,
            severity,
            action: MessageAction::Dismiss,
        })
    }

    /// A brief [`Overlay::Notice`] (a success/info toast) that auto-dismisses by
    /// `dt`. Opened via `AppState::open_notice` so its settle is armed.
    #[must_use]
    pub fn notice(title: impl Into<String>, body: Vec<String>) -> Self {
        Overlay::Notice(Message {
            title: title.into(),
            body,
            severity: Severity::Success,
            action: MessageAction::Dismiss,
        })
    }

    /// The first-run wizard as a modal (ADR-0029): a welcome message plus the one
    /// *add a project* primary action. Raised when the registry is empty.
    #[must_use]
    pub fn wizard() -> Self {
        Overlay::Alert(Message {
            title: "welcome to Rift".into(),
            body: vec![
                "No projects registered yet.".into(),
                "Add a project to start capturing agent sessions,".into(),
                "or run `rift init` in a repository.".into(),
            ],
            severity: Severity::Info,
            action: MessageAction::AddProject,
        })
    }

    /// The severity of a message overlay (`Alert`/`Notice`), or `None` for the
    /// results/confirm/running frames (which carry their own accent).
    #[must_use]
    pub fn severity(&self) -> Option<Severity> {
        match self {
            Overlay::Alert(m) | Overlay::Notice(m) => Some(m.severity),
            _ => None,
        }
    }
}

/// A focusable control within an open modal — where the focus ring can land
/// (ADR-0027). Enter/Space activate the button controls; `Body` is the
/// scrollable results region (Enter is inert; j/k and the wheel scroll it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayControl {
    /// The scrollable results body.
    Body,
    /// The confirm button (runs the pending mutating dispatch).
    Confirm,
    /// The cancel button (dismisses a confirm).
    Cancel,
    /// The close X (dismisses the modal).
    Close,
    /// A message's primary action button (e.g. the wizard's *add a project*).
    Primary,
}

impl Overlay {
    /// The focus ring's ordered controls for this modal (ADR-0027). Index 0 is
    /// the default focus — the control "open, press Enter" acts on — so a
    /// confirm defaults to Confirm and a results listing to its (scrollable)
    /// Body. Running has no focusable controls.
    #[must_use]
    pub fn controls(&self) -> &'static [OverlayControl] {
        use OverlayControl::{Body, Cancel, Close, Confirm, Primary};
        match self {
            Overlay::Results { .. } => &[Body, Close],
            Overlay::Confirm { .. } => &[Confirm, Cancel, Close],
            Overlay::Running { .. } => &[],
            // A message defaults its focus to the primary action when it has
            // one (the wizard opens with *add project* focused), else to Close.
            Overlay::Alert(m) => match m.action {
                MessageAction::AddProject => &[Primary, Close],
                MessageAction::Dismiss => &[Close],
            },
            Overlay::Notice(_) => &[Close],
        }
    }
}

impl Overlay {
    /// Build the Results overlay from a completed outcome: body sanitized
    /// line-by-line, bounded with an honest truncation notice, nonzero exits
    /// named plainly in the footer.
    #[must_use]
    pub fn results(request: &DispatchRequest, outcome: &DispatchOutcome) -> Self {
        let mut lines: Vec<String> = outcome
            .human
            .lines()
            .take(OVERLAY_MAX_LINES)
            .map(crate::col2::sanitize_for_terminal)
            .collect();
        let total = outcome.human.lines().count();
        let truncated = total > OVERLAY_MAX_LINES;
        if truncated {
            lines.push(format!(
                "… truncated ({} more line(s))",
                total.saturating_sub(OVERLAY_MAX_LINES)
            ));
        }
        let footer = (outcome.code != 0).then(|| {
            let verb = request
                .verb_line()
                .split_whitespace()
                .nth(1)
                .unwrap_or("command")
                .to_owned();
            format!(
                "{verb}: FAILED — {} ({})",
                outcome.class().as_str(),
                outcome.code
            )
        });
        Self::Results {
            title: request.verb_line(),
            lines,
            scroll: 0,
            footer,
            truncated,
        }
    }
}

/// The injected dispatcher seam. The binary implements it over its private
/// subcommand router; loop tests use a recording/scripted mock. `repo` is the
/// resolved target repository (the selected project's path) — implementations
/// must never consult process cwd.
pub trait CommandDispatcher {
    /// Run one delegated request against `repo`, synchronously.
    fn dispatch(&mut self, req: &DispatchRequest, repo: &std::path::Path) -> DispatchOutcome;
}

/// A dispatcher for contexts with no CLI behind them (headless snapshots,
/// old tests): every request answers with a plain "unavailable" outcome.
pub struct NullDispatcher;

impl CommandDispatcher for NullDispatcher {
    fn dispatch(&mut self, req: &DispatchRequest, _repo: &std::path::Path) -> DispatchOutcome {
        DispatchOutcome {
            code: 4,
            human: format!(
                "{} is unavailable here (no dispatcher injected)",
                req.verb_line()
            ),
            json: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{parse, Tier};
    use proptest::prelude::*;

    /// Every delegable grammar verb maps to a request whose mutability agrees
    /// with the verb's tier — and every dashboard verb maps to none. The
    /// grammar and the dispatcher cannot drift.
    #[test]
    fn request_mapping_matches_the_tier_table() {
        let lines = [
            "/status",
            "/memory reread-report",
            "/memory diff-since abc",
            "/memory file-if-changed src/main.rs",
            "/memory cost-hotspots 3",
            "/checkpoint list",
            "/checkpoint show ab12",
            "/session list",
            "/session show s1",
            "/explain ab12",
            "/doctor",
            "/verify",
            "/gc --dry-run",
            "/projects list",
            "/projects prune --dry-run",
            "/init",
            "/enable claude-code",
            "/disable claude-code",
            "/configure",
            "/configure splash never",
            "/rebuild",
            "/gc",
            "/projects prune",
            "/select",
            "/add",
            "/remove",
            "/relabel x",
            "/toggle fff",
            "/reinstall fff",
            "/focus graphs",
            "/help",
            "/quit",
        ];
        for line in lines {
            let verb = parse(line).unwrap_or_else(|e| panic!("{line}: {e:?}"));
            match (verb.tier(), request_for(&verb)) {
                (Tier::Dashboard, None) => {}
                (Tier::ReadOnly, Some(req)) => {
                    assert!(!req.is_mutating(), "{line} must be read-only");
                }
                (Tier::Mutating, Some(req)) => {
                    assert!(req.is_mutating(), "{line} must confirm first");
                }
                (tier, req) => panic!("{line}: tier {tier:?} vs request {req:?}"),
            }
        }
    }

    proptest::proptest! {
        /// Every overlay body line is sanitized for ARBITRARY outcome bytes: no
        /// control characters (so a crafted outcome can never repaint the
        /// terminal from the palette), and the body is bounded with an honest
        /// truncation notice.
        #[test]
        fn results_overlay_sanitizes_and_bounds_arbitrary_outcomes(text in ".{0,2000}", code in 0i32..80) {
            let outcome = DispatchOutcome { code, human: text, json: None };
            let overlay = Overlay::results(&DispatchRequest::Status, &outcome);
            if let Overlay::Results { lines, truncated, footer, .. } = overlay {
                prop_assert!(lines.len() <= OVERLAY_MAX_LINES + 1);
                for line in &lines {
                    prop_assert!(
                        !line.chars().any(|c| c.is_control() && c != '\t'),
                        "control char survived sanitization: {line:?}"
                    );
                }
                let _ = truncated;
                if code != 0 {
                    prop_assert!(footer.is_some(), "a nonzero exit names its class");
                }
            } else {
                prop_assert!(false, "results() builds Results");
            }
        }
    }

    #[test]
    fn outcome_classes_fold_the_exit_contract() {
        assert_eq!(OutcomeClass::from_code(0), OutcomeClass::Ok);
        assert_eq!(OutcomeClass::from_code(65), OutcomeClass::Integrity);
        assert_eq!(OutcomeClass::from_code(1), OutcomeClass::Failure);
        assert_eq!(OutcomeClass::from_code(75).as_str(), "transient");
    }

    // ---- M56-08: message routing (ADR-0029) --------------------------------

    #[test]
    fn a_dismiss_message_carries_only_the_close_control() {
        // An error/notice is dismiss-only: Close is its one control, the default
        // focus (index 0), so "open, press Enter" closes it.
        let alert = Overlay::alert("e", vec!["x".into()], Severity::Error);
        assert_eq!(alert.controls(), &[OverlayControl::Close]);
        let notice = Overlay::notice("ok", vec!["done".into()]);
        assert_eq!(notice.controls(), &[OverlayControl::Close]);
    }

    #[test]
    fn the_wizard_defaults_focus_to_its_primary_action() {
        // The wizard opens with the *add project* button focused, Close second.
        let wizard = Overlay::wizard();
        assert_eq!(
            wizard.controls(),
            &[OverlayControl::Primary, OverlayControl::Close],
            "the wizard focuses its primary action first"
        );
        let Overlay::Alert(m) = &wizard else {
            panic!("the wizard is an Alert")
        };
        assert_eq!(m.action, MessageAction::AddProject);
        assert_eq!(m.action.primary_label(), Some("Add a project"));
    }

    #[test]
    fn severity_reports_only_for_message_overlays() {
        assert_eq!(
            Overlay::alert("e", vec![], Severity::Error).severity(),
            Some(Severity::Error)
        );
        assert_eq!(
            Overlay::notice("ok", vec![]).severity(),
            Some(Severity::Success)
        );
        assert_eq!(
            Overlay::Running { title: "x".into() }.severity(),
            None,
            "a running frame carries its own accent, not a severity"
        );
    }

    #[test]
    fn message_constructors_set_the_severity_axis() {
        assert_eq!(Message::error("t", vec![]).severity, Severity::Error);
        assert_eq!(Message::warning("t", vec![]).severity, Severity::Warning);
        assert_eq!(Message::success("t", vec![]).severity, Severity::Success);
        // A plain message is dismiss-only (no primary action leaks in).
        assert_eq!(Message::error("t", vec![]).action, MessageAction::Dismiss);
        assert_eq!(MessageAction::Dismiss.primary_label(), None);
    }
}
