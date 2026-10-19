//! Per-family hook **responses**: the single place that knows
//! what JSON a native hook API expects back, and whether a family offers a
//! context channel Rift may hand a session pointer through.
//!
//! Two rules govern this module and neither bends:
//!
//! 1. **A hook never blocks the agent.** Every response is either a documented
//!    neutral acknowledgement or nothing at all. Rift makes no policy decision
//!    on an agent's behalf, and an injection failure means *no injection* —
//!    never a stalled turn. The caller's `catch_unwind` + `exit(0)` guard is
//!    what enforces that at the process edge; this module never gives it a
//!    reason to fire.
//! 2. **No channel is invented.** A family receives an injected pointer only
//!    where its response contract is known. [`injection_points`] states this
//!    per family; the families whose hook-response schema is not established
//!    receive the same guidance through their managed instructions file
//!    (`rift_agent_install::native::install_recall_skill`) instead. Guessing at a hook schema would
//!    risk exactly the breakage rule 1 forbids, and a pointer that never
//!    arrives is a missed saving, while a malformed hook response is a broken
//!    agent.

use std::fmt;

use rift_core::AgentId;
use rift_recall_types::{ContextKind, ContextPlan};
use serde_json::{json, Value};

/// Which of the documented step kinds Rift writes when a family's channel is
/// [`Channel::InjectSteps`].
///
/// Antigravity documents three, and they are not interchangeable. A recorded
/// run settled which one the table names: both the transient and the user kind
/// reach the model at the invocation they are injected into, but a few
/// invocations later only the user kind is still in the model's reach — and
/// every fact this channel carries (a session pointer, what the context holds,
/// what to do after a compaction) is read later than that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InjectKind {
    /// A transient system message, dropped from the trajectory afterwards.
    EphemeralMessage,
    /// A message attributed to the user, kept in the trajectory.
    UserMessage,
    /// A tool call injected into the trajectory.
    ToolCall,
}

impl InjectKind {
    /// The token this kind carries in a capability row.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EphemeralMessage => "ephemeral_message",
            Self::UserMessage => "user_message",
            Self::ToolCall => "tool_call",
        }
    }

    /// The field an injected step carries its **text** in, or `None` for a kind
    /// that is not text at all.
    ///
    /// The tool-call kind is deliberately the `None`: injecting one would make
    /// Rift issue a call in the agent's name, which is a decision about the
    /// session rather than a fact handed to it, and no accounting of tokens
    /// buys that. A row naming it is a row this module answers nothing on.
    #[must_use]
    pub const fn message_field(self) -> Option<&'static str> {
        match self {
            Self::EphemeralMessage => Some("ephemeralMessage"),
            Self::UserMessage => Some("userMessage"),
            Self::ToolCall => None,
        }
    }
}

/// One of the closed rewrite forms Rift may ask a family to apply to a tool
/// call it was about to make.
///
/// Document/search forms add or redirect a field. The shell-read form replaces
/// only a strict, already-classified redundant read with the bounded result the
/// dispatcher selected. The enumeration is closed on purpose: it is the whole
/// answer to "what may Rift rewrite?".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RewriteForm {
    /// A read of a document is pointed at the Markdown sidecar Rift produced
    /// from it.
    DocumentSidecarPath,
    /// A content search that declared no result cap is given one.
    GrepHeadLimit,
    /// A strict Codex shell read is replaced by a command printing Rift's
    /// bounded prepared result.
    ShellReadReplacement,
    /// A terminal result's stretches the window provably holds at the
    /// revision on disk are replaced by a frame naming them; every other
    /// line is kept verbatim.
    HeldOutputSubstitution,
    /// A terminal result's runs of lines alike but for their digits are each
    /// folded to their first line and one line with a count; every line the
    /// model may act on is kept verbatim.
    FoldedOutput,
}

impl RewriteForm {
    /// The token this form carries in a capability row.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DocumentSidecarPath => "document_sidecar_path",
            Self::GrepHeadLimit => "grep_head_limit",
            Self::ShellReadReplacement => "shell_read_replacement",
            Self::HeldOutputSubstitution => "held_output_substitution",
            Self::FoldedOutput => "folded_output",
        }
    }
}

