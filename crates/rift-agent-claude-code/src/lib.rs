//! First-party Rift adapter for **Claude Code**.
//!
//! Claude Code exposes a rich hook set including compaction. Note the lifecycle
//! distinction the canonical vocabulary preserves: `Stop` ends an assistant
//! **turn**, while `SessionEnd` terminates the **session** — they map to distinct
//! canonical kinds (`TurnEnd` vs `SessionEnd`).
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use rift_agent::{
    event, settle::StopSentinel, shell_read, AdapterError, AgentAdapter, CompatNotes, EditKind,
    EditRequest, HiKind, HookFacts, ReadChannel, ReadRequest, ReadSpan, SearchKind, SearchRequest,
    TokenUsage,
};
use rift_core::{AgentId, CanonicalEvent, EventKind, Trust};

mod stop_hook;
pub mod transcript;

pub use stop_hook::{STOP_HOOK_COMMAND, STOP_SENTINEL};

/// The Claude Code adapter.
pub struct ClaudeCodeAdapter;

/// The Claude Code tools whose use means a file on disk changed.
///
/// `NotebookEdit` and the two `mcp__acp__*` bridge tools were missing: a
/// notebook edit, and every edit made through an ACP client, produced a
/// `ToolCall` with no `FileEdit` beside it, so the change was invisible to
/// everything downstream that reads edit events.
///
/// `MultiEdit` is kept from the original list: it is a real Claude Code tool
/// and dropping it could only lose capture.
pub const FILE_MUTATING_TOOLS: &[&str] = &[
    "Write",
    "Edit",
    "MultiEdit",
    "NotebookEdit",
    "mcp__acp__Write",
    "mcp__acp__Edit",
];

/// The tool Claude Code dispatches a **subagent** with.
///
/// It is `Agent`. Claude Code never exposed a tool named `Task`, and a hook
/// matcher written against `Task` silently never fires — the failure mode is
/// not an error but an absence, which is why it survives so long. The name is
/// pinned here so a future subagent-nesting change starts from the verified
/// spelling rather than from the plausible one.
pub const SUBAGENT_TOOL: &str = "Agent";

/// The argument names this adapter reads a tool call through, spelled once.
///
/// They are the values of `fixtures/claude-code-tool-field-surface.md`'s rows,
/// and `pinned_tool_field_surface` asserts the two agree — the pin is not a
/// document *about* this code, it is where these strings come from.
mod field {
    /// `Read`, `Edit`, `Write` and the rest: the path.
    pub(crate) const PATH: &str = "file_path";
    /// `Read`: the first line, counting from 1.
    pub(crate) const OFFSET: &str = "offset";
    /// `Read`: how many lines, not which line to stop at.
    pub(crate) const LIMIT: &str = "limit";
    /// `Write`: the whole new file.
    pub(crate) const CONTENT: &str = "content";
    /// `Edit`: the text written in.
    pub(crate) const NEW_STRING: &str = "new_string";
    /// `Edit`: the text taken out.
    pub(crate) const OLD_STRING: &str = "old_string";
    /// `Edit`: whether every occurrence was meant.
    pub(crate) const REPLACE_ALL: &str = "replace_all";
    /// `Glob`: the name pattern.
    pub(crate) const PATTERN: &str = "pattern";
    /// `Grep`: the result cap, when the caller set one.
    pub(crate) const HEAD_LIMIT: &str = "head_limit";
    /// `Bash`: the shell command line itself.
    pub(crate) const COMMAND: &str = "command";
}

/// The tool whose one argument is a shell command line.
///
/// `PowerShell` carries the same argument and is deliberately absent: the
/// grammar Rift recognises is a list of POSIX commands, and that tool runs a
/// language in which several of those names mean other programs.
const SHELL_TOOL: &str = "Bash";

/// How many lines a stretch of text spans — zero for the empty string, else
/// one more than the newlines it contains, whether or not it ends in one.
fn line_span(text: &str) -> u32 {
    if text.is_empty() {
        return 0;
    }
    let breaks = u32::try_from(text.matches('\n').count()).unwrap_or(u32::MAX);
    breaks.saturating_add(1)
}

impl ClaudeCodeAdapter {
    /// The agent id this adapter serves.
    pub const AGENT_ID: AgentId = AgentId::CLAUDE_CODE;

    fn tool_name(native: &serde_json::Value) -> Option<&str> {
        native.get("tool_name").and_then(serde_json::Value::as_str)
    }

    /// The tool's own arguments, as an object.
    fn tool_input(
        native: &serde_json::Value,
    ) -> Option<&serde_json::Map<String, serde_json::Value>> {
        native
            .get("tool_input")
            .and_then(serde_json::Value::as_object)
    }

    /// A non-empty string argument.
    fn arg_str<'a>(
        input: &'a serde_json::Map<String, serde_json::Value>,
        key: &str,
    ) -> Option<&'a str> {
        input
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
    }

    /// The path argument, which every file-shaped tool here spells the same.
    fn arg_path(input: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
        Self::arg_str(input, field::PATH).map(str::to_owned)
    }

    /// What a shell call was, when the command line is one the pinned
    /// grammar spells and nothing at all otherwise.
    ///
    /// The recogniser is shared: this family publishes the grammar, and the
    /// other two run the same commands through their own shell tools, so one
    /// reading of a command line serves all three and only the argument's
    /// name differs.
    fn shell_call(native: &serde_json::Value) -> Option<shell_read::ShellCall> {
        let input = Self::tool_input(native)?;
        shell_read::recognise(Self::arg_str(input, field::COMMAND)?)
    }
}

impl AgentAdapter for ClaudeCodeAdapter {
    fn agent_id(&self) -> AgentId {
        Self::AGENT_ID
    }

    fn map(
        &self,
        verb: &str,
        native: &serde_json::Value,
    ) -> Result<Vec<CanonicalEvent>, AdapterError> {
        use EventKind::{
            FileEdit, PromptSubmit, SessionCompaction, SessionEnd, SessionStart, SubagentEnd,
            SubagentStart, ToolCall, ToolResult, TurnEnd,
        };
        use Trust::{CodeFact, GitFact, ToolOutputUntrusted, TranscriptUntrusted};
        Ok(match verb {
            "SessionStart" => vec![event(SessionStart, GitFact)],
            "UserPromptSubmit" => vec![event(PromptSubmit, TranscriptUntrusted)],
            "PreToolUse" => {
                let mut evs = vec![event(ToolCall, CodeFact)];
                if FILE_MUTATING_TOOLS.contains(&Self::tool_name(native).unwrap_or_default()) {
                    evs.push(event(FileEdit, CodeFact));
                }
                evs
            }
            "PostToolUse" => vec![event(ToolResult, ToolOutputUntrusted)],
            "Stop" => vec![event(TurnEnd, GitFact)],
            "SubagentStart" => vec![event(SubagentStart, GitFact)],
            "SubagentStop" => vec![event(SubagentEnd, GitFact)],
            "PreCompact" => vec![event(SessionCompaction, GitFact)],
            "SessionEnd" => vec![event(SessionEnd, GitFact)],
            other => return Err(AdapterError::UnknownVerb(other.to_owned())),
        })
    }

