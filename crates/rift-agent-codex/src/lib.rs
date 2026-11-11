//! First-party Rift adapter for **OpenAI Codex / OpenAI CLI**.
//!
//! Native verbs are pinned here with the adapter; the exact lifecycle names are
//! version-dependent (`(verify)` against the current Codex hook docs) and a drift
//! is caught by the conformance fixtures, not silent capture loss.
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

mod patch_regions;
pub mod rollout;

use rift_agent::{
    event, shell_read, AdapterError, AgentAdapter, CompatNotes, EditRequest, HookFacts, ModelFact,
    ModelSource, ReadRequest, SearchRequest, TokenUsage,
};
use rift_core::{AgentId, CanonicalEvent, EventKind, Trust};
use std::path::Path;

/// The Codex adapter.
pub struct CodexAdapter;

/// The Codex tools whose use means a file on disk changed.
///
/// `apply_patch` is Codex's **canonical** mutation tool; `Write` and `Edit`
/// are matcher aliases Codex registers so Claude-style hook configs keep
/// working. The canonical name was the one missing, so the tool Codex
/// actually uses to edit files produced a `ToolCall` with no `FileEdit`.
///
/// `MultiEdit` is not a Codex tool; it is kept because carrying a name Codex
/// never sends costs nothing, while removing one it might send loses capture.
pub const FILE_MUTATING_TOOLS: &[&str] = &["apply_patch", "Write", "Edit", "MultiEdit"];

/// The argument a patch document travels in, spelled once.
///
/// One name for two canonical tools: the shell command line and the patch
/// arrive under the same key, which the pinned source shows from both the
/// handler's side and the hook runtime's.
const PATCH_FIELD: &str = "command";

/// The tool name a shell command line arrives under.
///
/// It is the historical Claude-style spelling rather than the handler's own
/// name: the pinned source shows this family building its hook payload with
/// `HookToolName::bash()`, so a read, a listing and a grep all arrive here.
const SHELL_TOOL: &str = "Bash";

/// The shell argument naming the directory one call ran in, as upstream's
/// own exec arguments spell it.
const WORKDIR_FIELD: &str = "workdir";

/// The markers a patch document opens a file section with, the one that
/// renames a file, and the three that bound an update's chunks.
///
/// The grammar is pinned in `fixtures/codex-tool-field-surface.md` as its
/// author states it: a header is a fixed prefix at the start of a line, and
/// one header names one file. A hunk body is read only for each chunk's new
/// side, never for a line number: the patch states none, and turning its
/// lines into numbers would be arithmetic nobody published.
mod patch {
    /// A section that creates a file.
    pub(crate) const ADD: &str = "*** Add File: ";
    /// A section that changes one.
    pub(crate) const UPDATE: &str = "*** Update File: ";
    /// A section that removes one.
    pub(crate) const DELETE: &str = "*** Delete File: ";
    /// A rename, which makes one section name two paths.
    pub(crate) const MOVE: &str = "*** Move to: ";
    /// A line that opens a chunk of an update, with or without a header.
    pub(crate) const CHUNK: &str = "@@";
    /// The line that pins the chunk before it to the end of the file.
    pub(crate) const END_OF_FILE: &str = "*** End of File";
    /// The line that closes the document.
    pub(crate) const END_PATCH: &str = "*** End Patch";
}

/// The single path a patch document names, or `None` when it names anything
/// other than exactly one.
///
/// One request carries one path, so a patch that touches two files, renames
/// one, or names none is recorded as nothing at all. That is the safe
/// direction: a patch nobody recorded costs a saving, while picking one path
/// out of several attributes a change to a file that may not have changed.
fn single_patched_path(patch_text: &str) -> Option<String> {
    let mut named: Option<&str> = None;
    for line in patch_text.lines() {
        let line = line.trim_start();
        if line.starts_with(patch::MOVE) || line.starts_with(patch::DELETE) {
            return None;
        }
        for marker in [patch::ADD, patch::UPDATE] {
            if let Some(path) = line.strip_prefix(marker) {
                if named.is_some() {
                    return None;
                }
                named = Some(path.trim());
            }
        }
    }
    named.filter(|path| !path.is_empty()).map(str::to_owned)
}

impl CodexAdapter {
    /// The agent id this adapter serves.
    pub const AGENT_ID: AgentId = AgentId::CODEX;

    fn tool_name(native: &serde_json::Value) -> Option<&str> {
        native.get("tool_name").and_then(serde_json::Value::as_str)
    }

