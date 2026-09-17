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
}

impl RewriteForm {
    /// The token this form carries in a capability row.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DocumentSidecarPath => "document_sidecar_path",
            Self::GrepHeadLimit => "grep_head_limit",
            Self::ShellReadReplacement => "shell_read_replacement",
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
/// This is deliberately not [`crate::read_ledger::Decision`]. That type says
/// what the *ledger* concluded and carries the shape of a decline, in line
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
mod tests {
    use super::*;

    /// The field names a family's pinned rows entitle Rift to write on this
    /// verb. Derived from the rows rather than listed separately, so a channel
    /// cannot be given a field the table does not carry.
    fn permitted_fields(agent: &AgentId, verb: &str) -> Vec<&'static str> {
        let mut fields = Vec::new();
        for (a, v, channel) in CAPABILITIES {
            if a != agent || *v != verb {
                continue;
            }
            match channel {
                Channel::AdditionalContext { .. } => {
                    fields.extend(["hookSpecificOutput", "hookEventName", "additionalContext"]);
                }
                Channel::DenyWithReason { reason_field, .. } => {
                    fields.extend([
                        "hookSpecificOutput",
                        "hookEventName",
                        "decision",
                        "permissionDecision",
                        reason_field,
                    ]);
                }
                Channel::UpdatedInput { .. } => {
                    fields.extend([
                        "hookSpecificOutput",
                        "hookEventName",
                        "permissionDecision",
                        "updatedInput",
                    ]);
                }
                Channel::InjectSteps { .. } => fields.push("injectSteps"),
                Channel::Neutral => fields.push("decision"),
            }
        }
        fields
    }