    fn verbs(&self) -> &'static [&'static str] {
        &[
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "Stop",
            "SubagentStart",
            "SubagentStop",
            "PreCompact",
            "SessionEnd",
        ]
    }

    fn stop_sentinel(&self) -> Option<StopSentinel> {
        Some(STOP_SENTINEL)
    }

    fn compat_notes(&self) -> CompatNotes {
        CompatNotes {
            agent: Self::AGENT_ID,
            lifecycle_gaps: &[],
            surfaces_token_usage: true,
            best_effort: false,
            note: "Rich hook set incl. compaction; usage when present in tool/turn payloads.",
        }
    }

    /// The default plus **the child session id Claude Code actually sends**.
    ///
    /// The default reads `subagent_session_id`/`child_session_id`/
    /// `agent_session_id`; the pinned Agent SDK types name the field
    /// **`agent_id`** — required on `SubagentStartHookInput` and
    /// `SubagentStopHookInput`, and present on tool-lifecycle inputs whenever
    /// the hook fires from inside a Task-spawned subagent. So the id was on the
    /// wire and no adapter read it, and every subagent's work was attributed
    /// flat to the parent turn with no nesting at all. That was the true half
    /// of the `claude-code.subagent_boundary` sweep row; the pin is what showed
    /// the row was naming the wrong field on the wrong hook.
    fn facts(&self, verb: &str, native: &serde_json::Value) -> HookFacts {
        let mut facts = rift_agent::default_facts(verb, native);
        if facts.child_session.is_none() {
            facts.child_session = native
                .get("agent_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
        }
        facts
    }

    /// `Read`, and only through the three arguments the pin names.
    ///
    /// The strictness is the substance. `offset` counts from line 1 and
    /// `limit` is a count, so the tool can have delivered
    /// `offset ..= offset + limit - 1` and no more; with `limit` absent the
    /// tool reads on to the end of the file and stops early only where a token
    /// budget runs out, which is a function of the bytes and not of any number
    /// anyone published — so the range runs to the end of the file and the
    /// size at which it stops being vouchable is applied where the bytes are.
    ///
    /// **A fourth argument takes the call out of the shape altogether.** `pages`
    /// selects pages of a PDF and a notebook read returns cells: neither has a
    /// line range, and an argument that arrived after this pin was taken is
    /// one whose effect on the range nobody has checked. Answering `None` to
    /// all of them costs a saving; answering with lines would record a context
    /// as holding text it never received.
    fn read_request(&self, _verb: &str, native: &serde_json::Value) -> Option<ReadRequest> {
        if Self::tool_name(native)? == SHELL_TOOL {
            return Self::shell_call(native)?.read_request();
        }
        if Self::tool_name(native)? != "Read" {
            return None;
        }
        let input = Self::tool_input(native)?;
        if input
            .keys()
            .any(|key| !matches!(key.as_str(), field::PATH | field::OFFSET | field::LIMIT))
        {
            return None;
        }
        let path = Self::arg_path(input)?;
        let lo = match input.get(field::OFFSET) {
            None => 1,
            Some(value) => u32::try_from(value.as_u64()?).ok()?.max(1),
        };
        let hi_kind = match input.get(field::LIMIT) {
            // No cap: the client reads from `lo` to the end of the file and
            // stops only if a token cap cuts it short, in which case the
            // result says so in a field (the tool-field pin's O7 for the
            // whole delivery, O5 for the truncation). So the range this can
            // have delivered ends at the file's last line — which the adapter
            // does not know and never opens a file to learn. It states the end
            // of the file and the resolution step, where the bytes are already
            // in hand for hashing, clamps it; the delivery bound that decides
            // whether the whole range may be *credited* is applied there too.
            None => HiKind::DefaultWindow(HiKind::END_OF_FILE),
            // A zero-line read delivers nothing, and nothing is not a range
            // this can hold: recording the line it would have started on would
            // claim one line too many.
            Some(value) => match u32::try_from(value.as_u64()?).ok()? {
                0 => return None,
                lines => HiKind::Explicit(lo.saturating_add(lines - 1)),
            },
        };
        Some(ReadRequest {
            path,
            span: ReadSpan::lines(lo, hi_kind),
            channel: ReadChannel::Tool,
        })
    }

    /// The `Read` tool reads one file; the shell tool reads as many as its
    /// command line spells.
    ///
    /// A `cd` into a crate followed by two `cat`s is one `Bash` call and two
    /// reads, and it is the shape this family's agents actually write — so
    /// answering with the first alone would record a third of a call and
    /// leave the rest of it outside every figure that divides by what was
    /// read.
    fn read_requests(&self, verb: &str, native: &serde_json::Value) -> Vec<ReadRequest> {
        if Self::tool_name(native) == Some(SHELL_TOOL) {
            return Self::shell_call(native)
                .map(|call| call.read_requests())
                .unwrap_or_default();
        }
        self.read_request(verb, native).into_iter().collect()
    }

    /// The file-mutating tools, split by whether one edit site follows from
    /// the payload.
    ///
    /// `Write` carries the whole new file, so its hash is the hash the file
    /// should have afterwards — the equality that later separates "the agent
    /// wrote these bytes" from "something reformatted them on save". `Edit`
    /// carries the pair, and the tool's own uniqueness rule is why one site
    /// follows from it. Everything else — several edits in one call, a
    /// notebook cell, an edit routed through a bridge whose payload no
    /// artifact pins — is a change with no derivable site, which is what
    /// [`EditKind::Other`] says.
    fn edit_request(&self, _verb: &str, native: &serde_json::Value) -> Option<EditRequest> {
        let tool = Self::tool_name(native)?;
        if !FILE_MUTATING_TOOLS.contains(&tool) {
            return None;
        }
        let input = Self::tool_input(native)?;
        let path = Self::arg_path(input)?;
        let kind = match tool {
            "Write" => match Self::arg_str(input, field::CONTENT) {
                Some(content) => EditKind::Write {
                    content_sha256: rift_agent::content_sha256(content.as_bytes()),
                },
                None => EditKind::Other,
            },
            "Edit" => match (
                Self::arg_str(input, field::NEW_STRING),
                input
                    .get(field::OLD_STRING)
                    .and_then(serde_json::Value::as_str),
            ) {
                (Some(new_string), Some(old_string)) => EditKind::Edit {
                    new_string: new_string.to_owned(),
                    old_string_lines: line_span(old_string),
                    replace_all: input
                        .get(field::REPLACE_ALL)
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                },
                _ => EditKind::Other,
            },
            _ => EditKind::Other,
        };
        Some(EditRequest { path, kind })
    }

    /// `Glob` is a name search; `Grep` is a content search and stays one.
    ///
    /// The only thing worth knowing about a `Grep` here is whether the caller
    /// already bounded its results. A caller that set `head_limit` chose a
    /// number — including the explicit "no bound" — and a choice already made
    /// is not one to make again; a caller that set none left the bound to
    /// whatever the tool does.
    fn search_request(&self, _verb: &str, native: &serde_json::Value) -> Option<SearchRequest> {
        let input = Self::tool_input(native)?;
        let kind = match Self::tool_name(native)? {
            "Glob" => SearchKind::Glob {
                pattern: Self::arg_str(input, field::PATTERN)?.to_owned(),
            },
            "Grep" => SearchKind::Grep {
                has_head_limit: input.contains_key(field::HEAD_LIMIT),
            },
            SHELL_TOOL => return Self::shell_call(native)?.search_request(),
            _ => return None,
        };
        Some(SearchRequest { kind })
    }

    /// The one file a content search named, where the grammar named it.
    ///
    /// The tool-shaped content search is deliberately absent: this family's
    /// pinned page names `head_limit`, `offset`, `glob`, `type`,
    /// `output_mode` and `multiline` as that tool's arguments and does not
    /// name the one that scopes it to a path, so no row claims it and nothing
    /// here reads it.
    fn searched_file(&self, _verb: &str, native: &serde_json::Value) -> Option<String> {
        Self::shell_call(native)?.searched_file().map(str::to_owned)
    }

    fn usage(&self, verb: &str, native: &serde_json::Value) -> Option<TokenUsage> {
        if !matches!(verb, "PostToolUse" | "Stop") {
            return None;
        }
        let u = TokenUsage {
            input: rift_agent::read_count(native, &["usage", "input_tokens"]),
            output: rift_agent::read_count(native, &["usage", "output_tokens"]),
            cache_read: rift_agent::read_count(native, &["usage", "cache_read_input_tokens"]),
            cache_write: rift_agent::read_count(native, &["usage", "cache_creation_input_tokens"]),
        };
        u.is_present().then_some(u)
    }

    fn main_context_usage(&self, verb: &str, native: &serde_json::Value) -> Option<TokenUsage> {
        if !matches!(verb, "PreToolUse" | "Stop") {
            return None;
        }
        let u = TokenUsage {
            input: rift_agent::read_count(native, &["context_usage", "input_tokens"]),
            output: rift_agent::read_count(native, &["context_usage", "output_tokens"]),
            cache_read: rift_agent::read_count(
                native,
                &["context_usage", "cache_read_input_tokens"],
            ),
            cache_write: rift_agent::read_count(
                native,
                &["context_usage", "cache_creation_input_tokens"],
            ),
        };
        u.is_present().then_some(u)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_agent::conformance::{check, Case};
    use rift_core::EventKind::{
        FileEdit, PromptSubmit, SessionCompaction, SessionEnd, SessionStart, SubagentEnd,
        SubagentStart, ToolCall, ToolResult, TurnEnd,
    };

    fn cases() -> Vec<Case> {
        vec![
            Case::new("SessionStart", serde_json::json!({}), &[SessionStart]),
            Case::new(
                "UserPromptSubmit",
                serde_json::json!({"text": "hi"}),
                &[PromptSubmit],
            ),
            Case::new(
                "PreToolUse",
                serde_json::json!({"tool_name": "Bash"}),
                &[ToolCall],
            ),
            Case::new(
                "PreToolUse",
                serde_json::json!({"tool_name": "Edit"}),
                &[ToolCall, FileEdit],
            ),
            Case::new(
                "PostToolUse",
                serde_json::json!({"tool_name": "Bash"}),
                &[ToolResult],
            ),
            Case::new("Stop", serde_json::json!({}), &[TurnEnd]),
            Case::new("SessionEnd", serde_json::json!({}), &[SessionEnd]),
            Case::new(
                "SubagentStart",
                serde_json::json!({"agent_id": "child-1", "agent_type": "explore"}),
                &[SubagentStart],
            ),
            Case::new(
                "SubagentStop",
                serde_json::json!({"agent_id": "child-1", "agent_type": "explore"}),
                &[SubagentEnd],
            ),
            Case::new(
                "PreCompact",
                serde_json::json!({"trigger": "auto"}),
                &[SessionCompaction],
            ),
        ]
    }

    #[test]
    fn conformance_kit_passes() {
        check(&ClaudeCodeAdapter, &cases()).unwrap();
    }

    /// **The subagent id Claude Code sends is the one Rift reads** — the true
    /// half of the `claude-code.subagent_boundary` sweep row, closed against
    /// the pinned Agent SDK types rather than against a wrapper's guess.
    ///
    /// The row named `tool_response.agentId` on `PostToolUse[Agent]`. The
    /// pinned schema names `agent_id`, required on `SubagentStartHookInput`
    /// and `SubagentStopHookInput` and present on tool-lifecycle inputs fired
    /// from inside a subagent — a strictly better boundary, because it arrives
    /// at the start of the child's work rather than at the parent tool's
    /// return. The old default read three names, none of them this one.
    #[test]
    fn the_subagent_id_on_the_wire_becomes_the_child_session() {
        for verb in ["SubagentStart", "SubagentStop", "PreToolUse"] {
            let facts = ClaudeCodeAdapter.facts(
                verb,
                &serde_json::json!({"agent_id": "child-7", "agent_type": "explore"}),
            );
            assert_eq!(
                facts.child_session.as_deref(),
                Some("child-7"),
                "{verb} carries the child id and it must not be dropped"
            );
        }
        // A main-thread hook has no `agent_id`, and absence stays absence — the
        // adapter honesty rule; a fabricated child id would nest a turn under
        // itself.
        assert!(ClaudeCodeAdapter
            .facts("PreToolUse", &serde_json::json!({"tool_name": "Bash"}))
            .child_session
            .is_none());
        // …and the override must not have cost the convention it refines.
        let read = ClaudeCodeAdapter.facts(
            "PreToolUse",
            &serde_json::json!({"tool_name": "Read", "tool_input": {"file_path": "/repo/a.rs"}}),
        );
        assert_eq!(read.read_path.as_deref(), Some("/repo/a.rs"));
    }

    /// **Every verb the adapter declares is driven by a conformance case.**
    ///
    /// The three other supported adapters carried this assertion first;
    /// Claude Code — the one agent this repository actually captures from —
    /// did not, so `SubagentStart`, `SubagentStop` and `PreCompact`
    /// were declared, installed and never once driven through `map` in a test.
    #[test]
    fn every_declared_verb_is_covered_by_a_conformance_case() {
        let cases = cases();
        for verb in ClaudeCodeAdapter.verbs() {
            assert!(
                cases.iter().any(|case| case.verb == *verb),
                "{verb} is declared and has no conformance case"
            );
        }
    }

    /// Each of these is a tool whose use means a file on disk changed.
    /// Three of them used to produce a bare `ToolCall`: a notebook edit, and
    /// every edit made through an ACP client, were captured as "a tool ran"
    /// with no record that the working tree moved.
    #[test]
    fn every_file_mutating_tool_fans_out_to_a_file_edit() {
        for tool in FILE_MUTATING_TOOLS {
            let kinds: Vec<_> = ClaudeCodeAdapter
                .map("PreToolUse", &serde_json::json!({"tool_name": tool}))
                .unwrap()
                .iter()
                .map(|e| e.kind)
                .collect();
            assert_eq!(kinds, vec![ToolCall, FileEdit], "{tool} must imply an edit");
        }
        // A read-shaped tool still must not claim an edit.
        for tool in ["Read", "Bash", "Grep", SUBAGENT_TOOL] {
            let kinds: Vec<_> = ClaudeCodeAdapter
                .map("PreToolUse", &serde_json::json!({"tool_name": tool}))
                .unwrap()
                .iter()
                .map(|e| e.kind)
                .collect();
            assert_eq!(kinds, vec![ToolCall], "{tool} does not edit a file");
        }
    }

    /// The subagent dispatch tool is `Agent`. A hook matcher written against
    /// `Task` never fires — silently, because the failure is an absence.
    #[test]
    fn the_subagent_tool_is_named_agent_not_task() {
        assert_eq!(SUBAGENT_TOOL, "Agent");
        assert!(!FILE_MUTATING_TOOLS.contains(&SUBAGENT_TOOL));
    }

    #[test]
    fn unknown_verb_errors_not_dropped() {
        assert!(matches!(
            ClaudeCodeAdapter.map("MysteryVerb", &serde_json::Value::Null),
            Err(AdapterError::UnknownVerb(_))
        ));
    }

    #[test]
    fn token_usage_extracted_when_present_never_fabricated() {
        let with = serde_json::json!({"usage": {"input_tokens": 10, "output_tokens": 3}});
        let u = ClaudeCodeAdapter.usage("Stop", &with).unwrap();
        assert_eq!(u.input, Some(10));
        assert_eq!(u.output, Some(3));
        assert_eq!(u.cache_read, None);
        // absent usage → no fabricated counts
        assert!(ClaudeCodeAdapter
            .usage("Stop", &serde_json::json!({}))
            .is_none());
        assert!(ClaudeCodeAdapter.usage("SessionStart", &with).is_none());
    }

    #[test]
    fn main_context_extraction_is_opt_in_and_separate() {
        let payload = serde_json::json!({
            "usage": {"input_tokens": 900_000},
            "context_usage": {
                "input_tokens": 10,
                "output_tokens": 2,
                "cache_read_input_tokens": 30,
                "cache_creation_input_tokens": 4
            }
        });
        let context = ClaudeCodeAdapter
            .main_context_usage("PreToolUse", &payload)
            .unwrap();
        assert_eq!(context.input, Some(10));
        assert_eq!(context.cache_read, Some(30));
        assert!(ClaudeCodeAdapter
            .main_context_usage("PostToolUse", &payload)
            .is_none());
    }
}

/// **Claude Code's own published hook surface, pinned into this tree** (an
/// owner decision of 2026-08-24).
///
/// The artifact is `fixtures/claude-code-hook-surface.md`, and it pins two
/// Anthropic-published enumerations because neither one alone is the surface:
/// the Agent SDK's `HookEvent` union (the events a program may register a
/// callback for) and the `plugin-dev` hook-development skill's `## Hook
/// Events` section (the events a `settings.json` may key). Each carries names
/// the other omits, so the published surface is their union.
///
/// **Why it earns its place, in one measurement.** The sweep row
/// `claude-code.subagent_boundary` recorded *"Rift declares `SubagentStart`,
/// which Claude Code does not publish"*, cited to a third party's Go wrapper
/// rather than to Anthropic — whose own type union lists `SubagentStart`.
/// The sentence was true of the wrapper and false of the agent, and the same
/// mistake had already been made in the opposite direction for Codex.
#[cfg(test)]
mod pinned_surface {
    use super::{AgentAdapter, ClaudeCodeAdapter};

    /// The committed excerpt. `include_str!` rather than a runtime read so a
    /// missing or renamed fixture is a compile error, not a skipped test.
    const FIXTURE: &str = include_str!("../fixtures/claude-code-hook-surface.md");

    /// A provenance field of one pinned artifact (`key: value`, indented).
    ///
    /// `nth` selects the artifact: 0 is the Agent SDK, 1 is the skill. Two
    /// artifacts means two `blob-sha:` lines, and a reader of one number must
    /// be able to say which file it belongs to.
    fn provenance(field: &str, nth: usize) -> &'static str {
        let needle = format!("{field}:");
        FIXTURE
            .lines()
            .filter_map(|line| line.trim().strip_prefix(&needle))
            .map(str::trim)
            .nth(nth)
            .unwrap_or_else(|| panic!("the pin states no {field} #{nth}"))
    }

    /// Every hook event the pin publishes: the union of the two artifacts.
    ///
    /// Read only from inside the fenced blocks, so the prose above them — which
    /// necessarily names events while explaining which artifact omits which —
    /// cannot smuggle a name into the enumeration. `python` fences contribute
    /// their `Literal["…"]`s; `markdown` fences contribute their `### …`
    /// headings.
    fn published_verbs() -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        let mut fence: Option<&str> = None;
        for line in FIXTURE.lines() {
            if let Some(lang) = line.strip_prefix("```") {
                fence = if fence.is_some() {
                    None
                } else {
                    Some(lang.trim())
                };
                continue;
            }
            let found = match fence {
                Some("python") => line
                    .split_once("Literal[\"")
                    .and_then(|(_, rest)| rest.split('"').next()),
                Some("markdown") => line.trim().strip_prefix("### "),
                _ => None,
            };
            if let Some(name) = found {
                if !out.contains(&name) {
                    out.push(name);
                }
            }
        }
        out
    }

    /// Both artifacts state where they came from, precisely enough that a
    /// reviewer reproduces each sha offline from the bytes alone.
    #[test]
    fn the_pin_states_a_provenance_a_reader_can_recheck() {
        assert_eq!(
            provenance("repository", 0),
            "https://github.com/anthropics/claude-agent-sdk-python"
        );
        assert_eq!(provenance("tag", 0), "v0.2.143");
        assert_eq!(provenance("path", 0), "src/claude_agent_sdk/types.py");
        assert_eq!(
            provenance("blob-sha", 0),
            "308b76cb7fd928d124666c255b253c92c343f15d"
        );
        assert_eq!(
            provenance("repository", 1),
            "https://github.com/anthropics/claude-code"
        );
        assert_eq!(
            provenance("commit", 0),
            "45bdfa96ca415da92e62b6ca85a1d6e29adf3c44"
        );
        assert_eq!(
            provenance("path", 1),
            "plugins/plugin-dev/skills/hook-development/SKILL.md"
        );
        assert_eq!(
            provenance("blob-sha", 1),
            "d1c0c199c70e8cef8fee8d3b792b0a27ff33046c"
        );
        for nth in 0..2 {
            let sha = provenance("blob-sha", nth);
            assert_eq!(sha.len(), 40, "a git blob sha is 40 hex characters");
            assert!(
                sha.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
                "blob sha must be lowercase hex"
            );
            let from = provenance("retrieved-from", nth);
            assert!(
                from.contains(provenance("path", nth)),
                "artifact #{nth}: {from} does not name the path it pins"
            );
        }
    }

    /// **Twelve, and the list itself is the deliverable** — a count proves
    /// nothing a reviewer can check.
    #[test]
    fn the_pin_is_a_closed_enumeration_of_claude_codes_published_hook_events() {
        let mut got = published_verbs();
        got.sort_unstable();
        assert_eq!(
            got,
            [
                "Notification",
                "PermissionRequest",
                "PostToolUse",
                "PostToolUseFailure",
                "PreCompact",
                "PreToolUse",
                "SessionEnd",
                "SessionStart",
                "Stop",
                "SubagentStart",
                "SubagentStop",
                "UserPromptSubmit",
            ],
            "the pin's published set moved; re-pin the surface, do not edit the \
             expectation"
        );
        // The two artifacts must each still be contributing, or a future
        // re-pin could quietly drop one half and keep the union looking right
        // only because the other half happened to grow.
        assert!(
            FIXTURE.contains("HookEvent = ("),
            "artifact A's union is missing from the excerpt"
        );
        assert!(
            FIXTURE.contains("## Hook Events"),
            "artifact B's section heading is missing from the excerpt"
        );
    }

    /// Direction one: every verb Rift declares for Claude Code is one Claude
    /// Code publishes — the direction the sweep row got backwards, now a test
    /// rather than a sentence.
    #[test]
    fn every_verb_rift_declares_for_claude_code_is_one_claude_code_publishes() {
        let published = published_verbs();
        for verb in ClaudeCodeAdapter.verbs() {
            assert!(
                published.contains(verb),
                "the adapter declares {verb}, which neither pinned Anthropic \
                 artifact publishes"
            );
        }
    }

    /// Every event Claude Code publishes that Rift does not register a hook
    /// for, and **the list itself is the deliverable**, not a count.
    ///
    /// Three names, each for its own reason:
    ///
    /// * `Notification` fires when Claude Code notifies the *user* — an
    ///   interface event about the terminal, not about the session's work.
    ///   Nothing in it changes the transcript, the tree or the turn.
    /// * `PostToolUseFailure` is the failure arm of a tool call Rift already
    ///   captures through `PostToolUse`; the canonical `ToolResult` it would
    ///   map to is the same row, so registering it would double-count a failed
    ///   tool and nothing else. Distinguishing outcome would need a field on
    ///   the canonical event, which is a schema change and deliberately not
    ///   taken here.
    /// * `PermissionRequest` carries only facts `PreToolUse` already carries,
    ///   and asks about a tool that may then be denied — the same reasoning as
    ///   the Codex adapter's row of the same name, which is where it is argued
    ///   in full.
    #[test]
    fn the_claude_code_events_rift_does_not_declare_are_enumerated_with_their_reason() {
        let declared = ClaudeCodeAdapter.verbs();
        let mut undeclared: Vec<&str> = published_verbs()
            .into_iter()
            .filter(|verb| !declared.contains(verb))
            .collect();
        undeclared.sort_unstable();
        assert_eq!(
            undeclared,
            ["Notification", "PermissionRequest", "PostToolUseFailure"],
            "a published Claude Code event is neither declared nor named here; \
             decide it, do not widen the expectation"
        );
    }
}