/// What a family lets Rift write back on one of its hooks.
///
/// This is the vocabulary of [`CAPABILITIES`], and it is deliberately small:
/// each variant is a channel some family's own published surface documents,
/// pinned into this tree as an artifact before it appeared here. A channel
/// this enum cannot express is a channel Rift does not answer on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// Text handed to the model beside the event, through the family's context
    /// field. `limit_field` names the key that bounds it where the family has
    /// one, which may be a key on the hook's own configuration rather than a
    /// field of the wire payload.
    AdditionalContext {
        /// The key that bounds the text, if the family declares one.
        limit_field: Option<&'static str>,
    },
    /// Steps injected into the trajectory before a model invocation.
    InjectSteps {
        /// Which documented step kind Rift writes.
        kind: InjectKind,
    },
    /// The call is declined and an explanation travels with the decision.
    ///
    /// `confirmed` is **not** "documented": every family below has this in
    /// writing. It records whether a run has been observed in which the
    /// explanation reached the *model* — the only thing that separates a
    /// reasoned decision from a silent tool failure, which a model is right to
    /// retry. Nothing declines a call on an unconfirmed row.
    DenyWithReason {
        /// The field the explanation travels in.
        reason_field: &'static str,
        /// Whether a recorded run has shown the model receiving it.
        confirmed: bool,
    },
    /// The tool's input is amended before it runs, in one of the closed forms.
    UpdatedInput {
        /// The closed set of amendments this family accepts from Rift.
        forms: &'static [RewriteForm],
    },
    /// The tool's result is replaced after it ran, in one of the closed forms.
    /// Only the result's own text changes; its shape is the one the tool gave.
    UpdatedToolOutput {
        /// The closed set of result replacements this family accepts.
        forms: &'static [RewriteForm],
    },
    /// The family requires an acknowledgement here, or documents none, and
    /// Rift writes nothing that carries a fact. Naming the verb keeps a later
    /// reader from mistaking silence for an oversight.
    Neutral,
}

impl fmt::Display for Channel {
    /// Renders the channel exactly as a fixture's capability row spells it, so
    /// the pinned artifact and the table can be compared as text.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AdditionalContext { limit_field } => write!(
                f,
                "channel=AdditionalContext limit_field={}",
                limit_field.unwrap_or("none")
            ),
            Self::InjectSteps { kind } => {
                write!(f, "channel=InjectSteps kind={}", kind.as_str())
            }
            Self::DenyWithReason {
                reason_field,
                confirmed,
            } => write!(
                f,
                "channel=DenyWithReason reason_field={reason_field} confirmed={confirmed}"
            ),
            Self::UpdatedInput { forms } => {
                write!(f, "channel=UpdatedInput forms=")?;
                for (i, form) in forms.iter().enumerate() {
                    if i > 0 {
                        write!(f, ",")?;
                    }
                    write!(f, "{}", form.as_str())?;
                }
                Ok(())
            }
            Self::UpdatedToolOutput { forms } => {
                write!(f, "channel=UpdatedToolOutput forms=")?;
                for (i, form) in forms.iter().enumerate() {
                    if i > 0 {
                        write!(f, ",")?;
                    }
                    write!(f, "{}", form.as_str())?;
                }
                Ok(())
            }
            Self::Neutral => write!(f, "channel=Neutral"),
        }
    }
}