    /// What a shell call was, when the command line is one the pinned grammar
    /// spells and nothing at all otherwise.
    ///
    /// This family has no structured read tool and no search tool, so the
    /// command line is the only place a read or a listing can be recognised —
    /// and the grammar recognised is another vendor's published one, matched
    /// exactly, never a reading of shell in general.
    fn shell_call(native: &serde_json::Value) -> Option<shell_read::ShellCall> {
        if Self::tool_name(native)? != SHELL_TOOL {
            return None;
        }
        let command = native
            .get("tool_input")?
            .get(PATCH_FIELD)
            .and_then(serde_json::Value::as_str)?;
        shell_read::recognise(command)
    }

    /// Where a shell call's relative names resolve from.
    ///
    /// The session's `cwd` is where the agent stands, not where a command ran:
    /// a call can carry its own `workdir`, and a relative name read there is a
    /// different file from the same name under the session. The pinned
    /// upstream payload does not promise the field, so its absence keeps the
    /// session reading, while a field present in a shape this adapter cannot
    /// place makes the call's directory unknown rather than guessed.
    fn execution_dir(native: &serde_json::Value) -> ExecutionDir {
        let Some(field) = native.get("tool_input").and_then(|i| i.get(WORKDIR_FIELD)) else {
            return ExecutionDir::Session;
        };
        let Some(dir) = field.as_str().filter(|dir| !dir.is_empty()) else {
            return ExecutionDir::Unknown;
        };
        if Path::new(dir).is_absolute() {
            return ExecutionDir::Named(dir.to_owned());
        }
        match rift_agent::payload_cwd(native) {
            Some(cwd) if Path::new(&cwd.path).is_absolute() => {
                ExecutionDir::Named(joined(&cwd.path, dir))
            }
            _ => ExecutionDir::Unknown,
        }
    }

    /// A recognised name, spelled from where the call ran; `None` when that
    /// place is unknown, so the call passes through with no read claimed.
    fn placed(native: &serde_json::Value, path: String) -> Option<String> {
        if Path::new(&path).is_absolute() {
            return Some(path);
        }
        match Self::execution_dir(native) {
            ExecutionDir::Session => Some(path),
            ExecutionDir::Named(dir) => Some(joined(&dir, &path)),
            ExecutionDir::Unknown => None,
        }
    }
}

/// The answer [`CodexAdapter::execution_dir`] gives.
enum ExecutionDir {
    /// No override: names resolve against the session directory downstream.
    Session,
    /// The absolute directory the call named for itself.
    Named(String),
    /// An override that names no place this adapter can prove.
    Unknown,
}

/// A name under a directory, joined with `/` as the read ledger keys it on
/// every platform.
fn joined(dir: &str, name: &str) -> String {
    format!("{}/{name}", dir.trim_end_matches(['/', '\\']))
}

