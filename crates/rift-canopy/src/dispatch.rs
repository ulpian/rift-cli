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

/// The **one sanctioned process spawn** from the TUI (ADR-0025): open a project
/// in `$EDITOR`. Injected at launch exactly like [`CommandDispatcher`] — the
/// render crate never constructs a process; loop tests use a recording mock.
/// Structurally **outside** the palette grammar, so the closed-grammar
/// no-spawn proof (ADR-0015/0021) is preserved: no spawn is reachable from
/// parse or dispatch; exactly one spawn is reachable from this seam.
pub trait EditorLauncher {
    /// Open `path` in `$EDITOR`, synchronously (the binary suspends the
    /// terminal guard, argv-execs `$EDITOR <path>`, and re-enters the guard).
    /// Returns a short human outcome for the status line — `Ok` on success,
    /// `Err(reason)` when `$EDITOR` is unset/empty or the launch failed. Never
    /// a fallback guess at an editor.
    ///
    /// # Errors
    /// A human reason (unset `$EDITOR`, spawn failure) — never key/transcript
    /// bytes.
    fn open(&mut self, path: &std::path::Path) -> Result<(), String>;
}

/// A launcher for contexts with no terminal behind them (headless snapshots,
/// tests): every open is a no-op success. The real spawn lives in the binary.
pub struct NullEditorLauncher;

impl EditorLauncher for NullEditorLauncher {
    fn open(&mut self, _path: &std::path::Path) -> Result<(), String> {
        Ok(())
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
}