/// **The per-family channel table, as data.**
///
/// One row per `(family, verb, channel)` Rift may answer on. A row exists only
/// where that family's own published output surface is pinned into this tree
/// as a committed artifact — repository or page, tag or digest, path, and a
/// verbatim excerpt — under `crates/rift-agent-*/fixtures/*-hook-output-surface.md`.
/// Each adapter's `pinned_output_surface` test asserts its family's rows here
/// and the rows in its fixture are the same set, in both directions, so
/// neither can grow without the other.
///
/// **A deny row is confirmed by a recorded run and by nothing else**, not by a
/// reference page — including the page that states plainly that the reason is
/// shown to the model. Each family's fixture carries the run its flag rests
/// on, or records why the attempt could not be made; a table is not a place to
/// promote a document into a measurement.
pub const CAPABILITIES: &[(AgentId, &str, Channel)] = &[
    (
        AgentId::CLAUDE_CODE,
        "PreToolUse",
        // Confirmed by the recorded run of 2026-09-03 in this family's fixture:
        // 11 declined reads of 11 delivered the reason to the model verbatim,
        // and 0 of 11 re-issued the call.
        Channel::DenyWithReason {
            reason_field: "permissionDecisionReason",
            confirmed: true,
        },
    ),
    (
        AgentId::CLAUDE_CODE,
        "PreToolUse",
        Channel::UpdatedInput {
            forms: &[RewriteForm::DocumentSidecarPath, RewriteForm::GrepHeadLimit],
        },
    ),
    (
        AgentId::CLAUDE_CODE,
        "PostToolUse",
        Channel::AdditionalContext { limit_field: None },
    ),
    (
        AgentId::CLAUDE_CODE,
        // The 2026-10-11 run in this family's fixture: a Bash result is
        // replaced only by the object of its own shape, and the replacement is
        // what the transcript keeps. The forms are a held stretch of lines
        // stood in for by its frame, or runs of alike lines folded to one
        // line each, every other byte kept; never both on one result.
        "PostToolUse",
        Channel::UpdatedToolOutput {
            forms: &[
                RewriteForm::HeldOutputSubstitution,
                RewriteForm::FoldedOutput,
            ],
        },
    ),
    (
        AgentId::CLAUDE_CODE,
        "UserPromptSubmit",
        Channel::AdditionalContext { limit_field: None },
    ),
    (
        AgentId::CLAUDE_CODE,
        "SessionStart",
        Channel::AdditionalContext { limit_field: None },
    ),
    (
        AgentId::CLAUDE_CODE,
        // The fourth boundary, and the only one a subagent has: neither of the
        // two above fires inside a Task-spawned window, so before this row a
        // subagent was handed nothing at all. The pinned SDK type carries the
        // same `additionalContext` field the other two travel on, and the
        // recorded runs of 2026-09-15 in this family's fixture showed the text
        // reaching the subagent and surviving its later tool calls.
        "SubagentStart",
        Channel::AdditionalContext { limit_field: None },
    ),
    (AgentId::CLAUDE_CODE, "PreCompact", Channel::Neutral),
    (
        AgentId::CODEX,
        "PreToolUse",
        // Confirmed by the recorded run of 2026-09-04 in this family's fixture:
        // 10 declined shell reads of 10 delivered the reason to the model, on
        // the client that has the surface at all. The client wraps those bytes
        // in a sentence of its own before the model reads them, which is a fact
        // about what a model sees and not about what this module writes — the
        // frame travels whole either way. The same run re-issued the call 10
        // times of 10, which is why this family is reached only by a repository
        // that names it: a confirmed channel is permission to be understood,
        // not permission to act.
        Channel::DenyWithReason {
            reason_field: "permissionDecisionReason",
            confirmed: true,
        },
    ),
    (
        AgentId::CODEX,
        "PreToolUse",
        // Installed Codex 0.153.1 was exercised with two sibling updatedInput
        // hooks in both orders: both saw the original input and the first
        // configured rewrite won. Rift is installed first for this event, so
        // strict read replacement has deterministic precedence.
        Channel::UpdatedInput {
            forms: &[RewriteForm::ShellReadReplacement],
        },
    ),
    (
        AgentId::CODEX,
        "UserPromptSubmit",
        Channel::AdditionalContext {
            limit_field: Some("additionalContextLimit"),
        },
    ),
    (
        AgentId::CODEX,
        "SessionStart",
        Channel::AdditionalContext {
            limit_field: Some("additionalContextLimit"),
        },
    ),
    (AgentId::CODEX, "PreCompact", Channel::Neutral),
    (AgentId::CODEX, "PostCompact", Channel::Neutral),
    (
        AgentId::ANTIGRAVITY,
        "PreToolUse",
        // Confirmed by the recorded runs in this family's fixture: the reason
        // reached the model synchronously in 3 runs of 3 on 2026-09-03, and
        // again in 13 of 13 on 2026-09-04 with the sealed frame this tree
        // really writes, on a later client. Both runs also re-issued the
        // declined call every single time, which is why nothing here declines
        // a call unless a repository names this family: a confirmed channel is
        // permission to be understood, not permission to act.
        Channel::DenyWithReason {
            reason_field: "reason",
            confirmed: true,
        },
    ),
    (
        AgentId::ANTIGRAVITY,
        "PreInvocation",
        // The kind the recorded run of 2026-09-03 found still readable a few
        // invocations after it was injected; the transient kind was not.
        Channel::InjectSteps {
            kind: InjectKind::UserMessage,
        },
    ),
    (AgentId::ANTIGRAVITY, "PostToolUse", Channel::Neutral),
    // The same step kinds as the row above, and deliberately not a second
    // injection point: one loop, injected into twice, spends twice for one
    // fact. Named so the empty object Rift already writes after an invocation
    // is a pinned shape rather than an unexamined habit.
    (AgentId::ANTIGRAVITY, "PostInvocation", Channel::Neutral),
    (AgentId::ANTIGRAVITY, "Stop", Channel::Neutral),
];

/// The fence a fixture's capability rows live in, so prose that necessarily
/// discusses rows while explaining them cannot smuggle one into the set.
const CAPABILITY_FENCE: &str = "```rift-capability-rows";