impl AgentAdapter for CodexAdapter {
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
            "SessionEnd" => vec![event(SessionEnd, GitFact)],
            "UserPromptSubmit" => vec![event(PromptSubmit, TranscriptUntrusted)],
            "PreToolUse" => {
                let mut events = vec![event(ToolCall, CodeFact)];
                if FILE_MUTATING_TOOLS.contains(&Self::tool_name(native).unwrap_or_default()) {
                    events.push(event(FileEdit, CodeFact));
                }
                events
            }
            "PostToolUse" => vec![event(ToolResult, ToolOutputUntrusted)],
            "Stop" => vec![event(TurnEnd, GitFact)],
            "PreCompact" | "PostCompact" => vec![event(SessionCompaction, GitFact)],
            "SubagentStart" => vec![event(SubagentStart, GitFact)],
            "SubagentStop" => vec![event(SubagentEnd, GitFact)],
            other => return Err(AdapterError::UnknownVerb(other.to_owned())),
        })
    }

    /// **Ten, measured from `fixtures/codex-hook-surface.rs`**, never counted
    /// by hand.
    ///
    /// `SessionEnd` joined this table when Codex's own published schema was
    /// pinned: `SessionEndCommandInput` is one of the eleven hook command
    /// inputs upstream generates, so a Codex session had an end Rift declined
    /// to register a hook for and therefore never recorded. The table was
    /// previously graded against the Go wrapper's `HookNames()`, which knew
    /// five — see the module-level `pinned_surface` docs.
    ///
    /// `PermissionRequest` is published and deliberately **not** declared: see
    /// the `codex.permission_request_undeclared` row of
    /// `crates/rift-shadow-hooks/tests/adapter_sweep.rs`.
    fn verbs(&self) -> &'static [&'static str] {
        &[
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "PreCompact",
            "PostCompact",
            "SubagentStart",
            "SubagentStop",
            "Stop",
            "SessionEnd",
        ]
    }

    /// **The one family that bounds its context channel by a number the hook's
    /// author writes**, so the answer is read off the same capability table the
    /// response shape is read off — never restated here.
    ///
    /// The key it is written under is `additionalContextLimit`, and the pinned
    /// artifact (E, `config/src/hook_config.rs`) settles where it lives: on the
    /// handler's own configuration entry, beside the command, not in any
    /// payload or response. So this is the value the installer writes once, and
    /// the value the hook fits its boundary text to thereafter.
    fn declared_context_limit(&self, verb: &str) -> Option<u32> {
        rift_agent::respond::declared_context_limit(&Self::AGENT_ID, verb)
    }

    fn compat_notes(&self) -> CompatNotes {
        CompatNotes {
            agent: Self::AGENT_ID,
            // "no explicit session-end event" was here until 2026-08-24, and it
            // was false of the agent: Codex publishes `SessionEnd`. The note
            // outlived the fact because it was copied from a wrapper that never
            // registered the hook.
            lifecycle_gaps: &["token usage is optional and client-version dependent"],
            surfaces_token_usage: true,
            best_effort: false,
            note: "Current PascalCase Codex hooks; runtime model is read from the common payload.",
        }
    }

    fn facts(&self, _verb: &str, native: &serde_json::Value) -> HookFacts {
        let model = native
            .get("model")
            .and_then(serde_json::Value::as_str)
            .filter(|m| !m.is_empty())
            .map(|model| ModelFact {
                model: model.to_owned(),
                source: ModelSource::RuntimeHook,
            });
        let tool_name = Self::tool_name(native).map(str::to_owned);
        let read_path = tool_name
            .as_deref()
            .filter(|name| name.eq_ignore_ascii_case("read"))
            .and_then(|_| native.get("tool_input"))
            .and_then(|v| v.get("file_path"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        HookFacts {
            model,
            tool_name,
            // `agent_id` first, and the pin is why: Codex's own
            // `SubagentStartCommandInput`/`SubagentStopCommandInput` name the
            // child `agent_id` (and carry it on every turn-scoped input when
            // the hook fires from inside a subagent). This adapter read
            // `subagent_id`, a name nothing in the published schema uses, so a
            // Codex subagent's work was attributed flat to the parent with no
            // nesting at all. `subagent_id` is kept as a fallback because
            // carrying a name the agent may not send costs nothing while
            // dropping one it might send loses capture.
            child_session: ["agent_id", "subagent_id"]
                .into_iter()
                .find_map(|key| native.get(key).and_then(serde_json::Value::as_str))
                .map(str::to_owned),
            read_path,
            // Codex puts the agent's working directory on every payload as
            // `cwd`, the same spelling Claude Code uses.
            cwd: rift_agent::payload_cwd(native),
        }
    }

    /// A read is a command line, or it is nothing.
    fn read_request(&self, _verb: &str, native: &serde_json::Value) -> Option<ReadRequest> {
        let request = Self::shell_call(native)?.read_request()?;
        let path = Self::placed(native, request.path)?;
        Some(ReadRequest { path, ..request })
    }

    /// …and one command line is as many reads as it spells. This family has
    /// no read tool at all, so a chain of `cat`s under one `cd` is not a
    /// convenience here but the only way several files reach the window.
    fn read_requests(&self, _verb: &str, native: &serde_json::Value) -> Vec<ReadRequest> {
        let requests = Self::shell_call(native)
            .map(|call| call.read_requests())
            .unwrap_or_default();
        requests
            .into_iter()
            .filter_map(|request| {
                let path = Self::placed(native, request.path)?;
                Some(ReadRequest { path, ..request })
            })
            .collect()
    }

    /// A listing is a command line too; a content search stays a content
    /// search, here as everywhere.
    fn search_request(&self, _verb: &str, native: &serde_json::Value) -> Option<SearchRequest> {
        Self::shell_call(native)?.search_request()
    }

    /// The one file a content search named — this family reads files, and
    /// searches inside them, through the same shell tool.
    fn searched_file(&self, _verb: &str, native: &serde_json::Value) -> Option<String> {
        let path = Self::shell_call(native)?.searched_file()?.to_owned();
        Self::placed(native, path)
    }

    /// A patch document changed a file: which file, and the new side of each
    /// chunk it updated.
    ///
    /// The payload carries the whole patch under one argument. A single file
    /// header names the path. An update's chunks become one region each, to be
    /// located in the bytes the patch left behind; a patch whose chunks this
    /// reading cannot stand behind is a change with no derivable site.
    ///
    /// The two names that look like edits — the ones a Claude-style hook
    /// matcher would use — never arrive as a tool name on this family: they
    /// select a handler and are dropped before the payload is written. They
    /// stay in the file-mutating list, where carrying a name that never
    /// arrives costs nothing, and are deliberately not read here.
    fn edit_request(&self, _verb: &str, native: &serde_json::Value) -> Option<EditRequest> {
        if Self::tool_name(native)? != "apply_patch" {
            return None;
        }
        let patch_text = native
            .get("tool_input")?
            .get(PATCH_FIELD)
            .and_then(serde_json::Value::as_str)?;
        Some(EditRequest {
            path: single_patched_path(patch_text)?,
            kind: patch_regions::update_kind(patch_text),
        })
    }

    fn usage(&self, verb: &str, native: &serde_json::Value) -> Option<TokenUsage> {
        if !matches!(verb, "PostToolUse" | "Stop") {
            return None;
        }
        let u = TokenUsage {
            input: rift_agent::read_count(native, &["usage", "input_tokens"]),
            output: rift_agent::read_count(native, &["usage", "output_tokens"]),
            cache_read: rift_agent::read_count(native, &["usage", "cached_input_tokens"]),
            cache_write: None,
        };
        u.is_present().then_some(u)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_agent::conformance::{check, Case};
    use rift_core::EventKind::{
        PromptSubmit, SessionCompaction, SessionEnd, SessionStart, SubagentEnd, SubagentStart,
        ToolCall, ToolResult, TurnEnd,
    };

    #[test]
    fn conformance_kit_passes() {
        let cases = vec![
            Case::new("SessionStart", serde_json::json!({}), &[SessionStart]),
            Case::new(
                "SessionEnd",
                serde_json::json!({"reason": "other"}),
                &[SessionEnd],
            ),
            Case::new("UserPromptSubmit", serde_json::json!({}), &[PromptSubmit]),
            Case::new(
                "PreToolUse",
                serde_json::json!({"tool_name":"Bash"}),
                &[ToolCall],
            ),
            Case::new("PostToolUse", serde_json::json!({}), &[ToolResult]),
            Case::new("PreCompact", serde_json::json!({}), &[SessionCompaction]),
            Case::new("PostCompact", serde_json::json!({}), &[SessionCompaction]),
            Case::new("SubagentStart", serde_json::json!({}), &[SubagentStart]),
            Case::new("SubagentStop", serde_json::json!({}), &[SubagentEnd]),
            Case::new("Stop", serde_json::json!({}), &[TurnEnd]),
        ];
        assert!(CodexAdapter
            .verbs()
            .iter()
            .all(|verb| cases.iter().any(|case| case.verb == *verb)));
        check(&CodexAdapter, &cases).unwrap();
    }

    /// `apply_patch` is the tool Codex actually edits files with; it
    /// was absent from the matcher, so Codex's canonical mutation produced a
    /// bare `ToolCall` and the working-tree change went unrecorded.
    #[test]
    fn the_canonical_codex_mutation_tool_fans_out_to_a_file_edit() {
        for tool in FILE_MUTATING_TOOLS {
            let kinds: Vec<_> = CodexAdapter
                .map("PreToolUse", &serde_json::json!({"tool_name": tool}))
                .unwrap()
                .iter()
                .map(|e| e.kind)
                .collect();
            assert_eq!(
                kinds,
                vec![ToolCall, rift_core::EventKind::FileEdit],
                "{tool} must imply an edit"
            );
        }
        assert!(FILE_MUTATING_TOOLS.contains(&"apply_patch"));
        let shell: Vec<_> = CodexAdapter
            .map("PreToolUse", &serde_json::json!({"tool_name": "shell"}))
            .unwrap()
            .iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(shell, vec![ToolCall], "a shell call is not an edit");
    }

    #[test]
    fn usage_extracted_when_present() {
        let v = serde_json::json!({"usage": {"input_tokens": 5, "cached_input_tokens": 2}});
        let u = CodexAdapter.usage("Stop", &v).unwrap();
        assert_eq!(u.input, Some(5));
        assert_eq!(u.cache_read, Some(2));
        assert!(CodexAdapter.usage("SessionStart", &v).is_none());
    }

    #[test]
    fn runtime_model_is_normalized() {
        let facts = CodexAdapter.facts("SessionStart", &serde_json::json!({"model":"gpt-5.4"}));
        assert_eq!(facts.model.unwrap().model, "gpt-5.4");
    }
}

/// **Codex's own published hook surface, pinned into this tree** (an owner
/// decision of 2026-08-24).
///
/// The artifact is `fixtures/codex-hook-surface.rs`: repository, tag, path,
/// git blob sha and a verbatim excerpt of
/// `openai/codex@rust-v0.149.1:codex-rs/hooks/src/schema.rs`. Rift holds
/// a supported agent's own surface authoritative for its verb table and admits
/// that authority **only** through a committed artifact, so this module is the
/// thing that turns a downloaded file into evidence: it parses the fixture and
/// pins what the parse yields, which means an edit to the fixture that changes
/// the answer reddens here rather than quietly re-grading a table.
///
/// **Why it earns its place, in one measurement.** The sweep row
/// `codex.verbs_beyond_the_surface` recorded *"Rift declares nine verbs; Codex
/// publishes five"*, cited to a third party's Go wrapper and its `HookNames()`
/// rather than to OpenAI. That sentence is true of that wrapper and **false of
/// Codex**: the pin below enumerates eleven published hook events and every one
/// of Rift's nine is among them. A table graded against a third party's view of
/// an agent is the defect this artifact exists to stop.
#[cfg(test)]
mod pinned_surface {
    use super::{AgentAdapter, CodexAdapter};

    /// The committed excerpt. `include_str!` rather than a runtime read so a
    /// missing or renamed fixture is a compile error, not a skipped test.
    const FIXTURE: &str = include_str!("../fixtures/codex-hook-surface.rs");

    /// A provenance field from the fixture's header block (`//   key: value`).
    fn provenance(field: &str) -> &'static str {
        let needle = format!("{field}:");
        let found = FIXTURE
            .lines()
            .filter_map(|line| line.trim().strip_prefix("//"))
            .find_map(|rest| rest.trim().strip_prefix(&needle));
        match found {
            Some(value) => value.trim(),
            None => panic!("the pin states no {field}"),
        }
    }

    /// Every hook event the pin publishes, in the excerpt's own order.
    ///
    /// The enumeration is closed because upstream gives each published event
    /// exactly one `fn <event>_hook_event_name_schema` whose body is a
    /// `string_const_schema("…")`. Keying on the `_hook_event_name_schema`
    /// suffix — rather than on every `string_const_schema` call — is what
    /// excludes `session_end_reason_schema`'s `"other"`, which is a compaction
    /// *reason* and not an event. That near-miss is kept inside the excerpt on
    /// purpose so the exclusion is visible to a reader instead of lucky.
    fn published_verbs() -> Vec<&'static str> {
        let mut out = Vec::new();
        let mut armed = false;
        for line in FIXTURE.lines() {
            let line = line.trim();
            if line.starts_with("fn ") {
                armed = line.contains("_hook_event_name_schema(");
                continue;
            }
            if !armed {
                continue;
            }
            if let Some(rest) = line.strip_prefix("string_const_schema(\"") {
                if let Some(name) = rest.split('"').next() {
                    out.push(name);
                    armed = false;
                }
            }
        }
        out
    }

    /// The pin says where it came from, and says it precisely enough that a
    /// reviewer can reproduce the sha offline from the bytes alone.
    #[test]
    fn the_pin_states_a_provenance_a_reader_can_recheck() {
        assert_eq!(provenance("repository"), "https://github.com/openai/codex");
        assert_eq!(provenance("tag"), "rust-v0.149.1");
        assert_eq!(provenance("path"), "codex-rs/hooks/src/schema.rs");
        let sha = provenance("blob-sha");
        assert_eq!(sha, "3847cc91818a9a141c066e1fc6d734adb60f9bde");
        assert_eq!(sha.len(), 40, "a git blob sha is 40 hex characters");
        assert!(
            sha.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "blob sha must be lowercase hex"
        );
        // The retrieval URL must actually name the pinned tag and path, or the
        // two halves of the provenance can drift apart without anything moving.
        let from = provenance("retrieved-from");
        assert!(from.contains(provenance("tag")), "{from}");
        assert!(from.contains(provenance("path")), "{from}");
    }

    /// **Eleven, and the list itself is the deliverable** — a count proves
    /// nothing a reviewer can check.
    #[test]
    fn the_pin_is_a_closed_enumeration_of_codexs_published_hook_events() {
        let mut got = published_verbs();
        got.sort_unstable();
        assert_eq!(
            got,
            [
                "PermissionRequest",
                "PostCompact",
                "PostToolUse",
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
        assert!(
            !got.contains(&"other"),
            "`session_end_reason_schema`'s \"other\" is a compaction reason, not \
             an event — the parse must exclude it by name, not by luck"
        );
        // The upstream wire enum is the same set minus SessionEnd, which has no
        // output type. Asserting the relationship keeps the excerpt honest: if
        // a future re-pin drops one of the two regions, this reddens.
        assert!(
            FIXTURE.contains("enum HookEventNameWire"),
            "the excerpt must carry the wire enum as well as the const schemas"
        );
        for verb in &got {
            if *verb == "SessionEnd" {
                continue;
            }
            assert!(
                FIXTURE.contains(&format!("#[serde(rename = \"{verb}\")]")),
                "{verb} is published but absent from the wire enum excerpt"
            );
        }
    }

    /// Direction one: every verb Rift declares for Codex is one Codex
    /// publishes — the claim the sweep row got backwards, now a test rather
    /// than a sentence.
    #[test]
    fn every_verb_rift_declares_for_codex_is_one_codex_publishes() {
        let published = published_verbs();
        for verb in CodexAdapter.verbs() {
            assert!(
                published.contains(verb),
                "the adapter declares {verb}, which {} does not publish at {}",
                provenance("repository"),
                provenance("tag")
            );
        }
    }

    /// Every event Codex publishes that Rift does not register a hook for, and
    /// **the list itself is the deliverable** — not a count, and never an
    /// empty set asserted by hand.
    ///
    /// One name is on it, deliberately. `PermissionRequest` asks a hook whether
    /// a tool may run; its payload carries `session_id`, `turn_id`, `model`,
    /// `permission_mode`, `tool_name` and `tool_input`, and **every one of
    /// those facts is already on `PreToolUse`**, which Rift does register. So
    /// declaring it buys no fact, and costs one of two wrong things: mapped to
    /// `ToolCall` it double-counts a tool that may then be denied (its input
    /// carries no `tool_use_id`, so the dedup key cannot even see the pair);
    /// mapped to nothing it installs a hook on Codex's approval path that
    /// records nothing. Recording the *decision* honestly would need a
    /// canonical permission event, which is a schema change and deliberately
    /// not taken here. Not registering it loses nothing: Codex only calls
    /// hooks named in `.codex/hooks.json`.
    #[test]
    fn the_codex_events_rift_does_not_declare_are_enumerated_with_their_reason() {
        let declared = CodexAdapter.verbs();
        let undeclared: Vec<&str> = published_verbs()
            .into_iter()
            .filter(|verb| !declared.contains(verb))
            .collect();
        assert_eq!(
            undeclared,
            ["PermissionRequest"],
            "a published Codex event is neither declared nor named here; decide \
             it, do not widen the expectation"
        );
    }
}

/// **Codex's hook *output* contract, pinned into this tree.**
///
/// The artifact is `fixtures/codex-hook-output-surface.md`, and it carries five files at one stable tag, each with its own blob sha,
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

    use super::{AgentAdapter, CodexAdapter};

    /// The committed pin. `include_str!` rather than a runtime read so a
    /// missing or renamed fixture is a compile error, not a skipped test.
    const FIXTURE: &str = include_str!("../fixtures/codex-hook-output-surface.md");

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

    /// Five artifacts, one tag, five blob shas — each reproducible offline by
    /// hashing the downloaded bytes, with no GitHub API in the loop.
    #[test]
    fn the_pin_states_a_provenance_a_reader_can_recheck() {
        for nth in 0..5 {
            assert_eq!(
                provenance("repository", nth),
                "https://github.com/openai/codex"
            );
            assert_eq!(
                provenance("tag", nth),
                "rust-v0.152.1",
                "every artifact is pinned at one stable tag"
            );
            assert_digest("blob-sha", nth, 40);
            let from = provenance("retrieved-from", nth);
            assert!(
                from.contains(provenance("path", nth)),
                "artifact #{nth}: {from} does not name the path it pins"
            );
        }
        assert_eq!(
            provenance("path", 0),
            "codex-rs/hooks/schema/generated/pre-tool-use.command.output.schema.json"
        );
        assert_eq!(
            provenance("path", 3),
            "codex-rs/hooks/src/events/pre_tool_use.rs"
        );
        assert_eq!(provenance("path", 4), "codex-rs/config/src/hook_config.rs");
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
            capability_rows(&AgentId::CODEX),
            "the pinned output surface and the capability table disagree; re-pin \
             the surface or drop the row, never edit one to match the other"
        );
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
                CodexAdapter.verbs().contains(&verb),
                "the pin claims a channel on {verb}, which this adapter does not register"
            );
        }
    }
}