/// **Claude Code's hook *output* contract, pinned into this tree.**
///
/// The artifact is `fixtures/claude-code-hook-output-surface.md`, and it carries a reference page's digest and an SDK type file's tag and blob sha,
/// plus a verbatim excerpt for every field Rift is allowed to write back. The
/// standing rule that a supported agent's own published surface is the
/// authority, admitted only through a committed artifact, is what this module
/// enforces on the *response* side: it parses the pin and asserts the parse
/// equals `rift_agent::respond`'s channel table, so an edit to either one that
/// changes the answer reddens here instead of quietly widening what Rift may
/// write on a live agent's hook.
///
/// The asymmetry is the point. A channel Rift declines to use costs a missed
/// saving; a response shape Rift invented costs a broken agent.
#[cfg(test)]
mod pinned_output_surface {
    use rift_agent::respond::{capability_rows, pinned_capability_rows};
    use rift_core::AgentId;

    use super::{AgentAdapter, ClaudeCodeAdapter};

    /// The committed pin. `include_str!` rather than a runtime read so a
    /// missing or renamed fixture is a compile error, not a skipped test.
    const FIXTURE: &str = include_str!("../fixtures/claude-code-hook-output-surface.md");

    /// A provenance field of one pinned artifact (`key: value`, indented).
    ///
    /// `nth` selects the artifact: a file with several pins has several
    /// digests, and a reader of one number must be able to say which source it
    /// belongs to.
    fn provenance(field: &str, nth: usize) -> &'static str {
        let needle = format!("{field}:");
        FIXTURE
            .lines()
            .filter_map(|line| line.trim().strip_prefix(&needle))
            .map(str::trim)
            .nth(nth)
            .unwrap_or_else(|| panic!("the pin states no {field} #{nth}"))
    }

    /// Every digest is the right width, lowercase hex, and reproducible from
    /// the bytes alone by the command the pin prints beside it.
    fn assert_digest(field: &str, nth: usize, width: usize) {
        let digest = provenance(field, nth);
        assert_eq!(
            digest.len(),
            width,
            "{field} #{nth} is not {width} characters"
        );
        assert!(
            digest.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "{field} #{nth} must be lowercase hex"
        );
    }

    /// Both artifacts state where they came from, precisely enough that a
    /// reviewer reproduces each digest offline from the bytes alone. The
    /// reference page has no tag to pin, so its digest and its retrieval date
    /// are the pin; the SDK's type file has both.
    #[test]
    fn the_pin_states_a_provenance_a_reader_can_recheck() {
        assert_eq!(
            provenance("repository", 0),
            "https://docs.claude.com/en/docs/claude-code/hooks"
        );
        assert_eq!(provenance("path", 0), "/en/docs/claude-code/hooks.md");
        assert_digest("sha256", 0, 64);
        assert_eq!(
            provenance("repository", 1),
            "https://github.com/anthropics/claude-agent-sdk-python"
        );
        assert_eq!(provenance("tag", 0), "v0.2.152");
        assert_eq!(provenance("path", 1), "src/claude_agent_sdk/types.py");
        assert_digest("blob-sha", 0, 40);
        for nth in 0..2 {
            let from = provenance("retrieved-from", nth);
            assert!(
                from.contains(provenance("path", nth).trim_start_matches('/')),
                "artifact #{nth}: {from} does not name the path it pins"
            );
        }
    }

    /// **The pin and the code table are one set.** Every channel
    /// `rift_agent::respond` may answer on for this family is a row the
    /// artifact pins, and every row the artifact pins is in the table — so
    /// neither a new response shape nor a new excerpt arrives alone.
    #[test]
    fn the_capability_table_equals_the_pinned_fixtures() {
        let pinned = pinned_capability_rows(FIXTURE);
        assert!(!pinned.is_empty(), "the pin states no capability rows");
        assert_eq!(
            pinned,
            capability_rows(&AgentId::CLAUDE_CODE),
            "the pinned output surface and the capability table disagree; re-pin \
             the surface or drop the row, never edit one to match the other"
        );
    }

    /// **Both ends of a spawn are pinned to carry the prompt being
    /// processed.** The link a subagent's opening makes to the call that
    /// spawned it rests on nothing else, so the fields it reads are a block
    /// of the pin, the excerpts above it are the published page's own words,
    /// and the page they come from is pinned by digest like the others.
    #[test]
    fn both_ends_of_a_spawn_are_pinned_to_carry_the_prompt_id() {
        let block = FIXTURE
            .split("```rift-input-fields")
            .nth(1)
            .and_then(|rest| rest.split("```").next())
            .expect("the pin states the input fields a spawn link reads");
        let fields = |verb: &str| -> Vec<&str> {
            block
                .lines()
                .find_map(|line| line.trim().strip_prefix(&format!("verb={verb} fields=")))
                .unwrap_or_else(|| panic!("no input fields pinned for {verb}"))
                .split(',')
                .collect()
        };
        assert!(fields("PreToolUse").contains(&"prompt_id"));
        assert!(fields("PreToolUse").contains(&"tool_use_id"));
        assert!(fields("SubagentStart").contains(&"prompt_id"));
        assert!(fields("SubagentStart").contains(&"agent_type"));
        assert!(FIXTURE.contains(
            "| `prompt_id` | UUID identifying the user prompt currently being processed."
        ));
        assert!(FIXTURE.contains(
            "In addition to the [common input fields](#common-input-fields), SubagentStart hooks receive `agent_id`"
        ));
        // The input-field artifact is the last one the pin states.
        let last = |field: &str| {
            let needle = format!("{field}:");
            FIXTURE
                .lines()
                .filter(|line| line.trim().starts_with(&needle))
                .count()
                .saturating_sub(1)
        };
        assert_eq!(
            provenance("repository", last("repository")),
            "https://code.claude.com/docs/en/hooks"
        );
        assert_digest("sha256", last("sha256"), 64);
        assert!(provenance("retrieved-from", last("retrieved-from"))
            .ends_with(provenance("path", last("path"))));
        assert!(ClaudeCodeAdapter.verbs().contains(&"SubagentStart"));
    }

    /// A row on a verb this adapter never registers is a pin that reads as
    /// coverage and delivers none: the hook would never fire to answer on it.
    #[test]
    fn every_pinned_row_names_a_verb_this_adapter_declares() {
        for row in pinned_capability_rows(FIXTURE) {
            let verb = row
                .strip_prefix("verb=")
                .and_then(|rest| rest.split(' ').next())
                .unwrap_or_else(|| panic!("malformed capability row: {row}"));
            assert!(
                ClaudeCodeAdapter.verbs().contains(&verb),
                "the pin claims a channel on {verb}, which this adapter does not register"
            );
        }
    }
}