/// This family's rows, rendered in the one-line form a fixture spells them in.
///
/// The rendering is the comparison: an adapter proves its pin and this table
/// agree by comparing two sorted lists of strings, with no per-variant
/// destructuring to fall out of date.
#[must_use]
pub fn capability_rows(agent: &AgentId) -> Vec<String> {
    let mut rows: Vec<String> = CAPABILITIES
        .iter()
        .filter(|(a, _, _)| a == agent)
        .map(|(_, verb, channel)| format!("verb={verb} {channel}"))
        .collect();
    rows.sort();
    rows
}

/// The capability rows a pinned output-surface fixture states, read only from
/// inside its `rift-capability-rows` fence.
///
/// The parse is deliberately dumb — whole lines, taken as written — because a
/// clever parse is one that can be talked into accepting a row the artifact
/// does not actually contain.
#[must_use]
pub fn pinned_capability_rows(fixture: &str) -> Vec<String> {
    let mut rows = Vec::new();
    let mut inside = false;
    for line in fixture.lines() {
        if inside {
            if line.trim_end() == "```" {
                inside = false;
            } else {
                rows.push(line.trim().to_owned());
            }
        } else if line.trim_end() == CAPABILITY_FENCE {
            inside = true;
        }
    }
    rows.sort();
    rows
}

/// The verbs at which a family may be handed text, with the channel each one
/// travels on.
///
/// **Derived from [`CAPABILITIES`], never listed separately**, so a verb
/// becomes an injection point by the same act that pins it — a committed
/// output-surface fixture — and by no other. A family with no pinned context
/// channel answers with an empty list: its guidance ships in the managed
/// instructions file, not through the hook wire.
///
/// Two rows a family has are deliberately not injection points. A post-result
/// context field is a place to tag a result, not a boundary at which a turn is
/// told what it holds, and it is off by default wherever it exists; and a
/// declined call carries its own reason rather than a turn's facts.
#[must_use]
pub fn injection_points(agent: &AgentId) -> Vec<(&'static str, Channel)> {
    CAPABILITIES
        .iter()
        .filter(|(family, verb, channel)| {
            family == agent
                && !AFTER_THE_CALL.contains(verb)
                && matches!(
                    channel,
                    Channel::AdditionalContext { .. } | Channel::InjectSteps { .. }
                )
        })
        .map(|(_, verb, channel)| (*verb, *channel))
        .collect()
}

/// Verbs that fire *after* the call they are about. A context channel there is
/// a tag on a result, which is a separate, off-by-default experiment — never a
/// boundary at which a session is handed what it holds.
const AFTER_THE_CALL: &[&str] = &["PostToolUse"];

/// Whether this `(agent, verb)` pair is one this module can write an injection
/// on **today**.
///
/// Narrower than [`injection_points`] on purpose, and the gap is the honest
/// part: a row exists as soon as the family's surface is pinned, while a
/// writer exists only once the shape it would write has somewhere to go. What
/// is still outside it:
///
/// - A trajectory-step channel whose kind is not text ([`InjectKind::ToolCall`])
///   has no text to write, and this module never invents one.
///
/// A context channel that declares a **limit** used to be outside it too, and
/// no longer is: the limit is a number Rift itself declares when it installs
/// the family's hooks ([`declared_context_limit`]), so a caller can fit its
/// text to that number before offering it — which is the whole condition. A
/// caller that does not fit is not protected by silence here; it is protected
/// by the fit, and the client's own behaviour past the limit is a preview plus
/// a file path (a recorded run), never a broken response.
#[must_use]
pub fn accepts_injection(agent: &AgentId, verb: &str) -> bool {
    writable_channel(agent, verb).is_some()
}

/// The channel this `(agent, verb)` pair may be handed text on, in the exact
/// form [`injection_response`] will write it — the single reading both the
/// predicate and the writer are derived from, so neither can drift into
/// promising a shape the other does not produce.
fn writable_channel(agent: &AgentId, verb: &str) -> Option<Channel> {
    injection_points(agent)
        .into_iter()
        .find(|(at, channel)| {
            *at == verb
                && match channel {
                    Channel::AdditionalContext { .. } => true,
                    Channel::InjectSteps { kind } => kind.message_field().is_some(),
                    _ => false,
                }
        })
        .map(|(_, channel)| channel)
}

