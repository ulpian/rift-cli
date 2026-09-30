//! The one place a remedy sentence is written: what is wrong, where to act,
//! and what to do there, for every reason on every surface.
//!
//! A separate module so every front end prints the same words. `rift status`,
//! `rift doctor` and Codex's trust row all take their sentence from here, and
//! the match over [`Reason`] has no catch-all arm, so a new reason without a
//! sentence does not compile.
//!
//! Every sentence names only what the user or Rift's own command line can do.
//! Trust and configuration in another client are that client's to change, so a
//! sentence may send the user into the client to act, and never has Rift act
//! there for them.

use serde::{Deserialize, Serialize};

use crate::detect::{DetectionEnvironment, NativeIdeHost};
use crate::native::{CodexHookVerdict, CodexSurface, CodexTrust};
use crate::ClientFamily;

use super::{assess, client_surface, Facts, Finding, Reason, Severity, Surface};

/// One finding as a front end prints it, and the element of the `--json`
/// `health` array.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remedy {
    /// Why, as the stable code.
    pub reason: Reason,
    /// How much it matters.
    pub severity: Severity,
    /// The agent's registry token.
    pub agent: String,
    /// The session the finding is about, when it is about one.
    pub session: Option<String>,
    /// Where the agent's client runs, which decides where the sentence says
    /// to act.
    pub surface: Surface,
    /// One short line: the agent's name and what is wrong.
    pub headline: String,
    /// What to do and where.
    pub sentence: String,
    /// The finding's identifiers, paths and counts.
    pub detail: String,
}

/// Why Codex skips the hooks it skips, which decides the sentence's clause.
#[derive(Clone, Copy)]
enum SkipCause {
    Modified,
    Untrusted,
    Both,
}

/// The remedy for `finding` on `surface`.
///
/// `codex_trust` is Codex's trust as the finding's facts held it: the
/// skipped-hooks sentence counts modified and untrusted handlers from it, so
/// the finding and the trust row say the same numbers. Without it, the hooks
/// the finding names are counted and called new or changed, which is true of
/// either cause.
#[must_use]
pub fn remedy(finding: &Finding, surface: Surface, codex_trust: Option<&CodexTrust>) -> Remedy {
    let name = agent_name(&finding.agent);
    let (headline, sentence) = match finding.reason {
        Reason::CodexHooksSkipped => codex_hooks_skipped(finding, surface, codex_trust),
        Reason::CodexTrustUnverifiable => (
            format!("{name}: hook trust unverifiable"),
            codex_trust_unverifiable(&finding.detail, surface),
        ),
        Reason::LifecycleHooksMissing => (
            format!("{name}: no session start or prompt arriving"),
            lifecycle_hooks_missing(&finding.agent, name, surface),
        ),
        Reason::HookBinaryMissing => (
            format!("{name}: hook binary missing"),
            hook_binary_missing(&finding.agent, name, surface),
        ),
        Reason::HookBinaryDisagrees => (
            format!("{name}: hooks run a different Rift build"),
            hook_binary_disagrees(&finding.agent, name, surface),
        ),
        Reason::SessionBehindEvents => (
            format!("{name}: session behind its events (Rift defect)"),
            session_behind_events(name),
        ),
    };
    Remedy {
        reason: finding.reason,
        severity: finding.severity,
        agent: finding.agent.clone(),
        session: finding.session.clone(),
        surface,
        headline,
        sentence,
        detail: finding.detail.clone(),
    }
}

/// Every finding over `facts` as of `now_ms`, each with its remedy on the
/// surface its agent runs on, as `env` and the facts' sessions name it.
///
/// The one assessment every front end prints — `rift status`, `rift doctor`
/// and the dashboard — so none can find something another misses or word it
/// differently. Reads only what `facts` already holds and what `env` detects.
#[must_use]
pub fn remedies(facts: &Facts<'_>, env: &DetectionEnvironment, now_ms: i64) -> Vec<Remedy> {
    assess(facts, now_ms)
        .iter()
        .map(|finding| {
            let (surface, _) = client_surface(&finding.agent, facts.sessions, env);
            remedy(finding, surface, facts.codex_trust.as_ref())
        })
        .collect()
}