/// **Claude Code's read, edit and search *tool fields*, pinned into this tree.**
///
/// The artifact is `fixtures/claude-code-tool-field-surface.md`: a reference
/// page's digest, an SDK type file's tag and blob sha, and — where the page is
/// silent — recorded runs marked as observations, each with the client version
/// that produced it. This module parses its rows and asserts they equal
/// `rift_agent::tool_fields`' table for this family, and the extraction tests
/// build their payloads out of the *pinned* argument names rather than out of
/// names retyped beside the code they are meant to check.
///
/// The asymmetry that earns the ceremony: a tool Rift declines to recognise
/// costs a saving, while an argument read as something it is not — a page
/// number taken for a line number, an end line taken for a count — records
/// that a context holds text it never received, and every decision downstream
/// inherits that in silence.
#[cfg(test)]
mod pinned_tool_field_surface {
    use rift_agent::tool_fields::{pinned_fact, pinned_tool_field_rows, tool_field_rows};
    use rift_core::AgentId;

    use super::{
        AgentAdapter, ClaudeCodeAdapter, EditKind, HiKind, ReadSpan, SearchKind,
        FILE_MUTATING_TOOLS,
    };

    /// The committed pin. `include_str!` rather than a runtime read so a
    /// missing or renamed fixture is a compile error, not a skipped test.
    const FIXTURE: &str = include_str!("../fixtures/claude-code-tool-field-surface.md");