/// The ceiling, in tokens, Rift declares for its own boundary text on a family
/// whose context channel is bounded by a number the hook's *author* writes.
///
/// It is one number and not two because the two things that read it must agree:
/// the installer writes it into the family's hook configuration
/// (`rift_agent_install`, through [`crate::AgentAdapter::declared_context_limit`]),
/// and the hook fits its text to it before answering. A limit written in one
/// place and assumed in another is the one way this channel breaks quietly —
/// the client's own behaviour past its limit is to keep a head and a tail of
/// the text, spill the rest to a file and say so, which hands the model a
/// preview of a frame plus a path it will not open (a recorded run of
/// 2026-09-04 on codex-cli 0.153.1; the family's fixture carries it).
///
/// **Why 1,200.** It is the sum of the two boundary budgets Rift ships:
/// 400 tokens for a turn's delta manifest and 800 for a post-compaction
/// recovery map. Only one of the two ever travels on one hook, so the sum is
/// deliberate headroom: an operator who raises a budget in `serve.toml` should
/// not have to reinstall a hook file for the raise to take effect, and the
/// smaller of the two numbers is what the fit actually uses.
///
/// **Why not 0.** Zero is the value that disables the client's spilling
/// altogether. That would make truncation unreachable by construction and is
/// exactly the wrong direction: the limit is a second, independent bound on
/// text Rift already bounds itself, and giving up the host's own guard to
/// avoid ever meeting it is not a guarantee, it is the removal of one.
pub const DECLARED_CONTEXT_LIMIT_TOKENS: u32 = 1_200;

/// The token ceiling Rift declares for this `(agent, verb)`'s context channel,
/// or `None` where the family bounds that channel by nothing.
///
/// Read off [`CAPABILITIES`]: a limit exists exactly where the family's pinned
/// row names the key it is written under, so a family that grows a limit field
/// grows one here with its pin and never before it.
#[must_use]
pub fn declared_context_limit(agent: &AgentId, verb: &str) -> Option<u32> {
    match writable_channel(agent, verb)? {
        Channel::AdditionalContext { limit_field } => {
            limit_field.map(|_| DECLARED_CONTEXT_LIMIT_TOKENS)
        }
        _ => None,
    }
}

/// The injected text, in the shape the family's pinned row names.
///
/// Two shapes, because two families documented two channels: text handed to
/// the model beside the event, and a step written into the trajectory before
/// the model is invoked. Both carry the same facts and neither carries
/// anything else — no cap, no termination behaviour, no second step. The
/// step's kind comes off the row rather than out of this function, so the
/// recorded run that chose it is what decides which field a client reads.
fn injection_response(agent: &AgentId, verb: &str, text: &str) -> Option<Value> {
    match writable_channel(agent, verb)? {
        Channel::AdditionalContext { .. } => Some(json!({
            "hookSpecificOutput": {
                "hookEventName": verb,
                "additionalContext": text,
            }
        })),
        Channel::InjectSteps { kind } => Some(object(vec![(
            "injectSteps",
            Value::Array(vec![object(vec![(
                kind.message_field()?,
                Value::from(text),
            )])]),
        )])),
        _ => None,
    }
}

/// What the gate has been asked to do about a tool call the agent is about to
/// make.
///
/// This is deliberately not [`crate::window_journal::Decision`]. That type says
/// what the *journal* concluded and carries the shape of a decline, in line
/// numbers; this one carries the sentence a model will read, already rendered
/// by the one renderer that is allowed to write it. Keeping them apart is what
/// keeps this module out of the business of composing text: `gate` passes the
/// reason through unchanged and never so much as trims it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateDecision<'a> {
    /// Let the call run exactly as the agent wrote it. This is the answer for
    /// every call Rift has nothing to say about, which is nearly all of them.
    Allow,
    /// Decline this one call, and hand the model this explanation for it.
    ///
    /// A decline reaches a family only where a pinned row says the family
    /// documents an explanation field **and** a recorded run showed a model
    /// reading one. Everywhere else this is silently an [`Self::Allow`]: a
    /// call that fails with nothing the model can read is an unexplained
    /// failure, and retrying one of those is reasonable behaviour.
    Deny {
        /// The rendered explanation, passed through byte for byte.
        reason: &'a str,
    },
    /// Let the call run with one field of its input amended, in one of the two
    /// forms that only ever add what the caller left out or point a read at
    /// bytes Rift made. Never together with a decline: a call is either
    /// declined or run, and a declined call has no input to amend.
    Rewrite(Rewrite<'a>),
    /// Replace a terminal result that already ran with the same result in
    /// which the window's provably held stretches stand as their frames. The
    /// value is the whole result object, built by the caller from the one the
    /// client handed over, so its shape is the tool's own.
    ReplaceOutput(&'a Value),
}

/// One of the two rewrites, bound to the call it amends.
///
/// The whole input object travels with it because the field is a replacement
/// for the whole of what the agent wrote, not a patch on it: a rewrite that
/// carried only its own field would delete every other argument of the call.
/// Carrying the original here means the one changed field can be *proved* to
/// be the only difference, which is the property [`RewriteForm`] states and
/// this type is what makes it structural rather than a convention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rewrite<'a> {
    /// A read of a document, pointed at the Markdown sidecar Rift made of it.
    /// The call already names a path; this one replaces it.
    DocumentSidecar {
        /// The call as the agent wrote it.
        tool_input: &'a Value,
        /// The sidecar to read instead.
        file_path: &'a str,
    },
    /// A content search that declared no result cap, given one. The call must
    /// not already name a cap: a search that chose its own is a search Rift
    /// leaves alone.
    GrepHeadLimit {
        /// The call as the agent wrote it.
        tool_input: &'a Value,
        /// The cap to add.
        head_limit: u32,
    },
}