/// The sentence for Codex skipping `modified + untrusted` of Rift's hooks,
/// `None` when it skips none. [`CodexTrust::remedy`] prints exactly this.
pub(crate) fn codex_skipped_sentence(
    modified: usize,
    untrusted: usize,
    surface: CodexSurface,
) -> Option<String> {
    let cause = match (modified, untrusted) {
        (0, 0) => return None,
        (_, 0) => SkipCause::Modified,
        (0, _) => SkipCause::Untrusted,
        _ => SkipCause::Both,
    };
    Some(codex_sentence(modified + untrusted, cause, surface))
}

/// The Codex sentence for `skipped` hooks skipped for `cause`.
fn codex_sentence(skipped: usize, cause: SkipCause, surface: CodexSurface) -> String {
    let (why, which) = match cause {
        SkipCause::Modified => (
            "they changed after you approved them",
            "the hooks marked 'Modified since last trusted'",
        ),
        SkipCause::Untrusted => (
            "you have not approved them yet",
            "Rift's hooks it lists as untrusted",
        ),
        SkipCause::Both => (
            "they are new or changed after you approved them",
            "Rift's hooks it lists as untrusted or 'Modified since last trusted'",
        ),
    };
    let mut sentence = format!(
        "Codex is skipping {skipped} of Rift's hooks because {why}, so Rift cannot see \
         your Codex sessions start. In a terminal in this repository run `codex`, type \
         `/hooks`, and trust {which}."
    );
    if surface == CodexSurface::VsCode {
        sentence.push_str(" Then reload the VS Code window and start a new Codex conversation.");
    }
    sentence
}

/// Headline and sentence for Codex skipping Rift's hooks, counted from the
/// trust beside the finding when there is one.
fn codex_hooks_skipped(
    finding: &Finding,
    surface: Surface,
    codex_trust: Option<&CodexTrust>,
) -> (String, String) {
    let codex_surface = codex_surface(surface);
    let sentence = match codex_trust {
        Some(trust) => codex_skipped_sentence(
            handlers_judged(trust, CodexHookVerdict::Modified),
            handlers_judged(trust, CodexHookVerdict::Untrusted),
            codex_surface,
        ),
        None => None,
    };
    let skipped = match codex_trust {
        Some(trust) => trust.needs_review().len(),
        None => finding.detail.split(", ").count(),
    };
    let sentence =
        sentence.unwrap_or_else(|| codex_sentence(skipped, SkipCause::Both, codex_surface));
    (
        format!("Codex: {skipped} of Rift's hooks skipped"),
        sentence,
    )
}

/// Rift cannot reproduce Codex's answer: a warning, so the sentence says so
/// first, then where the user can read the answer in Codex itself.
fn codex_trust_unverifiable(detail: &str, surface: Surface) -> String {
    let mut sentence = format!(
        "Capture is not being lost as far as Rift can see, but Rift cannot tell whether \
         Codex runs its hooks ({detail}). In a terminal in this repository run `codex`, type \
         `/hooks`, and check that Rift's hooks are trusted."
    );
    if let Surface::Ide(host) = surface {
        sentence.push_str(&format!(" Then reload the {} window.", host.display_name()));
    }
    sentence
}

/// Tool calls arriving without the session and prompt around them: a restart
/// of the client first, where it runs, and the re-wiring only if that does
/// not bring them back.
fn lifecycle_hooks_missing(agent: &str, name: &str, surface: Surface) -> String {
    let restart = match surface {
        Surface::Ide(host) => format!(
            "In {}, reload the window and start a new {name} conversation",
            host.display_name()
        ),
        Surface::Terminal => {
            format!("In the terminal where {name} runs, quit it and start it again")
        }
        Surface::JetBrains => format!("In your JetBrains IDE, restart {name}"),
        Surface::Unknown => format!("Start a new {name} session wherever you run it"),
    };
    format!(
        "{name}'s newest session sent Rift tool calls but no session start or prompt, so \
         Rift sees the calls without the session and prompts around them. {restart}; if the \
         next session opens the same way, run `rift enable --agent {agent}` in a terminal \
         in this repository."
    )
}