/// **Codex's read, edit and search *tool fields*, pinned into this tree.**
///
/// The artifact is `fixtures/codex-tool-field-surface.md`: five files at one
/// stable tag, each with its own blob sha, and a verbatim excerpt for every
/// claim made about them. The short answer they settle is the finding — this
/// family fires its tool hooks under three canonical names, and neither a
/// structured read tool nor a search tool is among them.
#[cfg(test)]
mod pinned_tool_field_surface {
    use rift_agent::tool_fields::{pinned_fact, pinned_tool_field_rows, tool_field_rows};
    use rift_core::AgentId;

    use super::{AgentAdapter, CodexAdapter};
    use rift_agent::EditKind;
    use rift_agent::{HiKind, ReadSpan, SearchKind};

    /// The committed pin. `include_str!` rather than a runtime read so a
    /// missing or renamed fixture is a compile error, not a skipped test.
    const FIXTURE: &str = include_str!("../fixtures/codex-tool-field-surface.md");

    /// A `PreToolUse` payload for one tool, with the arguments given.
    fn call(tool: &str, args: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"tool_name": tool, "tool_input": args})
    }

    /// One tag, five files, five shas a reviewer reproduces from one clone.
    #[test]
    fn the_pin_states_a_provenance_a_reader_can_recheck() {
        let field = |name: &str| -> Vec<&str> {
            let needle = format!("{name}:");
            FIXTURE
                .lines()
                .filter_map(|line| line.trim().strip_prefix(&needle))
                .map(str::trim)
                .collect()
        };
        assert_eq!(field("repository"), ["https://github.com/openai/codex"]);
        assert_eq!(field("tag"), ["rust-v0.152.1"]);
        let shas = field("blob-sha");
        assert_eq!(shas.len(), 5, "one sha per pinned file");
        assert_eq!(field("path").len(), shas.len());
        for sha in shas {
            assert_eq!(sha.len(), 40, "a blob sha is 40 characters");
            assert!(
                sha.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
                "a sha must be lowercase hex"
            );
        }
    }

    /// **The pin and the code table are one set**, in both directions.
    #[test]
    fn the_tool_field_table_equals_the_pinned_fixture() {
        let pinned_rows = pinned_tool_field_rows(FIXTURE);
        assert!(!pinned_rows.is_empty(), "the pin states no tool-field rows");
        assert_eq!(
            pinned_rows,
            tool_field_rows(&AgentId::CODEX),
            "the pinned tool fields and the code table disagree; re-pin the \
             surface or drop the row, never edit one to match the other"
        );
    }

    /// A patch document naming one file is that file changed, with each
    /// chunk's new side carried to be located in the bytes it left.
    #[test]
    fn edit_request_reads_the_pinned_fields() {
        let command = pinned_fact(&AgentId::CODEX, "apply_patch", "command")
            .expect("the table names the argument the patch travels in");
        let payload = call(
            "apply_patch",
            serde_json::json!({command: "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch\n"}),
        );
        let request = CodexAdapter
            .edit_request("PreToolUse", &payload)
            .expect("a patch naming one file is that file changed");
        assert_eq!(request.path, "src/lib.rs");
        assert_eq!(
            request.kind,
            EditKind::Regions {
                regions: vec![rift_agent::Replacement {
                    new_string: "new".to_owned(),
                    old_string_lines: 1,
                    replace_all: false,
                }]
            },
            "one chunk, one region, and no line number taken from the patch"
        );
    }

    /// A patch that names two files, renames one, or names none is recorded as
    /// nothing: one request carries one path, and picking one out of several
    /// would attribute a change to a file that may not have changed.
    #[test]
    fn a_patch_that_names_anything_but_one_file_is_not_edit_shaped() {
        let command = pinned_fact(&AgentId::CODEX, "apply_patch", "command")
            .expect("the table names the argument the patch travels in");
        for patch in [
            "*** Begin Patch\n*** Update File: a.rs\n@@\n-x\n+y\n*** Update File: b.rs\n@@\n-x\n+y\n*** End Patch\n",
            "*** Begin Patch\n*** Update File: a.rs\n*** Move to: b.rs\n@@\n-x\n+y\n*** End Patch\n",
            "*** Begin Patch\n*** Delete File: a.rs\n*** End Patch\n",
            "*** Begin Patch\n*** End Patch\n",
        ] {
            let payload = call("apply_patch", serde_json::json!({command: patch}));
            assert!(
                CodexAdapter.edit_request("PreToolUse", &payload).is_none(),
                "recorded a path from a patch that names anything but one file"
            );
        }
    }

    /// **This family's reads and listings are command lines**, recognised
    /// under the grammar another vendor publishes for its own read-before-edit
    /// check and under nothing else. The two names that look like edits are
    /// matcher aliases that never reach a payload, so nothing is read from
    /// them here either.
    #[test]
    fn a_shell_call_is_read_only_under_the_pinned_grammar() {
        let command = pinned_fact(&AgentId::CODEX, "Bash", "command")
            .expect("the table names the argument a command line travels in");

        let read = call(
            "Bash",
            serde_json::json!({command: "sed -n '1,50p' src/lib.rs"}),
        );
        let request = CodexAdapter
            .read_request("PreToolUse", &read)
            .expect("a printed line range is a read of those lines");
        assert_eq!(request.path, "src/lib.rs");
        assert_eq!(request.span, ReadSpan::lines(1, HiKind::Explicit(50)));
        assert!(
            CodexAdapter.search_request("PreToolUse", &read).is_none(),
            "a read is never a search"
        );

        let listing = call("Bash", serde_json::json!({command: "ls src"}));
        assert_eq!(
            CodexAdapter
                .search_request("PreToolUse", &listing)
                .expect("a listing is a search of names")
                .kind,
            SearchKind::Listing {
                dir: "src".to_owned(),
                pattern: None
            }
        );

        // Everything else on this family is left exactly alone.
        for payload in [
            call("Bash", serde_json::json!({command: "ls src | wc -l"})),
            call("Bash", serde_json::json!({command: "grep -r needle src"})),
            call("Bash", serde_json::json!({command: "cargo test"})),
            call(
                "Write",
                serde_json::json!({"file_path": "/repo/a.rs", "content": "x"}),
            ),
            call("Edit", serde_json::json!({"file_path": "/repo/a.rs"})),
        ] {
            assert!(CodexAdapter.read_request("PreToolUse", &payload).is_none());
            assert!(CodexAdapter
                .search_request("PreToolUse", &payload)
                .is_none());
            assert!(CodexAdapter.edit_request("PreToolUse", &payload).is_none());
        }
    }
}