impl Rewrite<'_> {
    /// Which of the two closed forms this is.
    #[must_use]
    pub const fn form(&self) -> RewriteForm {
        match self {
            Self::DocumentSidecar { .. } => RewriteForm::DocumentSidecarPath,
            Self::GrepHeadLimit { .. } => RewriteForm::GrepHeadLimit,
        }
    }

    /// The whole input object with the one field changed, or `None` when this
    /// rewrite has no business being made.
    ///
    /// The field names are read from the family's own pinned tool-field table
    /// rather than retyped here, so the argument this rewrites is the argument
    /// that family's published surface names. No pin, no rewrite.
    fn updated_input(&self, agent: &AgentId) -> Option<Value> {
        let (tool, key, value) = match *self {
            Self::DocumentSidecar {
                tool_input,
                file_path,
            } => {
                if file_path.is_empty() {
                    return None;
                }
                (tool_input, ("Read", "path"), Value::from(file_path))
            }
            Self::GrepHeadLimit {
                tool_input,
                head_limit,
            } => {
                if head_limit == 0 {
                    return None;
                }
                (tool_input, ("Grep", "head_limit"), Value::from(head_limit))
            }
        };
        let field = crate::tool_fields::pinned_fact(agent, key.0, key.1)?;
        let written = tool.as_object()?;
        match self {
            // A sidecar stands in for a path the call already carries, and a
            // substitution that changes nothing is not one.
            Self::DocumentSidecar { .. } => {
                if written.get(field)? == &value {
                    return None;
                }
            }
            // A cap is only ever added. The pinned decision is that a search
            // naming its own cap -- including a cap of zero -- is untouched.
            Self::GrepHeadLimit { .. } => {
                if written.contains_key(field) {
                    return None;
                }
            }
        }
        let mut amended = written.clone();
        amended.insert(field.to_owned(), value);
        Some(Value::Object(amended))
    }
}

/// The amended-call response for a family that accepts this form, or `None`.
///
/// Three checks again, and the middle one is the closed set: the family's row
/// for this verb documents input substitution at all, that row lists this
/// form, and the amendment itself is one worth making. A family that documents
/// no substitution -- both of the other two say a tool call cannot be modified
/// -- reaches documents and search caps another way.
fn rewrite_response(agent: &AgentId, verb: &str, rewrite: &Rewrite<'_>) -> Option<Value> {
    let Some(Channel::UpdatedInput { forms }) = pinned_channel(agent, verb, |channel| {
        matches!(channel, Channel::UpdatedInput { .. })
    }) else {
        return None;
    };
    if !forms.contains(&rewrite.form()) {
        return None;
    }
    let amended = rewrite.updated_input(agent)?;
    Some(object(vec![(
        "hookSpecificOutput",
        object(vec![
            ("hookEventName", Value::from(verb)),
            ("permissionDecision", Value::from("allow")),
            ("updatedInput", amended),
        ]),
    )]))
}

/// The replaced-result response for a family whose row lists a result
/// rewrite form, or `None`. The caller spooled the original under its form.
///
/// Only an object passes: the pinned run showed a Bash result replaced by
/// anything but its own object shape is ignored by the client, and a
/// response the client ignores is one Rift should not have written.
fn output_response(agent: &AgentId, verb: &str, output: &Value) -> Option<Value> {
    let Some(Channel::UpdatedToolOutput { forms }) = pinned_channel(agent, verb, |channel| {
        matches!(channel, Channel::UpdatedToolOutput { .. })
    }) else {
        return None;
    };
    if forms.is_empty() || !output.is_object() {
        return None;
    }
    Some(object(vec![(
        "hookSpecificOutput",
        object(vec![
            ("hookEventName", Value::from(verb)),
            ("updatedToolOutput", output.clone()),
        ]),
    )]))
}