    /// A provenance field of one pinned artifact (`key: value`, indented).
    ///
    /// `nth` selects the artifact: a file with several pins has several
    /// digests, and a reader of one number must be able to say which source it
    /// belongs to.
    fn provenance(field: &str, nth: usize) -> &'static str {
        let needle = format!("{field}:");
        FIXTURE
            .lines()
            .filter_map(|line| line.trim().strip_prefix(&needle))
            .map(str::trim)
            .nth(nth)
            .unwrap_or_else(|| panic!("the pin states no {field} #{nth}"))
    }

    /// Both artifacts say where they came from, precisely enough that a
    /// reviewer reproduces each digest offline from the bytes alone. The
    /// reference page has no tag to pin, so its digest and its retrieval date
    /// are the pin; the type file has both.
    #[test]
    fn the_pin_states_a_provenance_a_reader_can_recheck() {
        assert_eq!(
            provenance("repository", 0),
            "https://docs.claude.com/en/docs/claude-code/tools-reference"
        );
        assert_eq!(
            provenance("path", 0),
            "/en/docs/claude-code/tools-reference.md"
        );
        let digest = provenance("sha256", 0);
        assert_eq!(digest.len(), 64, "a page digest is 64 characters");
        assert!(
            digest.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "a digest must be lowercase hex"
        );
        assert_eq!(provenance("tag", 0), "v0.2.152");
        assert_eq!(provenance("blob-sha", 0).len(), 40);
        for nth in 0..2 {
            let from = provenance("retrieved-from", nth);
            assert!(
                from.contains(provenance("path", nth).trim_start_matches('/')),
                "artifact #{nth}: {from} does not name the path it pins"
            );
        }
    }

    /// **The pin and the code table are one set.** Every tool this adapter
    /// reads anything from is a row the artifact pins, and every row the
    /// artifact pins is in the table — so neither a new field nor a new
    /// excerpt arrives alone.
    #[test]
    fn the_tool_field_table_equals_the_pinned_fixture() {
        let pinned_rows = pinned_tool_field_rows(FIXTURE);
        assert!(!pinned_rows.is_empty(), "the pin states no tool-field rows");
        assert_eq!(
            pinned_rows,
            tool_field_rows(&AgentId::CLAUDE_CODE),
            "the pinned tool fields and the code table disagree; re-pin the \
             surface or drop the row, never edit one to match the other"
        );
    }

    /// The value the pin gives for one of this family's argument names.
    fn pinned(tool: &str, key: &str) -> &'static str {
        pinned_fact(&AgentId::CLAUDE_CODE, tool, key)
            .unwrap_or_else(|| panic!("the table states no {key} for {tool}"))
    }

    /// A `PreToolUse` payload for one tool, with the arguments given.
    fn call(tool: &str, args: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"tool_name": tool, "tool_input": args})
    }

    /// A read is read through the argument names the pin carries, and the
    /// range it yields is the one the pinned semantics allow: the offset is
    /// the first line and the limit is a count, so the last line the tool can
    /// have delivered is `offset + limit - 1`.
    #[test]
    fn read_request_reads_the_pinned_fields() {
        let payload = call(
            "Read",
            serde_json::json!({
                pinned("Read", "path"): "/repo/src/lib.rs",
                pinned("Read", "lo"): 555,
                pinned("Read", "hi"): 350,
            }),
        );
        let request = ClaudeCodeAdapter
            .read_request("PreToolUse", &payload)
            .expect("a Read with a path and a range is read-shaped");
        assert_eq!(request.path, "/repo/src/lib.rs");
        assert_eq!(request.span, ReadSpan::lines(555, HiKind::Explicit(904)));
        assert_eq!(request.conservative_lines(), Some((555, 904)));

        // The semantics the arithmetic above rests on, stated by the pin
        // rather than by this test.
        assert_eq!(pinned("Read", "lo_base"), "1");
        assert_eq!(pinned("Read", "hi_kind"), "count");
    }

    /// A read with no cap reaches the end of the file, on the pin's authority
    /// and not on an assumption: the client hands over every line and says so
    /// when a token cap stopped it short. The adapter states the end rather
    /// than a line number because it never opens the file.
    #[test]
    fn a_read_with_no_cap_reaches_the_end_of_the_file() {
        let payload = call(
            "Read",
            serde_json::json!({pinned("Read", "path"): "/repo/src/lib.rs"}),
        );
        let request = ClaudeCodeAdapter
            .read_request("PreToolUse", &payload)
            .expect("a bare Read is still a read");
        assert_eq!(
            request.span,
            ReadSpan::lines(1, HiKind::DefaultWindow(HiKind::END_OF_FILE))
        );

        // The semantics the line above rests on, stated by the pin rather than
        // by this test.
        assert_eq!(pinned("Read", "default_window"), "to-end-of-file");
    }

    /// An `offset` with no `limit` is the same delivery starting later: the
    /// client reads from that line to the end, so the range runs from the
    /// offset rather than from line one.
    #[test]
    fn a_read_with_an_offset_and_no_cap_starts_there_and_runs_on() {
        let payload = call(
            "Read",
            serde_json::json!({
                pinned("Read", "path"): "/repo/src/lib.rs",
                pinned("Read", "lo"): 736,
            }),
        );
        let request = ClaudeCodeAdapter
            .read_request("PreToolUse", &payload)
            .expect("an offset with no limit is still a read");
        assert_eq!(
            request.span,
            ReadSpan::lines(736, HiKind::DefaultWindow(HiKind::END_OF_FILE))
        );
        assert_eq!(pinned("Read", "lo_base"), "1");
    }

    /// **Pages of a document are not lines of text.** A page selector
    /// addresses pages, a notebook read returns cells, and an argument added
    /// after this pin was taken is one whose effect on the range nobody has
    /// checked — so such a call leaves the read shape altogether rather than
    /// being read as a range that happens to ignore it.
    #[test]
    fn a_pdf_read_with_pages_is_not_read_shaped() {
        let with_pages = call(
            "Read",
            serde_json::json!({
                pinned("Read", "path"): "/repo/docs/spec.pdf",
                "pages": "1-5",
            }),
        );
        assert!(ClaudeCodeAdapter
            .read_request("PreToolUse", &with_pages)
            .is_none());

        let with_an_unpinned_argument = call(
            "Read",
            serde_json::json!({
                pinned("Read", "path"): "/repo/src/lib.rs",
                pinned("Read", "hi"): 50,
                "cells": "3-4",
            }),
        );
        assert!(ClaudeCodeAdapter
            .read_request("PreToolUse", &with_an_unpinned_argument)
            .is_none());

        // A read that delivers no lines has no range to record either.
        let zero_lines = call(
            "Read",
            serde_json::json!({
                pinned("Read", "path"): "/repo/src/lib.rs",
                pinned("Read", "hi"): 0,
            }),
        );
        assert!(ClaudeCodeAdapter
            .read_request("PreToolUse", &zero_lines)
            .is_none());
    }

    /// The search shapes stay apart, because what may be done about them
    /// differs: a name search has an answer that fits in a sentence, a content
    /// search never does. This family has no listing tool — it lists with a
    /// shell command, which is pinned and recognised nowhere — so its rows
    /// carry two of the three.
    #[test]
    fn search_request_distinguishes_glob_grep_and_listing() {
        let glob = call(
            "Glob",
            serde_json::json!({pinned("Glob", "pattern"): "**/*.rs"}),
        );
        assert_eq!(
            ClaudeCodeAdapter
                .search_request("PreToolUse", &glob)
                .expect("a Glob is a search")
                .kind,
            SearchKind::Glob {
                pattern: "**/*.rs".to_owned()
            }
        );

        let uncapped = call("Grep", serde_json::json!({"pattern": "respond"}));
        assert_eq!(
            ClaudeCodeAdapter
                .search_request("PreToolUse", &uncapped)
                .expect("a Grep is a search")
                .kind,
            SearchKind::Grep {
                has_head_limit: false
            }
        );

        // A caller that set the cap chose it — including the explicit "no
        // bound" — and a choice already made is not one to make again.
        for chosen in [serde_json::json!(40), serde_json::json!(0)] {
            let capped = call(
                "Grep",
                serde_json::json!({"pattern": "respond", pinned("Grep", "head_limit"): chosen}),
            );
            assert_eq!(
                ClaudeCodeAdapter
                    .search_request("PreToolUse", &capped)
                    .expect("a Grep is a search")
                    .kind,
                SearchKind::Grep {
                    has_head_limit: true
                }
            );
        }

        // A shell listing is a listing; a shell call is never an edit.
        let shell = call(
            "Bash",
            serde_json::json!({pinned("Bash", "command"): "ls /repo/src"}),
        );
        assert_eq!(
            ClaudeCodeAdapter
                .search_request("PreToolUse", &shell)
                .expect("a listing is a search of names")
                .kind,
            SearchKind::Listing {
                dir: "/repo/src".to_owned(),
                pattern: None
            }
        );
        assert!(ClaudeCodeAdapter
            .read_request("PreToolUse", &shell)
            .is_none());
        assert!(ClaudeCodeAdapter
            .edit_request("PreToolUse", &shell)
            .is_none());
    }

    /// **A command line is a read only under the published grammar.**
    ///
    /// The list is this family's own answer to which shell commands count as
    /// having read a file, so recognising an exact match against it is not a
    /// reading of shell in general — which is why everything the list does not
    /// spell stays untouched, whatever it plainly does.
    #[test]
    fn a_shell_read_is_recognised_only_under_the_pinned_grammar() {
        let command = pinned("Bash", "command");
        let read = call(
            "Bash",
            serde_json::json!({command: "head -n 30 /repo/src/lib.rs"}),
        );
        let request = ClaudeCodeAdapter
            .read_request("PreToolUse", &read)
            .expect("a counted head is a read of those lines");
        assert_eq!(request.path, "/repo/src/lib.rs");
        assert_eq!(request.span, ReadSpan::lines(1, HiKind::Explicit(30)));
        assert!(
            ClaudeCodeAdapter
                .search_request("PreToolUse", &read)
                .is_none(),
            "a read of a file is never a question about names"
        );

        for line in ["grep -r needle /repo/src", "cargo test", "cat $FILE"] {
            let payload = call("Bash", serde_json::json!({command: line}));
            assert!(
                ClaudeCodeAdapter
                    .read_request("PreToolUse", &payload)
                    .is_none(),
                "{line}"
            );
            assert!(
                ClaudeCodeAdapter
                    .search_request("PreToolUse", &payload)
                    .is_none(),
                "{line}"
            );
        }
    }

    /// **One `Bash` call, every read it performs.**
    ///
    /// The composition rules above the published vocabulary are what an agent
    /// on this family actually writes — a `cd` into a crate, a trailing
    /// `| head`, two `cat`s joined by `&&` — and each of those is one call
    /// carrying several facts. Answering with the first alone would record a
    /// fraction of the call and leave the rest outside the journal altogether;
    /// answering with none, as this adapter did before, left all of it there.
    #[test]
    fn a_shell_call_answers_with_every_read_the_line_performs() {
        let command = pinned("Bash", "command");
        let reads = |line: &str| {
            ClaudeCodeAdapter
                .read_requests(
                    "PreToolUse",
                    &call("Bash", serde_json::json!({command: line})),
                )
                .into_iter()
                .map(|request| (request.path, request.span))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            reads("cd /repo/src && cat lib.rs && sed -n '1,9p' main.rs"),
            vec![
                (
                    "/repo/src/lib.rs".to_owned(),
                    ReadSpan::lines(1, HiKind::DefaultWindow(HiKind::END_OF_FILE))
                ),
                (
                    "/repo/src/main.rs".to_owned(),
                    ReadSpan::lines(1, HiKind::Explicit(9))
                ),
            ]
        );
        assert_eq!(
            reads("cat /repo/src/lib.rs /repo/src/main.rs")
                .iter()
                .map(|(path, _)| path.clone())
                .collect::<Vec<_>>(),
            vec![
                "/repo/src/lib.rs".to_owned(),
                "/repo/src/main.rs".to_owned()
            ]
        );
        assert_eq!(
            reads("head -n 30 /repo/src/lib.rs | tail -n 5"),
            vec![(
                "/repo/src/lib.rs".to_owned(),
                ReadSpan::lines(26, HiKind::Explicit(30))
            )]
        );
        // The captioned chain this family's agents write: the labels between
        // the reads are not reads, and they cost the line nothing.
        assert_eq!(
            reads(
                "cd /repo/src && echo \"=== lib ===\" && sed -n '1,80p' lib.rs && echo && \
                 sed -n '100,140p' main.rs"
            ),
            vec![
                (
                    "/repo/src/lib.rs".to_owned(),
                    ReadSpan::lines(1, HiKind::Explicit(80))
                ),
                (
                    "/repo/src/main.rs".to_owned(),
                    ReadSpan::lines(100, HiKind::Explicit(140))
                ),
            ]
        );
        assert!(reads("echo \"=== lib ===\"").is_empty());
        // A structured read is still one read, and a line the grammar does not
        // spell is still none.
        assert_eq!(
            ClaudeCodeAdapter
                .read_requests(
                    "PreToolUse",
                    &call(
                        "Read",
                        serde_json::json!({pinned("Read", "path"): "/repo/src/lib.rs"})
                    )
                )
                .len(),
            1
        );
        assert!(reads("cargo test").is_empty());
        assert!(reads("ls /repo/src").is_empty());
    }

    /// An edit is read as a site only where one site follows from the payload.
    #[test]
    fn edit_request_reads_the_pinned_fields() {
        let write = call(
            "Write",
            serde_json::json!({
                pinned("Write", "path"): "/repo/src/new.rs",
                pinned("Write", "content"): "fn main() {}\n",
            }),
        );
        let request = ClaudeCodeAdapter
            .edit_request("PreToolUse", &write)
            .expect("a Write changes a file");
        assert_eq!(request.path, "/repo/src/new.rs");
        assert_eq!(
            request.kind,
            EditKind::Write {
                content_sha256: rift_agent::content_sha256(b"fn main() {}\n")
            }
        );

        let edit = call(
            "Edit",
            serde_json::json!({
                pinned("Edit", "path"): "/repo/src/lib.rs",
                pinned("Edit", "old"): "one\ntwo",
                pinned("Edit", "new"): "one\ntwo\nthree",
                pinned("Edit", "replace_all"): true,
            }),
        );
        assert_eq!(
            ClaudeCodeAdapter
                .edit_request("PreToolUse", &edit)
                .expect("an Edit changes a file")
                .kind,
            EditKind::Edit {
                new_string: "one\ntwo\nthree".to_owned(),
                old_string_lines: 2,
                replace_all: true,
            }
        );

        for tool in ["MultiEdit", "NotebookEdit", "mcp__acp__Edit"] {
            let other = call(
                tool,
                serde_json::json!({pinned(tool, "path"): "/repo/src/lib.rs", "edits": []}),
            );
            assert_eq!(
                ClaudeCodeAdapter
                    .edit_request("PreToolUse", &other)
                    .unwrap_or_else(|| panic!("{tool} changes a file"))
                    .kind,
                EditKind::Other,
                "{tool} carries no single before-and-after pair"
            );
        }
    }

    /// Every tool the pin calls an edit is one this adapter already treats as
    /// changing a file, and the reverse. Two lists of the same tools, kept
    /// together by nothing but attention, is how one of them goes stale.
    #[test]
    fn the_edit_shaped_rows_are_the_file_mutating_tools() {
        let mut pinned_edits: Vec<&str> = pinned_tool_field_rows(FIXTURE)
            .iter()
            .filter(|row| row.contains(" shape=edit"))
            .map(|row| {
                let tool = row
                    .strip_prefix("tool=")
                    .and_then(|rest| rest.split(' ').next())
                    .unwrap_or_else(|| panic!("malformed row: {row}"));
                *FILE_MUTATING_TOOLS
                    .iter()
                    .find(|known| **known == tool)
                    .unwrap_or_else(|| {
                        panic!("the pin calls {tool} an edit; this adapter does not")
                    })
            })
            .collect();
        pinned_edits.sort_unstable();
        let mut known = FILE_MUTATING_TOOLS.to_vec();
        known.sort_unstable();
        assert_eq!(pinned_edits, known);
    }
}