/// The wired binary is gone, so every hook fails before Rift sees it.
/// `rift enable` rewrites Rift's own entries in the agent's hook file to the
/// binary that runs it; `rift init` keeps wiring it finds, so it is not the
/// remedy.
fn hook_binary_missing(agent: &str, name: &str, surface: Surface) -> String {
    format!(
        "{name}'s hooks run a Rift binary that is no longer there, so each hook fails before \
         Rift sees it. In a terminal in this repository, {}, {}.{}",
        enable_step(agent),
        restart_step(name, surface),
        codex_retrust_step(agent)
    )
}

/// Both binaries run, so this is a warning: two builds write one ledger.
/// Either side can move: the agent's hooks with `rift enable`, or the record
/// with `rift hooks install --repoint`, each run with the build it should name.
fn hook_binary_disagrees(agent: &str, name: &str, surface: Surface) -> String {
    format!(
        "{name}'s hooks run a different Rift build from the one this repository's git hooks \
         record; capture is not being lost, but two builds write one ledger. To settle on the \
         recorded build, in a terminal in this repository {}, {}; to record the build the \
         hooks run instead, run that build as `rift hooks install --repoint`.{}",
        enable_step(agent),
        restart_step(name, surface),
        codex_retrust_step(agent)
    )
}

/// Rift's own defect: the words the doctor's session leg uses, and no act.
fn session_behind_events(name: &str) -> String {
    format!(
        "Rift recorded this {name} session ended or idle while its own events kept arriving \
         — Rift's own state error, not yours; nothing to do, the session's next hook repairs \
         it."
    )
}

/// The re-wiring step, run with the build the hooks should run.
fn enable_step(agent: &str) -> String {
    format!(
        "run `rift enable --agent {agent}` with the Rift build the hooks should run (it \
         rewrites only Rift's own entries in the hook file)"
    )
}

/// What the user does in the client after its hook file changed, so the
/// client reads the new file.
fn restart_step(name: &str, surface: Surface) -> String {
    match surface {
        Surface::Ide(host) => format!(
            "then reload the {} window and start a new {name} conversation",
            host.display_name()
        ),
        Surface::Terminal => format!("then quit {name} in its terminal and start it again"),
        Surface::JetBrains => format!("then restart {name} in your JetBrains IDE"),
        Surface::Unknown => format!("then start a new {name} session wherever you run it"),
    }
}

/// Codex skips a handler that changed after it was trusted, so re-wiring
/// Codex's hooks is followed by trusting them again, in Codex.
fn codex_retrust_step(agent: &str) -> &'static str {
    if agent == ClientFamily::Codex.token() {
        return " Codex then lists the rewritten hooks as changed: run `codex`, type \
                `/hooks`, and trust them.";
    }
    ""
}

/// The Codex client a surface names: the extension in VS Code, the command
/// line anywhere else, as the trust row reads it.
fn codex_surface(surface: Surface) -> CodexSurface {
    match surface {
        Surface::Ide(NativeIdeHost::VsCode | NativeIdeHost::VsCodeInsiders) => CodexSurface::VsCode,
        Surface::Ide(NativeIdeHost::Cursor | NativeIdeHost::Windsurf)
        | Surface::Terminal
        | Surface::JetBrains
        | Surface::Unknown => CodexSurface::Terminal,
    }
}

/// How many of Rift's handlers Codex judged `verdict`.
fn handlers_judged(trust: &CodexTrust, verdict: CodexHookVerdict) -> usize {
    trust
        .hooks
        .iter()
        .filter(|hook| hook.verdict == verdict)
        .count()
}

/// The agent's display name for the four client families, and its registry
/// token for any other: the token is what the user typed to enable it.
fn agent_name(agent: &str) -> &str {
    ClientFamily::from_token(agent).map_or(agent, |family| family.display_name())
}