/// The channel of one kind this `(family, verb)` has a pinned row for.
///
/// The table is the authority for what may be written and the fixtures are the
/// authority for the table, so a channel absent here is a channel Rift answers
/// nothing on — not a gap to fill in with a reading of a reference page.
fn pinned_channel(
    agent: &AgentId,
    verb: &str,
    wanted: fn(&Channel) -> bool,
) -> Option<&'static Channel> {
    CAPABILITIES
        .iter()
        .find(|(a, v, channel)| a == agent && *v == verb && wanted(channel))
        .map(|(_, _, channel)| channel)
}

/// A JSON object from pairs, so a response is built from named fields rather
/// than from a literal whose keys a reader has to trust.
fn object(fields: Vec<(&str, Value)>) -> Value {
    Value::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

/// The declined-call response for a family that may be handed one, or `None`.
///
/// Three things have to hold and each of them is a whole rule: the family's
/// row for this verb documents an explanation field; a recorded run confirmed
/// a model reads it; and there is an explanation to give. A decline with an
/// empty reason is the unexplained failure the confirmation exists to prevent,
/// so it is refused here rather than emitted.
fn deny(agent: &AgentId, verb: &str, reason: &str) -> Option<Value> {
    if reason.trim().is_empty() {
        return None;
    }
    let Some(Channel::DenyWithReason {
        reason_field,
        confirmed,
    }) = pinned_channel(agent, verb, |channel| {
        matches!(channel, Channel::DenyWithReason { .. })
    })
    else {
        return None;
    };
    if !confirmed {
        return None;
    }
    // The two envelopes are the two the fixtures pin, and which family uses
    // which is a fact about that family, not something to infer from the field
    // name. A family with neither is answered by its neutral response.
    if *agent == AgentId::ANTIGRAVITY {
        return Some(object(vec![
            ("decision", Value::from("deny")),
            (reason_field, Value::from(reason)),
        ]));
    }
    if *agent == AgentId::CLAUDE_CODE || *agent == AgentId::CODEX {
        return Some(object(vec![(
            "hookSpecificOutput",
            object(vec![
                ("hookEventName", Value::from(verb)),
                ("permissionDecision", Value::from("deny")),
                (reason_field, Value::from(reason)),
            ]),
        )]));
    }
    None
}

/// **The one place a decision about a tool call becomes JSON.**
///
/// Every declined call in this tree is composed here and nowhere else, which
/// is what makes the per-family rules checkable by reading one function: the
/// row has to exist, the run has to have happened, and the shape is the one
/// the fixture pins. Anything this function cannot do — an unpinned family, an
/// unconfirmed channel, a verb with no row, an empty reason, a panic — comes
/// back as the family's neutral response, so the worst outcome of a mistake
/// here is that a call Rift meant to decline simply runs.
///
/// The reason text arrives rendered. This function never composes, trims,
/// wraps or prefixes it: one renderer writes what a model reads, and a second
/// place that could change those bytes is a second renderer.
#[must_use]
pub fn gate(agent: &AgentId, verb: &str, decision: &GateDecision<'_>) -> Option<Value> {
    // Belt and braces over the caller's own guard. A hook that panics writes
    // nothing to stdout, and a client that reads nothing where its contract
    // expects an object is a client Rift has broken -- so the panic is caught
    // here too, at the boundary where the neutral answer is still known.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match decision {
        GateDecision::Allow => neutral(agent, verb),
        GateDecision::Deny { reason } => deny(agent, verb, reason).or_else(|| neutral(agent, verb)),
        GateDecision::Rewrite(rewrite) => {
            rewrite_response(agent, verb, rewrite).or_else(|| neutral(agent, verb))
        }
        GateDecision::ReplaceOutput(output) => {
            output_response(agent, verb, output).or_else(|| neutral(agent, verb))
        }
    }))
    .unwrap_or_else(|_| neutral(agent, verb))
}

/// Build the JSON a native hook expects on stdout, or `None` when the family
/// documents no response for this verb (the common case — most hooks are
/// purely observational and silence is the contract).
///
/// `inject` is the pointer text to hand the agent, and is honoured **only**
/// where [`accepts_injection`] holds. Passing text for a family or verb that
/// has no channel is not an error and not a guess: it is simply dropped, so a
/// caller can compute a pointer optimistically without knowing the per-family
/// table.
#[must_use]
pub fn respond(agent: &AgentId, verb: &str, inject: Option<&str>) -> Option<Value> {
    respond_with_decision(agent, verb, inject, &GateDecision::Allow)
}