    /// **The rule this module exists to keep: no field on an unpinned
    /// channel.** Every key `respond` can write, at the top level or inside
    /// `hookSpecificOutput`, is one the family's capability rows entitle it to
    /// write on that exact verb — and the rows exist only where a committed
    /// pin says so.
    ///
    /// Swept over every first-party family, so a family with no rows at all
    /// can answer with an empty acknowledgement and nothing else, and a verb
    /// no pin covers can carry no fact.
    #[test]
    fn every_response_shape_has_a_pinned_output_surface() {
        let verbs = [
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "PreInvocation",
            "PostInvocation",
            "PreCompact",
            "PostCompact",
            "SubagentStart",
            "SubagentStop",
            "Stop",
            "SessionEnd",
            "BeforeModel",
            "",
        ];
        for agent in AgentId::FIRST_PARTY {
            for verb in verbs {
                let permitted = permitted_fields(agent, verb);
                let input = a_call();
                for decision in decisions(&input) {
                    for inject in [None, Some("pointer")] {
                        let both = [
                            respond_with_decision(agent, verb, inject, &decision),
                            gate(agent, verb, &decision),
                        ];
                        for out in both.into_iter().flatten() {
                            let object = out.as_object().expect("a hook response is a JSON object");
                            let nested = object
                                .get("hookSpecificOutput")
                                .and_then(Value::as_object)
                                .into_iter()
                                .flat_map(|inner| inner.keys());
                            for key in object.keys().chain(nested) {
                                assert!(
                                    permitted.contains(&key.as_str()),
                                    "{} writes `{key}` on {verb}, which no pinned capability \
                                     row entitles it to; pin the channel or do not write the \
                                     field",
                                    agent.as_str()
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// Every decision the gate can be handed, so a sweep covers the answers as
    /// well as the questions.
    fn decisions(input: &Value) -> Vec<GateDecision<'_>> {
        vec![
            GateDecision::Allow,
            GateDecision::Deny {
                reason: "[rift] a rendered reason",
            },
            GateDecision::Rewrite(Rewrite::DocumentSidecar {
                tool_input: input,
                file_path: "/w/.rift/state/doc/abc.md",
            }),
            GateDecision::Rewrite(Rewrite::GrepHeadLimit {
                tool_input: input,
                head_limit: 40,
            }),
        ]
    }

    /// A call as an agent wrote it, carrying both fields the two rewrites
    /// touch plus one they must leave alone.
    fn a_call() -> Value {
        json!({"file_path": "/w/notes.docx", "pattern": "needle", "output_mode": "content"})
    }

    /// The families whose fixture carries a run showing a model reading a
    /// declined call's reason, and the verb it was seen on.
    const CONFIRMED_DENY: &[(AgentId, &str)] = &[
        (AgentId::CLAUDE_CODE, "PreToolUse"),
        (AgentId::CODEX, "PreToolUse"),
        (AgentId::ANTIGRAVITY, "PreToolUse"),
    ];

    /// **A call is declined only where a model will read why.** Swept over
    /// every family and every verb: the reason appears in a response exactly
    /// on the pairs whose pinned row is a confirmed explanation channel, and
    /// on every other pair the answer is indistinguishable from the one an
    /// allowed call gets — the same bytes, carrying none of the reason.
    ///
    /// This is the rule the whole design rests on. A family that declines a
    /// call whose reason its model never sees has not made a decision; it has
    /// produced a tool failure, which a model is right to retry, and the
    /// retries are the pool this program is trying to shrink.
    #[test]
    fn a_deny_is_emitted_only_on_a_family_with_a_confirmed_reason_channel() {
        let reason = "[rift] a rendered reason";
        let verbs = [
            "PreToolUse",
            "PostToolUse",
            "UserPromptSubmit",
            "SessionStart",
            "PreInvocation",
            "PreCompact",
            "Stop",
            "",
        ];
        for agent in AgentId::FIRST_PARTY {
            for verb in verbs {
                let out = gate(agent, verb, &GateDecision::Deny { reason });
                let carried = out
                    .as_ref()
                    .is_some_and(|value| value.to_string().contains(reason));
                let confirmed = CONFIRMED_DENY.iter().any(|(a, v)| a == agent && *v == verb);
                assert_eq!(
                    carried,
                    confirmed,
                    "{} {verb}: a decline is emitted exactly where a recorded run \
                     confirms the model reads the reason",
                    agent.as_str()
                );
                if !confirmed {
                    assert_eq!(
                        out,
                        gate(agent, verb, &GateDecision::Allow),
                        "{} {verb} answers a refused decline differently from an \
                         allowed call, which tells a client something happened",
                        agent.as_str()
                    );
                }
            }
        }
    }

    /// The reason travels as it was rendered. One renderer writes what a model
    /// reads; a second place that could trim, wrap or prefix those bytes is a
    /// second renderer, and the red-team corpus only covers the first.
    #[test]
    fn the_reason_is_passed_through_byte_for_byte() {
        let reason = "[rift] Read skipped: a file you read L1-40 are unchanged.  \n\ttrailing";
        for (agent, verb) in CONFIRMED_DENY {
            let out = gate(agent, verb, &GateDecision::Deny { reason })
                .expect("a confirmed family answers a decline");
            let field = match agent {
                a if *a == AgentId::ANTIGRAVITY => out["reason"].clone(),
                _ => out["hookSpecificOutput"]["permissionDecisionReason"].clone(),
            };
            assert_eq!(field, Value::from(reason), "{}", agent.as_str());
        }
    }

    /// Anything the gate cannot do is the family's neutral response — the
    /// answer an allowed call gets. An unknown family, a verb with no row, a
    /// decline with nothing to say: each is a mistake somewhere upstream, and
    /// the shape of the mistake is that the call simply runs.
    #[test]
    fn an_error_in_the_gate_is_the_neutral_response() {
        let cases = [
            // A family with no pinned surface at all.
            (AgentId::new("some-third-party-agent"), "PreToolUse"),
            // A verb this family has no explanation channel on.
            (AgentId::CLAUDE_CODE, "PostToolUse"),
            (AgentId::CODEX, "UserPromptSubmit"),
            (AgentId::ANTIGRAVITY, "Stop"),
            // A verb nobody declares.
            (AgentId::CLAUDE_CODE, ""),
        ];
        for (agent, verb) in &cases {
            assert_eq!(
                gate(agent, verb, &GateDecision::Deny { reason: "why" }),
                neutral(agent, verb),
                "{} {verb} answered a decline it cannot make",
                agent.as_str()
            );
        }
        // An empty explanation is the unexplained failure the confirmation
        // requirement exists to prevent, even on a confirmed family.
        for (agent, verb) in CONFIRMED_DENY {
            for reason in ["", "   ", "\n\t "] {
                assert_eq!(
                    gate(agent, verb, &GateDecision::Deny { reason }),
                    neutral(agent, verb),
                    "{} {verb} declined a call with nothing to say for it",
                    agent.as_str()
                );
            }
        }
    }

    /// **An allowed call is answered exactly as it was before the gate
    /// existed.** The gate is new behaviour for a decision Rift did not make
    /// until now; it is not a new answer to the calls it was already
    /// answering, and this test is the whole set of those answers written out
    /// as bytes rather than derived from the code that produces them.
    #[test]
    fn an_allowed_call_leaves_every_response_byte_as_it_was() {
        let cc = AgentId::CLAUDE_CODE;
        let agy = AgentId::ANTIGRAVITY;
        let expected: &[(&AgentId, &str, Option<&str>, Option<Value>)] = &[
            (
                &cc,
                "UserPromptSubmit",
                Some("hello"),
                Some(json!({"hookSpecificOutput": {
                    "hookEventName": "UserPromptSubmit",
                    "additionalContext": "hello",
                }})),
            ),
            (&cc, "UserPromptSubmit", None, None),
            (&cc, "PreToolUse", Some("hello"), None),
            (
                &cc,
                "SessionStart",
                Some("hello"),
                Some(json!({"hookSpecificOutput": {
                    "hookEventName": "SessionStart",
                    "additionalContext": "hello",
                }})),
            ),
            (&agy, "PreToolUse", None, Some(json!({"decision": "allow"}))),
            (&agy, "Stop", None, Some(json!({"decision": "stop"}))),
            (&agy, "PostToolUse", None, Some(json!({}))),
            (&agy, "PostInvocation", None, Some(json!({}))),
            // The one row that moved when this family's boundary was wired:
            // offered text, it now writes the step its pinned row names, and
            // offered none it is the acknowledgement it always was.
            (
                &agy,
                "PreInvocation",
                Some("hello"),
                Some(json!({"injectSteps": [{"userMessage": "hello"}]})),
            ),
            (&agy, "PreInvocation", None, Some(json!({}))),
            // And the row that moved when the bounded channel was wired: the
            // shape is the one every context field carries, and what is new is
            // that a caller now fits its text to the bound before offering it.
            (
                &AgentId::CODEX,
                "UserPromptSubmit",
                Some("hello"),
                Some(json!({"hookSpecificOutput": {
                    "hookEventName": "UserPromptSubmit",
                    "additionalContext": "hello",
                }})),
            ),
            (&AgentId::CODEX, "UserPromptSubmit", None, None),
        ];
        for (agent, verb, inject, want) in expected {
            assert_eq!(
                respond(agent, verb, *inject),
                *want,
                "{} {verb} moved",
                agent.as_str()
            );
            assert_eq!(
                respond_with_decision(agent, verb, *inject, &GateDecision::Allow),
                *want,
                "{} {verb} moved under an explicit allow",
                agent.as_str()
            );
        }
    }

    /// **An amended call reaches one family, in two forms, changing one
    /// field.** Swept over every family and verb: the amendment appears only
    /// where a pinned row lists the form, the object it carries is the call
    /// the agent wrote with exactly one key different, and every other family
    /// answers as if nothing had been asked.
    ///
    /// The closed set is the point. Both other families' own pages say a tool
    /// call cannot be modified, and a third form here would stop being an
    /// addition to what the agent asked for and start being a decision about
    /// it — which is what the declined-call channel is for, with a reason
    /// attached and a run behind it.
    #[test]
    fn a_rewrite_is_emitted_only_on_claude_code_and_only_in_the_two_forms() {
        let verbs = [
            "PreToolUse",
            "PostToolUse",
            "UserPromptSubmit",
            "SessionStart",
            "PreInvocation",
            "Stop",
            "",
        ];
        let call = json!({"file_path": "/w/notes.docx", "pattern": "needle"});
        let forms = [
            Rewrite::DocumentSidecar {
                tool_input: &call,
                file_path: "/w/.rift/state/doc/abc.md",
            },
            Rewrite::GrepHeadLimit {
                tool_input: &call,
                head_limit: 40,
            },
        ];
        for agent in AgentId::FIRST_PARTY {
            for verb in verbs {
                for form in forms {
                    let out = gate(agent, verb, &GateDecision::Rewrite(form));
                    let here = *agent == AgentId::CLAUDE_CODE && verb == "PreToolUse";
                    let amended = out
                        .as_ref()
                        .and_then(|value| value["hookSpecificOutput"].get("updatedInput"))
                        .cloned();
                    assert_eq!(
                        amended.is_some(),
                        here,
                        "{} {verb} amends a call on a channel no pinned row gives it",
                        agent.as_str()
                    );
                    let Some(amended) = amended else {
                        assert_eq!(
                            out,
                            gate(agent, verb, &GateDecision::Allow),
                            "{} {verb} answered a refused amendment visibly",
                            agent.as_str()
                        );
                        continue;
                    };
                    // The whole call travels, with one field different.
                    let before = call.as_object().expect("a call is an object");
                    let after = amended.as_object().expect("an amended call is an object");
                    for key in before.keys() {
                        assert!(
                            after.contains_key(key),
                            "the amended call dropped `{key}`, which the agent wrote"
                        );
                    }
                    assert!(
                        after.len() <= before.len() + 1,
                        "the amended call grew by more than the one field"
                    );
                    let changed: Vec<&String> = after
                        .iter()
                        .filter(|(key, value)| before.get(*key) != Some(*value))
                        .map(|(key, _)| key)
                        .collect();
                    assert_eq!(changed.len(), 1, "more than one field changed: {changed:?}");
                    assert!(
                        matches!(changed[0].as_str(), "file_path" | "head_limit"),
                        "the changed field is {}, which is not one of the two forms",
                        changed[0]
                    );
                }
            }
        }
    }

    /// A cap is added and never overridden, a sidecar replaces a path the call
    /// actually carries, and neither is written into something that is not a
    /// call. Each of these is an amendment that would change what the agent
    /// asked for rather than add to it, so each answers as an allowed call.
    #[test]
    fn an_amendment_that_would_change_the_call_is_refused() {
        let capped = json!({"pattern": "needle", "head_limit": 5});
        let zero = json!({"pattern": "needle", "head_limit": 0});
        let read = json!({"file_path": "/w/notes.docx"});
        let not_a_call = json!(["file_path", "/w/notes.docx"]);
        let cases = [
            // A search that named its own cap keeps it, zero included.
            Rewrite::GrepHeadLimit {
                tool_input: &capped,
                head_limit: 40,
            },
            Rewrite::GrepHeadLimit {
                tool_input: &zero,
                head_limit: 40,
            },
            // A cap of nothing is not a cap.
            Rewrite::GrepHeadLimit {
                tool_input: &read,
                head_limit: 0,
            },
            // A substitution that substitutes nothing.
            Rewrite::DocumentSidecar {
                tool_input: &read,
                file_path: "/w/notes.docx",
            },
            // A sidecar with no path to point at.
            Rewrite::DocumentSidecar {
                tool_input: &read,
                file_path: "",
            },
            // A read with no path field to replace.
            Rewrite::DocumentSidecar {
                tool_input: &capped,
                file_path: "/w/.rift/state/doc/abc.md",
            },
            // Something that is not a call at all.
            Rewrite::DocumentSidecar {
                tool_input: &not_a_call,
                file_path: "/w/.rift/state/doc/abc.md",
            },
        ];
        for case in cases {
            assert_eq!(
                gate(
                    &AgentId::CLAUDE_CODE,
                    "PreToolUse",
                    &GateDecision::Rewrite(case)
                ),
                neutral(&AgentId::CLAUDE_CODE, "PreToolUse"),
                "{case:?} was emitted"
            );
        }
    }

    /// A declined call carries no amendment and an amended call carries no
    /// explanation. They are separate answers to separate questions, and a
    /// response that held both would be telling a client to run a call it was
    /// simultaneously told not to run.
    #[test]
    fn a_decline_and_an_amendment_never_travel_together() {
        let call = json!({"file_path": "/w/notes.docx"});
        let declined = gate(
            &AgentId::CLAUDE_CODE,
            "PreToolUse",
            &GateDecision::Deny {
                reason: "[rift] a rendered reason",
            },
        )
        .expect("a confirmed family answers a decline");
        assert!(!declined.to_string().contains("updatedInput"));
        let amended = gate(
            &AgentId::CLAUDE_CODE,
            "PreToolUse",
            &GateDecision::Rewrite(Rewrite::DocumentSidecar {
                tool_input: &call,
                file_path: "/w/.rift/state/doc/abc.md",
            }),
        )
        .expect("this family accepts both forms");
        assert!(!amended.to_string().contains("permissionDecisionReason"));
        assert_eq!(
            amended["hookSpecificOutput"]["permissionDecision"],
            Value::from("allow")
        );
    }

    /// ★ **A declined call on the family that publishes two decline routes is
    /// written on the supported one, and only on it.**
    ///
    /// The pinned schema declares both: a nested `permissionDecision` pair, and
    /// a top-level `decision` of `"block"` with a `reason` beside it that the
    /// client's own test names deprecated. A recorded run found both still
    /// blocking, which is exactly the situation in which a writer drifts onto
    /// the older one and nobody notices — so the object is written out here
    /// key by key, and the deprecated keys are asserted absent rather than
    /// merely not mentioned.
    #[test]
    fn a_decline_is_written_on_the_supported_route_and_never_the_deprecated_one() {
        let reason = "[rift] Read skipped: notes.md@sha256:abcdef01 lines 1-40 are unchanged.";
        let out = gate(
            &AgentId::CODEX,
            "PreToolUse",
            &GateDecision::Deny { reason },
        )
        .expect("the run recorded in this family's fixture confirmed the channel");
        assert_eq!(
            out,
            json!({"hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }}),
            "the bytes this family's client parses, and no others"
        );
        let object = out.as_object().expect("one object");
        assert_eq!(object.len(), 1, "exactly one top-level key");
        for deprecated in ["decision", "reason", "continue", "stopReason"] {
            assert!(
                !object.contains_key(deprecated),
                "the deprecated route's {deprecated} was written"
            );
        }
        // The client refuses a decline with nothing to say, in as many words
        // (the fixture quotes the message). Composing one would spend a round
        // trip to be told so, which is why nothing composes one.
        for empty in ["", "   ", "\n"] {
            assert_eq!(
                gate(
                    &AgentId::CODEX,
                    "PreToolUse",
                    &GateDecision::Deny { reason: empty }
                ),
                None,
                "a decline with nothing to say is not written"
            );
        }
        // The separately planned bounded replacement uses Codex's confirmed
        // updatedInput route. A raw decline remains exactly the object above:
        // the two response forms are selected before encoding and never travel
        // together.
        assert!(capability_rows(&AgentId::CODEX).iter().any(|row| {
            row == "verb=PreToolUse channel=UpdatedInput forms=shell_read_replacement"
        }));
        // The bound Rift declares for its own boundary text belongs to the two
        // boundary verbs and not to this one. A decline is a different message
        // at a different moment: it is never fitted to that number, never
        // counted against it, and never travels beside the text that is.
        assert_eq!(declared_context_limit(&AgentId::CODEX, "PreToolUse"), None);
        assert!(declared_context_limit(&AgentId::CODEX, "UserPromptSubmit").is_some());
        assert!(!out.to_string().contains("additionalContext"));
        assert_eq!(
            respond_with_decision(
                &AgentId::CODEX,
                "PreToolUse",
                Some("a session pointer"),
                &GateDecision::Deny { reason },
            ),
            Some(out),
            "a decline is the whole answer; the pointer is dropped"
        );
    }

    /// The table is a table: every row names one of the three families whose
    /// output surface is pinned, on a non-empty verb, and no `(family, verb)`
    /// carries the same kind of channel twice.
    #[test]
    fn the_capability_table_is_well_formed() {
        let pinned_families = [AgentId::CLAUDE_CODE, AgentId::CODEX, AgentId::ANTIGRAVITY];
        let mut seen: Vec<String> = Vec::new();
        for (agent, verb, channel) in CAPABILITIES {
            assert!(
                pinned_families.contains(agent),
                "{} has a capability row but no pinned output surface",
                agent.as_str()
            );
            assert!(!verb.is_empty(), "a capability row names no verb");
            let discriminant = channel
                .to_string()
                .split(' ')
                .next()
                .expect("a rendered channel starts with its name")
                .to_owned();
            let key = format!("{} {verb} {discriminant}", agent.as_str());
            assert!(!seen.contains(&key), "duplicate capability row: {key}");
            seen.push(key);
        }
        for agent in pinned_families {
            assert!(
                !capability_rows(&agent).is_empty(),
                "{} is an alpha family with no rows at all",
                agent.as_str()
            );
        }
    }

    /// A reason channel counts as confirmed on exactly the families whose
    /// fixture carries a recorded run showing a model reading the reason —
    /// and the fixture, not this list, is the authority: each adapter's
    /// `the_capability_table_equals_the_pinned_fixtures` compares the two as
    /// text in both directions, so a flag moved here without moving the
    /// evidence reddens there.
    ///
    /// Spelling the three out is what stops a fourth from arriving quietly:
    /// this is the flag that decides whether Rift may decline a call at all.
    #[test]
    fn a_reason_channel_is_confirmed_only_where_a_run_was_recorded() {
        let expected = [
            // 11 declined reads of 11 read by the model, 0 re-issued.
            (AgentId::CLAUDE_CODE, true),
            // 10 declined reads of 10 read by the model, on the client that
            // has the surface; 10 re-issued, which is a mode's business.
            (AgentId::CODEX, true),
            // The reason reached the model 3 times of 3, synchronously.
            (AgentId::ANTIGRAVITY, true),
        ];
        for (agent, verb, channel) in CAPABILITIES {
            let Channel::DenyWithReason { confirmed, .. } = channel else {
                continue;
            };
            let Some((_, want)) = expected.iter().find(|(a, _)| a == agent) else {
                panic!("{} has a deny row this test does not list", agent.as_str())
            };
            assert_eq!(
                *confirmed,
                *want,
                "{} {verb} disagrees with the recorded runs; confirmation is an \
                 observation in the family's fixture, not a table edit",
                agent.as_str()
            );
        }
    }

    /// Rewrites stay closed at two additive forms for Claude Code and one
    /// bounded shell-read replacement for Codex.
    #[test]
    fn input_rewrites_are_the_three_pinned_forms_on_two_families() {
        let mut rows = Vec::new();
        for (agent, _, channel) in CAPABILITIES {
            if let Channel::UpdatedInput { forms } = channel {
                rows.push((agent.clone(), *forms));
            }
        }
        assert_eq!(
            rows,
            [
                (
                    AgentId::CLAUDE_CODE,
                    &[RewriteForm::DocumentSidecarPath, RewriteForm::GrepHeadLimit,][..],
                ),
                (AgentId::CODEX, &[RewriteForm::ShellReadReplacement][..],),
            ]
        );
    }

    /// A fixture's rows are read from inside its fence and nowhere else, so
    /// prose that quotes a row while explaining it never becomes one.
    #[test]
    fn capability_rows_are_read_only_from_inside_the_fence() {
        let fixture = "\
verb=PreToolUse channel=Neutral
```rift-capability-rows
verb=SessionStart channel=AdditionalContext limit_field=none
```
verb=Stop channel=Neutral
";
        assert_eq!(
            pinned_capability_rows(fixture),
            ["verb=SessionStart channel=AdditionalContext limit_field=none"]
        );
    }

    /// The families Rift configures, and the response contract each one has.
    /// This is the conformance table the injection design asks for: adding a family
    /// without deciding its response shape fails here.
    #[test]
    fn per_family_response_shapes_are_pinned() {
        // Claude Code: the one verified context channel.
        let claude = AgentId::CLAUDE_CODE;
        assert_eq!(
            injection_points(&claude)
                .iter()
                .map(|(verb, _)| *verb)
                .collect::<Vec<_>>(),
            ["UserPromptSubmit", "SessionStart", "SubagentStart"],
            "every boundary this family pins, and the post-result tag is not one"
        );
        let out = respond(&claude, "UserPromptSubmit", Some("hello")).expect("a response");
        assert_eq!(
            out["hookSpecificOutput"]["hookEventName"],
            "UserPromptSubmit"
        );
        assert_eq!(out["hookSpecificOutput"]["additionalContext"], "hello");
        // …and nothing at all on any other verb, injected text or not.
        assert_eq!(respond(&claude, "PostToolUse", Some("hello")), None);
        assert_eq!(respond(&claude, "UserPromptSubmit", None), None);

        // Antigravity: a decision object is required; Rift always allows.
        let agy = AgentId::ANTIGRAVITY;
        assert!(
            accepts_injection(&agy, "PreInvocation"),
            "the pinned step channel is written on"
        );
        assert_eq!(
            respond(&agy, "PreToolUse", None),
            Some(json!({ "decision": "allow" }))
        );
        assert_eq!(
            respond(&agy, "Stop", None),
            Some(json!({ "decision": "stop" }))
        );
        assert_eq!(respond(&agy, "PostToolUse", None), Some(json!({})));

        // Codex has both boundaries too, and its bounded channel is asserted
        // where the bound is (a_context_limit_is_declared_…).
        //
        // Gemini: an observational family with no pinned context channel —
        // its guidance ships in the managed instructions file instead.
        let gemini = AgentId::GEMINI;
        assert!(!accepts_injection(&gemini, "UserPromptSubmit"));
        for verb in ["SessionStart", "UserPromptSubmit", "BeforeModel", "Stop"] {
            assert_eq!(
                respond(&gemini, verb, Some("hello")),
                None,
                "gemini {verb} must stay silent"
            );
        }

        // The declined-call shapes, exactly as the three fixtures pin them:
        // two families nest the decision beside the event name, in the same
        // envelope and under the same two keys, and the third answers a flat
        // decision object. Which envelope a family takes is a fact about that
        // family, so both are written out here as bytes.
        let reason = "[rift] a rendered reason";
        assert_eq!(
            gate(&claude, "PreToolUse", &GateDecision::Deny { reason }),
            Some(json!({"hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }}))
        );
        assert_eq!(
            gate(&agy, "PreToolUse", &GateDecision::Deny { reason }),
            Some(json!({"decision": "deny", "reason": reason}))
        );
        assert_eq!(
            gate(
                &AgentId::CODEX,
                "PreToolUse",
                &GateDecision::Deny { reason }
            ),
            Some(json!({"hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }}))
        );

        // A pinned channel is not automatically a wired one. Every boundary row
        // in the table is written on now — including the bounded one, whose
        // caller fits its text to the number Rift declared — and every declined
        // call above is composed here; whether one is ever *asked* for is the
        // repository's policy to say, and no part of that decision is here.
        assert!(capability_rows(&claude)
            .contains(&"verb=SessionStart channel=AdditionalContext limit_field=none".to_owned()));
        assert_eq!(
            respond(&claude, "SessionStart", Some("hello")),
            Some(json!({"hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": "hello",
            }}))
        );
        assert!(capability_rows(&AgentId::CODEX).contains(
            &"verb=UserPromptSubmit channel=AdditionalContext limit_field=additionalContextLimit"
                .to_owned()
        ));
        assert_eq!(
            respond(&AgentId::CODEX, "UserPromptSubmit", Some("hello")),
            Some(json!({"hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": "hello",
            }}))
        );
        assert!(capability_rows(&agy)
            .contains(&"verb=PreInvocation channel=InjectSteps kind=user_message".to_owned()));
        assert_eq!(
            respond(&agy, "PreInvocation", Some("hello")),
            Some(json!({"injectSteps": [{"userMessage": "hello"}]}))
        );
        assert_eq!(
            respond(&agy, "PostInvocation", Some("hello")),
            Some(json!({}))
        );
    }

    /// **The declined shapes as bytes**, because a client reads bytes and not
    /// a value tree.
    ///
    /// Two families are declined and each has its own envelope: one nests the
    /// decision beside the event name, the other answers a flat object. A
    /// field in the right place with the wrong shape around it is a call that
    /// runs where Rift meant to decline it, or an agent handed a document it
    /// cannot parse — neither of which a comparison of parsed values would
    /// catch. Each family's own artifact pins the same object beside the
    /// recorded run it came from and its adapter asserts the equality against
    /// that pin; the literals here are what makes this table readable as one
    /// page.
    #[test]
    fn the_declined_envelopes_are_the_bytes_each_family_is_pinned_to() {
        let reason = "[rift] a rendered reason";
        let quoted = serde_json::to_string(reason).expect("a string serialises");
        let bytes = |agent: &AgentId| {
            let value = gate(agent, "PreToolUse", &GateDecision::Deny { reason })
                .expect("a confirmed reason channel is answered on");
            serde_json::to_string(&value).expect("a response serialises")
        };
        assert_eq!(
            bytes(&AgentId::ANTIGRAVITY),
            format!("{{\"decision\":\"deny\",\"reason\":{quoted}}}")
        );
        assert_eq!(
            bytes(&AgentId::CLAUDE_CODE),
            format!(
                "{{\"hookSpecificOutput\":{{\"hookEventName\":\"PreToolUse\",\
                 \"permissionDecision\":\"deny\",\"permissionDecisionReason\":{quoted}}}}}"
            )
        );
    }

    #[test]
    fn native_context_encoding_preserves_the_dispatchers_plan_verbatim() {
        let plan = ContextPlan::Deliver {
            kind: ContextKind::ReadReplacement,
            text: "[rift] prepared result".to_owned(),
            delivered_tokens: 5,
            avoided_tokens: 30,
            fallback: true,
        };
        assert_eq!(
            respond_with_plan(
                &AgentId::ANTIGRAVITY,
                "PreInvocation",
                &plan,
                &GateDecision::Allow,
            ),
            Some(json!({"injectSteps": [{"userMessage": "[rift] prepared result"}]}))
        );
        assert_eq!(
            respond_with_plan(
                &AgentId::CODEX,
                "UserPromptSubmit",
                &plan,
                &GateDecision::Allow,
            ),
            Some(json!({"hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": "[rift] prepared result",
            }}))
        );
        let shell_plan = ContextPlan::Deliver {
            kind: ContextKind::ReadReplacement,
            text: "[rift] it's already held".to_owned(),
            delivered_tokens: 6,
            avoided_tokens: 30,
            fallback: false,
        };
        assert_eq!(
            respond_with_plan(
                &AgentId::CODEX,
                "PreToolUse",
                &shell_plan,
                &GateDecision::Deny {
                    reason: "the raw read is redundant",
                },
            ),
            Some(json!({"hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "updatedInput": {
                    "command": "printf '%s\\n' '[rift] it'\\''s already held'",
                },
            }}))
        );
        assert!(delivers_read_replacement(
            &respond_with_plan(
                &AgentId::CODEX,
                "PreToolUse",
                &shell_plan,
                &GateDecision::Deny {
                    reason: "the raw read is redundant",
                },
            )
            .expect("the replacement is encoded")
        ));
        assert!(!delivers_read_replacement(
            &json!({"hookSpecificOutput": {"additionalContext": "not a replacement"}})
        ));
    }

    /// **A step channel writes one step, of the kind its row names, carrying
    /// nothing else.** The array holds exactly one object, that object holds
    /// exactly the one text field the recorded run chose, and no response on
    /// this family ever carries the field that would steer the agent's loop.
    ///
    /// The kind is the load-bearing half. Both text kinds reach the model at
    /// the invocation they are injected into; a few invocations later only the
    /// user kind is still in its reach, and every fact this channel carries —
    /// a session pointer, what the context holds, what it stopped holding — is
    /// read later than that. A row that named the transient kind would inject
    /// on time and be gone by the time it mattered.
    #[test]
    fn a_step_channel_writes_one_step_of_the_kind_its_row_names() {
        let agy = AgentId::ANTIGRAVITY;
        let out = respond(&agy, "PreInvocation", Some("[rift] a pointer")).expect("a response");
        let steps = out["injectSteps"]
            .as_array()
            .expect("the steps travel in an array");
        assert_eq!(steps.len(), 1, "one boundary, one step: {out}");
        let step = steps[0].as_object().expect("a step is an object");
        assert_eq!(
            step.keys().collect::<Vec<_>>(),
            ["userMessage"],
            "the kind the recorded run found still readable, and no other field"
        );
        assert_eq!(step["userMessage"], "[rift] a pointer");
        assert!(
            !out.to_string().contains("terminationBehavior"),
            "a hook that steers the agent's loop is a hook that blocks it: {out}"
        );
        // A kind that is not text has nothing to write, so the family answers
        // its acknowledgement instead of a step Rift composed.
        assert_eq!(InjectKind::ToolCall.message_field(), None);
        assert_eq!(InjectKind::UserMessage.message_field(), Some("userMessage"));
        assert_eq!(
            InjectKind::EphemeralMessage.message_field(),
            Some("ephemeralMessage")
        );
    }

    /// Text offered to a family with no channel is dropped, never emitted in
    /// an invented shape — the caller may compute a pointer without knowing
    /// the table, and no hook API ever sees JSON Rift guessed at.
    #[test]
    fn offering_a_pointer_to_a_channelless_family_never_invents_a_shape() {
        for agent in AgentId::FIRST_PARTY {
            if accepts_injection(agent, "UserPromptSubmit") {
                continue;
            }
            let out = respond(agent, "UserPromptSubmit", Some("pointer"));
            let neutral_out = respond(agent, "UserPromptSubmit", None);
            assert_eq!(
                out,
                neutral_out,
                "{} responded differently because a pointer was offered",
                agent.as_str()
            );
            if let Some(v) = out {
                assert!(
                    !v.to_string().contains("pointer"),
                    "{} leaked the pointer into a response shape it does not have",
                    agent.as_str()
                );
            }
        }
    }

    /// An empty pointer is not a pointer: the response falls back to the
    /// family's neutral contract rather than injecting nothing loudly.
    #[test]
    fn an_empty_pointer_injects_nothing() {
        assert_eq!(
            respond(&AgentId::CLAUDE_CODE, "UserPromptSubmit", Some("")),
            None
        );
    }

    /// Every family's injection points are exactly the boundary rows its
    /// pinned surface carries — including the family whose channel this module
    /// has a row for and no writer yet.
    ///
    /// The two halves are stated apart because they diverge on purpose: the
    /// **row** exists as soon as the fixture pins it, which is what the
    /// per-family wiring is built against; the **writer** exists only where the
    /// shape it would write is the shape the family documented. A family whose
    /// channel bounds its text is a row here and silence on the wire until its
    /// own ticket fits the text to that bound.
    #[test]
    fn injection_points_are_the_pinned_boundary_rows_and_a_result_tag_is_not_one() {
        let rows = |agent: &AgentId| -> Vec<String> {
            injection_points(agent)
                .iter()
                .map(|(verb, channel)| format!("verb={verb} {channel}"))
                .collect()
        };
        assert_eq!(
            rows(&AgentId::CLAUDE_CODE),
            [
                "verb=UserPromptSubmit channel=AdditionalContext limit_field=none",
                "verb=SessionStart channel=AdditionalContext limit_field=none",
                "verb=SubagentStart channel=AdditionalContext limit_field=none",
            ]
        );
        assert_eq!(
            rows(&AgentId::CODEX),
            [
                "verb=UserPromptSubmit channel=AdditionalContext \
                 limit_field=additionalContextLimit",
                "verb=SessionStart channel=AdditionalContext limit_field=additionalContextLimit",
            ]
        );
        assert_eq!(
            rows(&AgentId::ANTIGRAVITY),
            ["verb=PreInvocation channel=InjectSteps kind=user_message"]
        );
        // Every row this returns is a row the family's fixture pins, because
        // it is the same table read twice.
        for agent in [AgentId::CLAUDE_CODE, AgentId::CODEX, AgentId::ANTIGRAVITY] {
            let pinned = capability_rows(&agent);
            for row in rows(&agent) {
                assert!(pinned.contains(&row), "{row} is not a pinned row");
            }
            assert!(
                !injection_points(&agent)
                    .iter()
                    .any(|(verb, _)| *verb == "PostToolUse"),
                "a context field beside a result is a tag, not a boundary: {}",
                agent.as_str()
            );
        }
        // The writer is narrower than the table, and says so in both
        // directions.
        assert!(accepts_injection(&AgentId::CLAUDE_CODE, "SessionStart"));
        assert!(accepts_injection(&AgentId::CLAUDE_CODE, "UserPromptSubmit"));
        assert!(accepts_injection(&AgentId::CLAUDE_CODE, "SubagentStart"));
        assert!(!accepts_injection(&AgentId::CLAUDE_CODE, "PostToolUse"));
        assert!(accepts_injection(&AgentId::CODEX, "SessionStart"));
        assert!(accepts_injection(&AgentId::ANTIGRAVITY, "PreInvocation"));
    }

    /// A limit is stated for exactly the rows whose pinned surface names the
    /// key it is written under, and for nothing else.
    ///
    /// The two directions matter separately. A family that declares no limit
    /// must answer `None` rather than a default, or a caller would fit text to
    /// a bound nobody set; and a channel that is not a context field has no
    /// limit to state at all, however bounded its own contents are.
    #[test]
    fn a_context_limit_is_declared_where_the_pinned_row_names_its_key() {
        // The shape first: the same context field Claude Code is written on,
        // on both of this family's boundaries, exactly as its own generated
        // schemas pin it — a bounded channel is not a different channel.
        for verb in ["UserPromptSubmit", "SessionStart"] {
            let out = respond(&AgentId::CODEX, verb, Some("hello")).expect("a response");
            assert_eq!(out["hookSpecificOutput"]["hookEventName"], verb);
            assert_eq!(out["hookSpecificOutput"]["additionalContext"], "hello");
        }
        assert_eq!(respond(&AgentId::CODEX, "Stop", Some("hello")), None);
        for verb in ["UserPromptSubmit", "SessionStart"] {
            assert_eq!(
                declared_context_limit(&AgentId::CODEX, verb),
                Some(DECLARED_CONTEXT_LIMIT_TOKENS),
                "codex {verb}"
            );
            assert_eq!(
                declared_context_limit(&AgentId::CLAUDE_CODE, verb),
                None,
                "claude-code {verb} pins the field and no bound on it"
            );
        }
        assert_eq!(
            declared_context_limit(&AgentId::ANTIGRAVITY, "PreInvocation"),
            None,
            "a trajectory step is not a context field"
        );
        for verb in ["PreToolUse", "PostToolUse", "Stop", ""] {
            assert_eq!(
                declared_context_limit(&AgentId::CODEX, verb),
                None,
                "{verb}"
            );
        }
        assert_eq!(
            declared_context_limit(&AgentId::new("some-third-party-agent"), "UserPromptSubmit"),
            None
        );
    }

    /// An unknown third-party agent is silent on every verb: Rift never
    /// answers a hook API it has no contract with.
    #[test]
    fn an_unknown_agent_is_always_silent() {
        let stranger = AgentId::new("some-third-party-agent");
        assert!(injection_points(&stranger).is_empty());
        for verb in ["UserPromptSubmit", "PreToolUse", "Stop", ""] {
            assert_eq!(respond(&stranger, verb, Some("pointer")), None);
        }
    }
}