#[cfg(test)]
mod execution_dir {
    use super::{AgentAdapter, CodexAdapter};

    /// An absolute directory name on whichever platform runs the test.
    fn abs(name: &str) -> String {
        if cfg!(windows) {
            format!("C:\\{name}")
        } else {
            format!("/{name}")
        }
    }

    /// A shell call fired in session directory `cwd`, with `tool_input`
    /// carrying the command and whatever extra fields are given.
    fn shell(cwd: Option<&str>, command: &str, extra: &serde_json::Value) -> serde_json::Value {
        let mut input = serde_json::json!({ "command": command });
        if let (Some(input), Some(extra)) = (input.as_object_mut(), extra.as_object()) {
            input.extend(extra.clone());
        }
        let mut native = serde_json::json!({ "tool_name": "Bash", "tool_input": input });
        if let (Some(native), Some(cwd)) = (native.as_object_mut(), cwd) {
            native.insert("cwd".into(), cwd.into());
        }
        native
    }

    fn read_paths(native: &serde_json::Value) -> Vec<String> {
        CodexAdapter
            .read_requests("PreToolUse", native)
            .into_iter()
            .map(|request| request.path)
            .collect()
    }

    #[test]
    fn a_relative_read_under_an_outside_workdir_names_the_file_in_that_workdir() {
        let root = abs("r");
        let external = abs("e");
        let native = shell(
            Some(&root),
            "sed -n '1,190p' README.md",
            &serde_json::json!({ "workdir": external }),
        );
        let expected = format!("{external}/README.md");
        assert_eq!(read_paths(&native), vec![expected.clone()]);
        let single = CodexAdapter.read_request("PostToolUse", &native);
        assert_eq!(single.map(|request| request.path), Some(expected));
    }