/// Encode a dispatcher-owned context plan into one family's native response.
///
/// The adapter is deliberately unable to choose or restructure context here:
/// it may only encode the one payload the transport-independent dispatcher
/// selected. A non-allow gate decision still wins because it answers the call
/// currently waiting on this hook.
#[must_use]
pub fn respond_with_plan(
    agent: &AgentId,
    verb: &str,
    plan: &ContextPlan,
    decision: &GateDecision<'_>,
) -> Option<Value> {
    encode_plan(agent, verb, plan, decision)
}

/// Whether an encoded native response carries the bounded read replacement
/// selected by a [`ContextPlan`].
///
/// Native field names remain private to this encoder module. Hook bookkeeping
/// asks this semantic question instead of learning a transport's JSON shape.
#[must_use]
pub fn delivers_read_replacement(response: &Value) -> bool {
    response
        .pointer("/hookSpecificOutput/updatedInput/command")
        .and_then(Value::as_str)
        .is_some()
}

/// The same response, for a call the gate has something to say about.
///
/// A decision travels with the response because they are one answer: a hook
/// writes one object to stdout, and a declined call and an injected pointer
/// cannot both be it. A decline wins and the pointer is dropped, which is the
/// right way round — the decline is about the call the model is making now,
/// the pointer is about the turn, and a model that reads two messages in one
/// tool result reads neither carefully.
///
/// [`GateDecision::Allow`] is byte for byte what [`respond`] has always
/// written: the gate answers a family's neutral response and the pointer
/// channel is the only thing that speaks.
#[must_use]
pub fn respond_with_decision(
    agent: &AgentId,
    verb: &str,
    inject: Option<&str>,
    decision: &GateDecision<'_>,
) -> Option<Value> {
    let plan = inject
        .filter(|text| !text.is_empty())
        .map_or(ContextPlan::None, |text| ContextPlan::Deliver {
            kind: ContextKind::SessionPointer,
            text: text.to_owned(),
            delivered_tokens: 0,
            avoided_tokens: 0,
            fallback: false,
        });
    encode_plan(agent, verb, &plan, decision)
}

fn encode_plan(
    agent: &AgentId,
    verb: &str,
    plan: &ContextPlan,
    decision: &GateDecision<'_>,
) -> Option<Value> {
    // A dispatcher-selected read replacement is stronger than the decline
    // that caused it: Codex can execute the bounded substitute in place of the
    // raw command. If this pinned encoder cannot produce a response, the
    // confirmed decline below remains the fail-open/retry fallback.
    if let Some(response) = read_replacement_response(agent, verb, plan) {
        return Some(response);
    }
    if !matches!(decision, GateDecision::Allow) {
        if let Some(response) = gate(agent, verb, decision) {
            return Some(response);
        }
    }
    if let Some(text) = plan.text() {
        if !text.is_empty() {
            if let Some(response) = injection_response(agent, verb, text) {
                return Some(response);
            }
        }
    }
    neutral(agent, verb)
}

/// Encode a bounded dispatcher-owned read replacement as the one native
/// `updatedInput` form whose sibling ordering has been exercised live.
fn read_replacement_response(agent: &AgentId, verb: &str, plan: &ContextPlan) -> Option<Value> {
    let ContextPlan::Deliver {
        kind: ContextKind::ReadReplacement,
        text,
        ..
    } = plan
    else {
        return None;
    };
    if text.is_empty() {
        return None;
    }
    let Some(Channel::UpdatedInput { forms }) = pinned_channel(agent, verb, |channel| {
        matches!(channel, Channel::UpdatedInput { .. })
    }) else {
        return None;
    };
    if !forms.contains(&RewriteForm::ShellReadReplacement) {
        return None;
    }
    // A single-quoted POSIX shell word is deterministic and needs no helper
    // process or temporary file. Embedded apostrophes close, quote one byte,
    // and reopen the word; all other bytes, including newlines, remain data.
    let quoted = text.replace('\'', "'\\''");
    let command = format!("printf '%s\\n' '{quoted}'");
    Some(object(vec![(
        "hookSpecificOutput",
        object(vec![
            ("hookEventName", Value::from(verb)),
            ("permissionDecision", Value::from("allow")),
            (
                "updatedInput",
                object(vec![("command", Value::from(command))]),
            ),
        ]),
    )]))
}

/// The documented neutral response for families whose hook API requires one.
///
/// Antigravity's hook contract expects a decision object; Rift always allows
/// and never stops on its own account — capture observes, it does not govern.
fn neutral(agent: &AgentId, verb: &str) -> Option<Value> {
    if *agent != AgentId::ANTIGRAVITY {
        return None;
    }
    Some(match verb {
        "PreToolUse" => json!({ "decision": "allow" }),
        "Stop" => json!({ "decision": "stop" }),
        _ => json!({}),
    })
}

#[cfg(test)]
mod tests;