    #[test]
    fn a_relative_workdir_is_taken_from_the_session_directory() {
        let root = abs("r");
        let native = shell(
            Some(&root),
            "cat README.md",
            &serde_json::json!({ "workdir": "sub" }),
        );
        assert_eq!(read_paths(&native), vec![format!("{root}/sub/README.md")]);
    }

    #[test]
    fn a_leading_cd_composes_with_the_workdir_and_an_absolute_file_ignores_it() {
        let external = abs("e");
        let extra = serde_json::json!({ "workdir": external });
        let native = shell(Some(&abs("r")), "cd sub && cat README.md", &extra);
        assert_eq!(
            read_paths(&native),
            vec![format!("{external}/sub/README.md")]
        );
        let elsewhere = format!("{}/notes.md", abs("x"));
        // Quoted, as a shell needs a Windows path's backslashes to be.
        let native = shell(Some(&abs("r")), &format!("cat '{elsewhere}'"), &extra);
        assert_eq!(read_paths(&native), vec![elsewhere]);
    }

    #[test]
    fn a_call_with_no_workdir_keeps_the_path_as_spelled() {
        let native = shell(Some(&abs("r")), "cat README.md", &serde_json::json!({}));
        assert_eq!(read_paths(&native), vec!["README.md".to_owned()]);
    }

    #[test]
    fn an_unusable_workdir_leaves_the_call_with_no_read_at_all() {
        let cases = [
            (Some(abs("r")), serde_json::json!({ "workdir": "" })),
            (Some(abs("r")), serde_json::json!({ "workdir": 7 })),
            (None, serde_json::json!({ "workdir": "sub" })),
            (
                Some("rel".to_owned()),
                serde_json::json!({ "workdir": "sub" }),
            ),
        ];
        for (cwd, extra) in cases {
            let native = shell(cwd.as_deref(), "cat README.md", &extra);
            assert!(read_paths(&native).is_empty(), "{extra}");
            assert!(CodexAdapter.read_request("PreToolUse", &native).is_none());
            assert!(CodexAdapter.searched_file("PreToolUse", &native).is_none());
        }
    }

    #[test]
    fn a_wrapped_or_native_exec_name_is_never_read_as_a_shell_call() {
        for tool in ["exec_command", "functions.exec", "call"] {
            let native = serde_json::json!({
                "tool_name": tool,
                "cwd": abs("r"),
                "tool_input": { "cmd": "cat README.md", "command": "cat README.md" },
            });
            assert!(read_paths(&native).is_empty(), "{tool}");
        }
    }
}
