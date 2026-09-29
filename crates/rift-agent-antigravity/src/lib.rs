//! First-party adapter for Google Antigravity IDE and CLI.
//!
//! Antigravity's documented hook input is camelCase and does not currently
//! promise a runtime model or token usage. Missing facts remain absent.
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
    event, shell_read, AdapterError, AgentAdapter, CompatNotes, CwdFact, CwdSource, EditKind,
    EditRequest, HiKind, HookFacts, ModelFact, ModelSource, ReadChannel, ReadRequest, ReadSpan,
    SearchKind, SearchRequest, TurnSignals,
};
use rift_core::{AgentId, CanonicalEvent, EventKind, Trust};

/// Google Antigravity adapter.
pub struct AntigravityAdapter;

/// The argument names this adapter reads a tool call through, spelled once.
///
/// They are the values of `fixtures/antigravity-tool-field-surface.md`'s rows,
/// and `pinned_tool_field_surface` asserts the two agree.
mod field {
    /// `view_file`: the path.
    pub(crate) const ABSOLUTE_PATH: &str = "AbsolutePath";
    /// `view_file`: the first line of the range, in a base nothing states.
    pub(crate) const START_LINE: &str = "StartLine";
    /// `view_file`: the last line of the range, in that same base.
    pub(crate) const END_LINE: &str = "EndLine";
    /// Observed large-file pagination input. Its units and result semantics
    /// are not pinned, so its presence forces an unknown read span.
    pub(crate) const CONTENT_OFFSET: &str = "ContentOffset";
    /// `view_file`: whether the file is one of the agent's own skill files.
    /// Read only so that its presence does not take the call out of the read
    /// shape; it says nothing about the range.
    pub(crate) const IS_SKILL_FILE: &str = "IsSkillFile";
    /// On every recorded call and on no published list: the client's own
    /// description of what it is about to do. Read past for the same reason
    /// as the skill-file flag — it says nothing about a range, and refusing a
    /// call because of it would refuse every real call this family makes.
    pub(crate) const TOOL_ACTION: &str = "toolAction";
    /// The shorter half of that same description.
    pub(crate) const TOOL_SUMMARY: &str = "toolSummary";
    /// The three edit tools: the path.
    pub(crate) const TARGET_FILE: &str = "TargetFile";
    /// `write_to_file`: the whole new file.
    pub(crate) const CODE_CONTENT: &str = "CodeContent";
    /// `replace_file_content`: the text written in.
    pub(crate) const REPLACEMENT_CONTENT: &str = "ReplacementContent";
    /// `replace_file_content`: the text taken out.
    pub(crate) const TARGET_CONTENT: &str = "TargetContent";
    /// `replace_file_content`: whether every occurrence was meant.
    pub(crate) const ALLOW_MULTIPLE: &str = "AllowMultiple";
    /// `find_by_name`: the directory searched.
    pub(crate) const SEARCH_DIRECTORY: &str = "SearchDirectory";
    /// `find_by_name`: the name pattern.
    pub(crate) const PATTERN: &str = "Pattern";
    /// `list_dir`: the directory listed.
    pub(crate) const DIRECTORY_PATH: &str = "DirectoryPath";
    /// `run_command`: the shell command line itself.
    pub(crate) const COMMAND_LINE: &str = "CommandLine";
    /// `run_command`: the directory the command line runs in, which is what
    /// makes a relative path inside it resolvable.
    pub(crate) const CWD: &str = "Cwd";
}

/// The tool whose argument is a shell command line.
const SHELL_TOOL: &str = "run_command";

/// How many lines a stretch of text spans — zero for the empty string, else
/// one more than the newlines it contains, whether or not it ends in one.
fn line_span(text: &str) -> u32 {
    if text.is_empty() {
        return 0;
    }
    let breaks = u32::try_from(text.matches('\n').count()).unwrap_or(u32::MAX);
    breaks.saturating_add(1)
}

impl AntigravityAdapter {
    /// Stable Rift agent id.
    pub const AGENT_ID: AgentId = AgentId::ANTIGRAVITY;

    fn tool_name(native: &serde_json::Value) -> Option<&str> {
        native
            .get("toolCall")
            .and_then(|v| v.get("name"))
            .and_then(serde_json::Value::as_str)
    }

    /// The tool's own arguments, one object deeper than the other families
    /// put them.
    fn tool_args(
        native: &serde_json::Value,
    ) -> Option<&serde_json::Map<String, serde_json::Value>> {
        native
            .get("toolCall")
            .and_then(|call| call.get("args"))
            .and_then(serde_json::Value::as_object)
    }

    /// A non-empty string argument.
    fn arg_str<'a>(
        args: &'a serde_json::Map<String, serde_json::Value>,
        key: &str,
    ) -> Option<&'a str> {
        args.get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
    }

    /// A line-number argument, as written.
    fn arg_line(args: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<u32> {
        u32::try_from(args.get(key)?.as_u64()?).ok()
    }

    /// What a shell call was, when the command line is one the pinned grammar
    /// spells and nothing at all otherwise.
    ///
    /// The grammar is another vendor's published read-before-edit list; this
    /// family runs the same commands through its own tool, so the recogniser
    /// is shared and only the argument's name differs.
    fn shell_call(native: &serde_json::Value) -> Option<shell_read::ShellCall> {
        if Self::tool_name(native)? != SHELL_TOOL {
            return None;
        }
        let args = Self::tool_args(native)?;
        shell_read::recognise(Self::arg_str(args, field::COMMAND_LINE)?)
    }

    /// A path from a command line, resolved against the directory the command
    /// ran in.
    ///
    /// This family is the only one that states that directory beside the
    /// command, which is what makes a relative path in one resolvable at all.
    /// The join is lexical — nothing here touches a filesystem — and an
    /// already-absolute path is left exactly as it was written. With no `Cwd`
    /// on the call, a relative path stays relative and the caller resolves it
    /// the way it resolves every other one.
    fn resolve_against_cwd(
        args: &serde_json::Map<String, serde_json::Value>,
        path: String,
    ) -> String {
        if path.starts_with('/') {
            return path;
        }
        match Self::arg_str(args, field::CWD) {
            Some(cwd) => format!("{}/{path}", cwd.trim_end_matches('/')),
            None => path,
        }
    }
}

impl AgentAdapter for AntigravityAdapter {
    fn agent_id(&self) -> AgentId {
        Self::AGENT_ID
    }

    fn map(
        &self,
        verb: &str,
        native: &serde_json::Value,
    ) -> Result<Vec<CanonicalEvent>, AdapterError> {
        use EventKind::{FileEdit, PromptResponse, SessionStart, ToolCall, ToolResult, TurnEnd};
        use Trust::{CodeFact, GitFact, ToolOutputUntrusted};
        Ok(match verb {
            // Antigravity has no explicit session-start hook. Its documented
            // first model invocation is a deterministic implicit start.
            "PreInvocation"
                if native
                    .get("invocationNum")
                    .and_then(serde_json::Value::as_u64)
                    == Some(0) =>
            {
                vec![event(SessionStart, GitFact)]
            }
            "PreInvocation" => Vec::new(),
            "PostInvocation" => vec![event(PromptResponse, GitFact)],
            "PreToolUse" => {
                let mut events = vec![event(ToolCall, CodeFact)];
                if matches!(
                    Self::tool_name(native),
                    Some("write_to_file" | "replace_file_content" | "multi_replace_file_content")
                ) {
                    events.push(event(FileEdit, CodeFact));
                }
                events
            }
            "PostToolUse" => vec![event(ToolResult, ToolOutputUntrusted)],
            "Stop" => vec![event(TurnEnd, GitFact)],
            other => return Err(AdapterError::UnknownVerb(other.to_owned())),
        })
    }

    fn verbs(&self) -> &'static [&'static str] {
        &[
            "PreToolUse",
            "PostToolUse",
            "PreInvocation",
            "PostInvocation",
            "Stop",
        ]
    }

    fn turn_signals(&self) -> TurnSignals {
        TurnSignals {
            start: false,
            end: true,
        }
    }

    fn compat_notes(&self) -> CompatNotes {
        CompatNotes {
            agent: Self::AGENT_ID,
            lifecycle_gaps: &[
                "no explicit session-end event",
                "documented hooks do not expose runtime model or token usage",
                "no turn-start signal (End-only: `Stop` closes a turn nothing opened); the bridge synthesizes Start at session start and after each Stop",
            ],
            surfaces_token_usage: false,
            best_effort: false,
            note: "CamelCase conversation/tool hooks; model remains unavailable unless a concrete client setting is observed.",
        }
    }

    /// `view_file`, over the range **both readings of its line numbering
    /// agree on**.
    ///
    /// The hooks page names the two range arguments and never says whether a
    /// file's first line is line 0 or line 1 — it states a base exactly once,
    /// for a step index, which shows it states one when it means to. Under
    /// 1-based numbering the view delivers the start through the end; under
    /// 0-based it delivers one past each. Every line from one past the start
    /// through the end is delivered either way, and that is what is recorded:
    /// one line narrower at the top than the confident reading, never a line
    /// the tool did not deliver.
    ///
    /// This is not a guess split down the middle. It is the intersection of
    /// the only two possibilities, so it stays right whichever is true, and a
    /// re-pin that settles the base can only widen it.
    ///
    /// With no range given, both readings start at the file's first line — but
    /// nothing says where such a view stops, so the span is unknown and no
    /// coverage is recorded.
    fn read_request(&self, _verb: &str, native: &serde_json::Value) -> Option<ReadRequest> {
        if Self::tool_name(native)? == SHELL_TOOL {
            let mut request = Self::shell_call(native)?.read_request()?;
            request.path = Self::resolve_against_cwd(Self::tool_args(native)?, request.path);
            return Some(request);
        }
        if Self::tool_name(native)? != "view_file" {
            return None;
        }
        let args = Self::tool_args(native)?;
        // An argument this pin does not carry is one whose effect on the
        // range nobody has checked, so it takes the call out of the shape
        // rather than being ignored beside a range that assumes it away.
        if args.keys().any(|key| {
            !matches!(
                key.as_str(),
                field::ABSOLUTE_PATH
                    | field::START_LINE
                    | field::END_LINE
                    | field::IS_SKILL_FILE
                    | field::TOOL_ACTION
                    | field::TOOL_SUMMARY
                    | field::CONTENT_OFFSET
            )
        }) {
            return None;
        }
        let path = Self::arg_str(args, field::ABSOLUTE_PATH)?.to_owned();
        // Observed on an installed client's large-file calls, but neither its
        // request unit nor its delivered-result semantics are vendor-pinned.
        // Preserve that uncertainty instead of pretending the line arguments
        // describe what reached the model.
        if args.contains_key(field::CONTENT_OFFSET) {
            return Some(ReadRequest {
                path,
                span: ReadSpan::Unknown,
                channel: ReadChannel::Tool,
            });
        }
        let Some(start) = Self::arg_line(args, field::START_LINE) else {
            return Some(ReadRequest {
                path,
                span: ReadSpan::Unknown,
                channel: ReadChannel::Tool,
            });
        };
        let lo = start.saturating_add(1);
        let span = match Self::arg_line(args, field::END_LINE) {
            Some(end) if end >= lo => ReadSpan::lines(lo, HiKind::Explicit(end)),
            // An end below the first line the two readings agree on leaves
            // them agreeing on nothing.
            _ => ReadSpan::Unknown,
        };
        Some(ReadRequest {
            path,
            span,
            channel: ReadChannel::Tool,
        })
    }

    /// Every read one call performs — which for the shell tool is one per
    /// member of the command line, each resolved against the directory this
    /// family states beside the command exactly as the single one is.
    fn read_requests(&self, verb: &str, native: &serde_json::Value) -> Vec<ReadRequest> {
        let shell = Self::tool_name(native) == Some(SHELL_TOOL);
        let Some(args) = Self::tool_args(native).filter(|_| shell) else {
            return self.read_request(verb, native).into_iter().collect();
        };
        Self::shell_call(native)
            .map(|call| {
                call.read_requests()
                    .into_iter()
                    .map(|mut request| {
                        request.path = Self::resolve_against_cwd(args, request.path);
                        request
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The three file-changing tools, split by whether one edit site follows
    /// from the payload.
    ///
    /// Recorded, and never acted on: nothing on this family's page says what
    /// an edit returns to the model, so a decision resting on "you already saw
    /// this change" has no evidence behind it here. Recording still matters —
    /// the coverage of a file's old bytes must stop counting the moment they
    /// change, whoever changed them.
    fn edit_request(&self, _verb: &str, native: &serde_json::Value) -> Option<EditRequest> {
        let tool = Self::tool_name(native)?;
        let args = Self::tool_args(native)?;
        let path = Self::arg_str(args, field::TARGET_FILE)?.to_owned();
        let kind = match tool {
            "write_to_file" => match Self::arg_str(args, field::CODE_CONTENT) {
                Some(content) => EditKind::Write {
                    content_sha256: rift_agent::content_sha256(content.as_bytes()),
                },
                None => EditKind::Other,
            },
            "replace_file_content" => match (
                Self::arg_str(args, field::REPLACEMENT_CONTENT),
                args.get(field::TARGET_CONTENT)
                    .and_then(serde_json::Value::as_str),
            ) {
                (Some(new_string), Some(old_string)) => EditKind::Edit {
                    new_string: new_string.to_owned(),
                    old_string_lines: line_span(old_string),
                    replace_all: args
                        .get(field::ALLOW_MULTIPLE)
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                },
                _ => EditKind::Other,
            },
            "multi_replace_file_content" => EditKind::Other,
            _ => return None,
        };
        Some(EditRequest { path, kind })
    }

    /// Two listings and one content search.
    ///
    /// The content search reports that no cap was declared, and that is the
    /// whole truth of it: the tool takes no result-cap argument at all, so
    /// there is nothing for an additive cap to attach to and no row offers
    /// one. A content search here is recorded and left alone.
    fn search_request(&self, _verb: &str, native: &serde_json::Value) -> Option<SearchRequest> {
        let args = Self::tool_args(native)?;
        if Self::tool_name(native)? == SHELL_TOOL {
            let mut request = Self::shell_call(native)?.search_request()?;
            if let SearchKind::Listing { dir, .. } = &mut request.kind {
                *dir = Self::resolve_against_cwd(args, std::mem::take(dir));
            }
            return Some(request);
        }
        let kind = match Self::tool_name(native)? {
            "find_by_name" => SearchKind::Listing {
                dir: Self::arg_str(args, field::SEARCH_DIRECTORY)?.to_owned(),
                pattern: Self::arg_str(args, field::PATTERN).map(str::to_owned),
            },
            "list_dir" => SearchKind::Listing {
                dir: Self::arg_str(args, field::DIRECTORY_PATH)?.to_owned(),
                pattern: None,
            },
            "grep_search" => SearchKind::Grep {
                has_head_limit: false,
            },
            _ => return None,
        };
        Some(SearchRequest { kind })
    }

    /// The one file a content search named, where the shell grammar named
    /// it — resolved against the directory this family states beside the
    /// command, exactly as its shell reads are.
    ///
    /// Its own content-search tool is absent for the reason its row gives:
    /// the tool takes no result-cap argument and its pin names no path
    /// argument either, so there is nothing here to read.
    fn searched_file(&self, _verb: &str, native: &serde_json::Value) -> Option<String> {
        let args = Self::tool_args(native)?;
        let path = Self::shell_call(native)?.searched_file()?.to_owned();
        Some(Self::resolve_against_cwd(args, path))
    }

    fn facts(&self, _verb: &str, native: &serde_json::Value) -> HookFacts {
        let tool_name = Self::tool_name(native).map(str::to_owned);
        let args = native.get("toolCall").and_then(|v| v.get("args"));
        let read_path = tool_name
            .as_deref()
            .filter(|name| *name == "view_file")
            .and(args)
            .and_then(|v| v.get("AbsolutePath"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        // Future-compatible: accept an explicit documented-looking runtime
        // field if Antigravity adds it, but never inspect transcript/private DB.
        let runtime_model = native
            .get("model")
            .and_then(serde_json::Value::as_str)
            .filter(|m| !m.is_empty() && !m.eq_ignore_ascii_case("auto"))
            .map(|model| ModelFact {
                model: model.to_owned(),
                source: ModelSource::RuntimeHook,
            });
        let model = runtime_model.or_else(|| {
            native
                .get("_rift_configured_model")
                .and_then(serde_json::Value::as_str)
                .filter(|model| {
                    !model.is_empty()
                        && !model.eq_ignore_ascii_case("auto")
                        && !model.eq_ignore_ascii_case("default")
                })
                .map(|model| ModelFact {
                    model: model.to_owned(),
                    source: ModelSource::ConfiguredClient,
                })
        });
        HookFacts {
            model,
            tool_name,
            child_session: None,
            read_path,
            // Antigravity's payload has NO `cwd`; the workspace the IDE has
            // open is the only directory it names, and it names it as a list
            // under the `workspace_path` basis. The first entry is the
            // one the agent's tools run against; the basis travels with the
            // value so a reader is never told a workspace root was a cwd.
            cwd: native
                .get("workspacePaths")
                .and_then(serde_json::Value::as_array)
                .and_then(|paths| paths.first())
                .and_then(serde_json::Value::as_str)
                .filter(|p| !p.is_empty())
                .map(|path| CwdFact {
                    path: path.to_owned(),
                    source: CwdSource::WorkspacePath,
                }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_agent::conformance::{check, Case};
    use rift_core::EventKind::{
        FileEdit, PromptResponse, SessionStart, ToolCall, ToolResult, TurnEnd,
    };

    #[test]
    fn documented_events_map_without_inventing_a_session_end() {
        let cases = vec![
            Case::new(
                "PreInvocation",
                serde_json::json!({"invocationNum": 0}),
                &[SessionStart],
            ),
            Case::new(
                "PostInvocation",
                serde_json::json!({"invocationNum": 0}),
                &[PromptResponse],
            ),
            Case::new(
                "PreToolUse",
                serde_json::json!({"toolCall":{"name":"view_file","args":{"AbsolutePath":"/repo/a.rs"}}}),
                &[ToolCall],
            ),
            Case::new(
                "PreToolUse",
                serde_json::json!({"toolCall":{"name":"write_to_file"}}),
                &[ToolCall, FileEdit],
            ),
            Case::new(
                "PostToolUse",
                serde_json::json!({"stepIdx": 1}),
                &[ToolResult],
            ),
            Case::new("Stop", serde_json::json!({"fullyIdle": true}), &[TurnEnd]),
        ];
        assert!(AntigravityAdapter
            .verbs()
            .iter()
            .all(|verb| cases.iter().any(|case| case.verb == *verb)));
        check(&AntigravityAdapter, &cases).unwrap();
    }

    #[test]
    fn nested_tool_facts_and_model_absence_are_truthful() {
        let facts = AntigravityAdapter.facts(
            "PreToolUse",
            &serde_json::json!({
                "conversationId":"c",
                "toolCall":{"name":"view_file","args":{"AbsolutePath":"/repo/a.rs"}}
            }),
        );
        assert_eq!(facts.tool_name.as_deref(), Some("view_file"));
        assert_eq!(facts.read_path.as_deref(), Some("/repo/a.rs"));
        assert!(facts.model.is_none());
    }
    /// Antigravity's payload has no `cwd` at all: `workspacePaths[0]` is the
    /// only directory it names, and it is recorded under its own basis so a
    /// reader is never told a workspace root was an agent's cwd.
    #[test]
    fn the_workspace_path_is_the_cwd_antigravity_does_not_send() {
        let facts = AntigravityAdapter.facts(
            "PreToolUse",
            &serde_json::json!({
                "conversationId": "c",
                "workspacePaths": ["/repo/member", "/repo/other"]
            }),
        );
        let cwd = facts
            .cwd
            .expect("workspacePaths[0] is the observed directory");
        assert_eq!(cwd.path, "/repo/member");
        assert_eq!(cwd.source, CwdSource::WorkspacePath);

        // No list, an empty list and an empty first entry are all UNKNOWN.
        for payload in [
            serde_json::json!({"conversationId": "c"}),
            serde_json::json!({"conversationId": "c", "workspacePaths": []}),
            serde_json::json!({"conversationId": "c", "workspacePaths": [""]}),
            serde_json::json!({"conversationId": "c", "workspacePaths": "not-a-list"}),
        ] {
            assert!(
                AntigravityAdapter
                    .facts("PreToolUse", &payload)
                    .cwd
                    .is_none(),
                "{payload}"
            );
        }
        // And it is never mistaken for the `cwd` basis, which this agent has
        // no field for.
        assert!(AntigravityAdapter
            .facts("PreToolUse", &serde_json::json!({"cwd": "/elsewhere"}))
            .cwd
            .is_none());
    }
}

/// **Antigravity's hook *output* contract, pinned into this tree.**
///
/// The artifact is `fixtures/antigravity-hook-output-surface.md`, and it carries a documentation page's digest and the date it was fetched,
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

    use super::{AgentAdapter, AntigravityAdapter};

    /// The committed pin. `include_str!` rather than a runtime read so a
    /// missing or renamed fixture is a compile error, not a skipped test.
    const FIXTURE: &str = include_str!("../fixtures/antigravity-hook-output-surface.md");

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

    /// The page publishes no version string, so the pin is its digest and the
    /// date it was fetched — recorded as exactly that, rather than dressed up
    /// as a tag it does not have.
    #[test]
    fn the_pin_states_a_provenance_a_reader_can_recheck() {
        assert_eq!(
            provenance("repository", 0),
            "https://antigravity.google/docs/hooks/"
        );
        assert_eq!(
            provenance("revision", 0),
            "none published; the page carries no version or revision string"
        );
        assert_eq!(provenance("path", 0), "/docs/hooks.md");
        assert_digest("sha256", 0, 64);
        let from = provenance("retrieved-from", 0);
        assert!(
            from.ends_with("/docs/hooks.md"),
            "{from} does not name the path it pins"
        );
        assert_eq!(provenance("retrieved", 0), "2026-09-03");
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
            capability_rows(&AgentId::ANTIGRAVITY),
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
                AntigravityAdapter.verbs().contains(&verb),
                "the pin claims a channel on {verb}, which this adapter does not register"
            );
        }
    }
}

/// **The declined call, as bytes, held against the pin that states them.**
///
/// The channel table above settles that this family *may* be declined and in
/// which field the explanation travels. It does not settle what goes on the
/// wire, and a client reads bytes: an envelope with the right field names in
/// the wrong shape is a call that runs when Rift meant to decline it, or a
/// broken agent. So the artifact carries the object itself, with the rendered
/// reason standing in as a placeholder, and this module asserts that what
/// `rift_agent::respond::gate` writes for this family is that object character
/// for character.
///
/// The client's own prefix is pinned in the same fence for the opposite
/// reason: it is the one part of what the model reads that Rift must never
/// touch. Recorded runs show it wrapping the reason on delivery, so a reader
/// who finds it in a transcript can see here that it was the client's and not
/// something this tree wrote, trimmed or compensated for.
#[cfg(test)]
mod pinned_decline_response {
    use rift_agent::respond::{gate, GateDecision};
    use rift_core::AgentId;

    /// The committed pin, again by `include_str!` so a rename is a compile
    /// error rather than a skipped test.
    const FIXTURE: &str = include_str!("../fixtures/antigravity-hook-output-surface.md");

    /// The fence the declined-call bytes live in, so prose that quotes the
    /// object while explaining it never becomes the pin.
    const FENCE: &str = "```rift-decline-response";

    /// The stand-in the pin splices the rendered reason into.
    const PLACEHOLDER: &str = "\"<REASON>\"";

    /// One `key=value` line from inside the fence, taken whole, with a quoted
    /// value unquoted. Deliberately dumb, for the reason the row parser beside
    /// it is: a parse clever enough to be forgiving is a parse that can be
    /// talked into accepting what the artifact does not say.
    fn pinned(key: &str) -> String {
        let mut inside = false;
        for line in FIXTURE.lines() {
            if inside {
                if line.trim_end() == "```" {
                    break;
                }
                if let Some(value) = line.trim().strip_prefix(&format!("{key}=")) {
                    return value
                        .strip_prefix('"')
                        .and_then(|v| v.strip_suffix('"'))
                        .unwrap_or(value)
                        .to_owned();
                }
            } else if line.trim_end() == FENCE {
                inside = true;
            }
        }
        panic!("the pin states no {key}")
    }

    /// The bytes the pin states for a decline carrying `reason`.
    fn expected(reason: &str) -> String {
        let response = pinned("response");
        assert!(
            response.contains(PLACEHOLDER),
            "the pinned object carries no place for the reason: {response}"
        );
        response.replace(
            PLACEHOLDER,
            &serde_json::to_string(reason).expect("a string serialises"),
        )
    }

    /// ★ **The object this family is declined with is the object the artifact
    /// pins**, serialised, not merely equal field by field.
    ///
    /// A reason with a quote and a backslash in it is used on purpose: the
    /// splice has to be a JSON one, and a comparison that only ever saw plain
    /// text would pass for a renderer that emitted a broken document the day a
    /// path contained a quote.
    #[test]
    fn a_declined_call_is_the_object_the_pin_states() {
        for reason in [
            "[rift] Read skipped: a.txt@sha256:0123456789ab lines 1-40 are unchanged.",
            "[rift] Read skipped: \"quoted\\path\" lines 1-2 are unchanged.",
        ] {
            let written = gate(
                &AgentId::ANTIGRAVITY,
                "PreToolUse",
                &GateDecision::Deny { reason },
            )
            .expect("a confirmed reason channel is answered on");
            assert_eq!(
                serde_json::to_string(&written).expect("a response serialises"),
                expected(reason),
                "the bytes on the wire and the bytes in the pin have parted; re-pin \
                 the surface from a recorded run, never edit one to match the other"
            );
        }
    }

    /// ★ **Rift writes the frame and nothing around it.** The prefix a model
    /// actually reads is the client's, and it is pinned here so that no one
    /// adds a second one — the failure that would follow is silent, because a
    /// doubled prefix still parses and still reads almost right.
    #[test]
    fn the_reason_is_the_renderers_bytes_and_the_prefix_is_the_clients() {
        let reason = "[rift] Read skipped: a.txt@sha256:0123456789ab lines 1-40 are unchanged.";
        let written = gate(
            &AgentId::ANTIGRAVITY,
            "PreToolUse",
            &GateDecision::Deny { reason },
        )
        .expect("a confirmed reason channel is answered on");
        assert_eq!(
            written["reason"], reason,
            "the explanation travels verbatim or it is a second renderer"
        );

        let prefix = pinned("client_prefix");
        assert!(
            !prefix.is_empty(),
            "the pin states the client's prefix as an empty string"
        );
        assert!(
            !reason.starts_with(&prefix) && !written["reason"].as_str().unwrap().contains(&prefix),
            "the prefix is the client's own; writing it here would double it"
        );
        assert_eq!(
            format!("{prefix}{reason}"),
            format!("tool call denied by pre-tool hook: {reason}"),
            "what the recorded runs saw the model read, spelled out once so a \
             later reader can match it against a transcript"
        );
    }

    /// ★ **One renderer, two families.** The reason bytes handed to this
    /// family and to the other confirmed one are the same; only the envelope
    /// around them differs.
    ///
    /// Which is the property that makes a decline reviewable at all: a reader
    /// checking what a model was told reads one renderer's output, not one per
    /// family, and a family-shaped wording could not be red-teamed once.
    #[test]
    fn both_confirmed_families_are_declined_with_the_same_reason() {
        let reason = "[rift] Read skipped: a.txt@sha256:0123456789ab lines 1-40 are unchanged.";
        let here = gate(
            &AgentId::ANTIGRAVITY,
            "PreToolUse",
            &GateDecision::Deny { reason },
        )
        .expect("this family is answered on");
        let there = gate(
            &AgentId::CLAUDE_CODE,
            "PreToolUse",
            &GateDecision::Deny { reason },
        )
        .expect("the other confirmed family is answered on");
        assert_eq!(
            here["reason"], there["hookSpecificOutput"]["permissionDecisionReason"],
            "two envelopes, one explanation"
        );
    }
}

/// **Antigravity's read, edit and search *tool fields*, pinned into this tree.**
///
/// The artifact is `fixtures/antigravity-tool-field-surface.md`: a page digest
/// and a retrieval date, verbatim argument lists for every tool this adapter
/// reads, and — as prominently — what the page does not state. The line
/// numbering's base is the one that matters, and the range this adapter
/// derives is narrower than a confident reader's because of it.
#[cfg(test)]
mod pinned_tool_field_surface {
    use rift_agent::tool_fields::{pinned_fact, pinned_tool_field_rows, tool_field_rows};
    use rift_core::AgentId;

    use super::{AgentAdapter, AntigravityAdapter, EditKind, HiKind, ReadSpan, SearchKind};

    /// The committed pin. `include_str!` rather than a runtime read so a
    /// missing or renamed fixture is a compile error, not a skipped test.
    const FIXTURE: &str = include_str!("../fixtures/antigravity-tool-field-surface.md");

    /// The value the pin gives for one of this family's argument names.
    fn pinned(tool: &str, key: &str) -> &'static str {
        pinned_fact(&AgentId::ANTIGRAVITY, tool, key)
            .unwrap_or_else(|| panic!("the table states no {key} for {tool}"))
    }

    /// A `PreToolUse` payload, with the arguments nested where this family
    /// nests them.
    fn call(tool: &str, args: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"toolCall": {"name": tool, "args": args}})
    }

    /// The page has no tag to pin, so its digest and its retrieval date are.
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
        assert_eq!(
            field("repository"),
            ["https://antigravity.google/docs/hooks"]
        );
        assert_eq!(field("path"), ["/docs/hooks.md"]);
        let digest = field("sha256");
        assert_eq!(digest.len(), 1, "one page, one digest");
        assert_eq!(digest[0].len(), 64);
        assert!(
            digest[0]
                .chars()
                .all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "a digest must be lowercase hex"
        );
        assert!(field("retrieved-from")[0].contains("hooks.md"));
    }

    /// **The pin and the code table are one set**, in both directions.
    #[test]
    fn the_tool_field_table_equals_the_pinned_fixture() {
        let pinned_rows = pinned_tool_field_rows(FIXTURE);
        assert!(!pinned_rows.is_empty(), "the pin states no tool-field rows");
        assert_eq!(
            pinned_rows,
            tool_field_rows(&AgentId::ANTIGRAVITY),
            "the pinned tool fields and the code table disagree; re-pin the \
             surface or drop the row, never edit one to match the other"
        );
    }

    /// A view is read through the nested argument names the pin carries, and
    /// its range is the one both readings of an undocumented numbering base
    /// agree on: one past the start argument, through the end argument.
    #[test]
    fn read_request_reads_the_pinned_fields() {
        assert_eq!(
            pinned("view_file", "lo_base"),
            "undocumented",
            "the narrowing below is only justified while the base is unstated"
        );
        let payload = call(
            "view_file",
            serde_json::json!({
                pinned("view_file", "path"): "/repo/src/lib.rs",
                pinned("view_file", "lo"): 40,
                pinned("view_file", "hi"): 90,
            }),
        );
        let request = AntigravityAdapter
            .read_request("PreToolUse", &payload)
            .expect("a view with a path and a range is read-shaped");
        assert_eq!(request.path, "/repo/src/lib.rs");
        assert_eq!(
            request.span,
            ReadSpan::lines(41, HiKind::Explicit(90)),
            "one past the start, under either reading"
        );
    }

    /// A view with no range starts at the file's first line under either
    /// reading, and stops nowhere anyone published.
    #[test]
    fn a_view_with_no_range_has_no_span() {
        let rangeless = call(
            "view_file",
            serde_json::json!({pinned("view_file", "path"): "/repo/src/lib.rs"}),
        );
        let request = AntigravityAdapter
            .read_request("PreToolUse", &rangeless)
            .expect("a bare view is still a read");
        assert_eq!(request.span, ReadSpan::Unknown);
        assert_eq!(request.conservative_lines(), None);
    }

    /// A documented argument that says nothing about the range leaves the
    /// call read-shaped. The envelope keys are here because a recorded run
    /// put them on every real call — a rule that refused them would refuse
    /// this family outright, which is how a whole plane goes quiet without a
    /// failure.
    #[test]
    fn the_recorded_envelope_arguments_leave_a_view_read_shaped() {
        assert_eq!(
            pinned("view_file", "envelope"),
            "toolAction,toolSummary",
            "the accepted-argument set is the pin's, not this test's"
        );
        let as_recorded = call(
            "view_file",
            serde_json::json!({
                pinned("view_file", "path"): "/repo/src/lib.rs",
                "toolAction": "Viewing lib.rs first time",
                "toolSummary": "View lib.rs",
            }),
        );
        let request = AntigravityAdapter
            .read_request("PreToolUse", &as_recorded)
            .expect("the shape a recorded run actually carries is read-shaped");
        assert_eq!(request.path, "/repo/src/lib.rs");
        assert_eq!(request.span, ReadSpan::Unknown);
    }

    /// The pinned flag is a documented argument, so it leaves the call
    /// read-shaped.
    #[test]
    fn the_pinned_flag_leaves_a_view_read_shaped() {
        let with_flag = call(
            "view_file",
            serde_json::json!({
                pinned("view_file", "path"): "/repo/src/lib.rs",
                "IsSkillFile": true,
            }),
        );
        assert_eq!(pinned("view_file", "flag"), "IsSkillFile");
        assert!(AntigravityAdapter
            .read_request("PreToolUse", &with_flag)
            .is_some());
    }

    /// An argument the pin does not carry takes the call out of read shape.
    #[test]
    fn an_unpinned_argument_is_not_read_shaped() {
        let with_an_unpinned_argument = call(
            "view_file",
            serde_json::json!({
                pinned("view_file", "path"): "/repo/src/lib.rs",
                pinned("view_file", "hi"): 90,
                "Pages": "1-5",
            }),
        );
        assert!(AntigravityAdapter
            .read_request("PreToolUse", &with_an_unpinned_argument)
            .is_none());
    }

    /// A range the two readings agree on nothing about is no range.
    #[test]
    fn an_empty_range_is_no_range() {
        let inverted = call(
            "view_file",
            serde_json::json!({
                pinned("view_file", "path"): "/repo/src/lib.rs",
                pinned("view_file", "lo"): 40,
                pinned("view_file", "hi"): 40,
            }),
        );
        assert_eq!(
            AntigravityAdapter
                .read_request("PreToolUse", &inverted)
                .expect("still a read")
                .span,
            ReadSpan::Unknown
        );
    }

    /// The three shapes stay apart, and the content search reports the truth
    /// about itself: this family's grep takes no result-cap argument, so there
    /// is nothing an additive cap could attach to.
    #[test]
    fn search_request_distinguishes_glob_grep_and_listing() {
        let by_name = call(
            "find_by_name",
            serde_json::json!({
                pinned("find_by_name", "dir"): "/repo/src",
                pinned("find_by_name", "pattern"): "*.rs",
            }),
        );
        assert_eq!(
            AntigravityAdapter
                .search_request("PreToolUse", &by_name)
                .expect("a name search is a search")
                .kind,
            SearchKind::Listing {
                dir: "/repo/src".to_owned(),
                pattern: Some("*.rs".to_owned()),
            }
        );

        let listing = call(
            "list_dir",
            serde_json::json!({pinned("list_dir", "dir"): "/repo/src"}),
        );
        assert_eq!(
            AntigravityAdapter
                .search_request("PreToolUse", &listing)
                .expect("a listing is a search")
                .kind,
            SearchKind::Listing {
                dir: "/repo/src".to_owned(),
                pattern: None,
            }
        );

        let grep = call("grep_search", serde_json::json!({"Query": "respond"}));
        assert_eq!(
            AntigravityAdapter
                .search_request("PreToolUse", &grep)
                .expect("a content search is a search")
                .kind,
            SearchKind::Grep {
                has_head_limit: false
            }
        );
        assert_eq!(
            pinned("grep_search", "head_limit"),
            "none",
            "the pin's own answer: there is no cap argument to add"
        );

        // A shell listing is a listing, and a relative directory in one is
        // resolved against the directory the command ran in.
        let shell = call(
            "run_command",
            serde_json::json!({
                pinned("run_command", "command"): "ls src",
                pinned("run_command", "cwd"): "/repo",
            }),
        );
        assert_eq!(
            AntigravityAdapter
                .search_request("PreToolUse", &shell)
                .expect("a listing is a search of names")
                .kind,
            SearchKind::Listing {
                dir: "/repo/src".to_owned(),
                pattern: None
            }
        );
        assert!(AntigravityAdapter
            .read_request("PreToolUse", &shell)
            .is_none());
        assert!(AntigravityAdapter
            .edit_request("PreToolUse", &shell)
            .is_none());
    }

    /// **A command line is a read only under the pinned grammar**, and the
    /// path it names is resolved against the working directory this family
    /// states beside it — the fact that makes a relative path in a command
    /// line resolvable at all.
    #[test]
    fn a_shell_read_resolves_its_path_against_the_stated_directory() {
        let command = pinned("run_command", "command");
        let cwd = pinned("run_command", "cwd");

        let read = call(
            "run_command",
            serde_json::json!({command: "sed -n '3,9p' src/lib.rs", cwd: "/repo"}),
        );
        let request = AntigravityAdapter
            .read_request("PreToolUse", &read)
            .expect("a printed line range is a read of those lines");
        assert_eq!(request.path, "/repo/src/lib.rs");
        assert_eq!(request.span, ReadSpan::lines(3, HiKind::Explicit(9)));

        // An absolute path is already resolved, and a command line with no
        // stated directory leaves the path as it was written.
        let absolute = call(
            "run_command",
            serde_json::json!({command: "cat /elsewhere/lib.rs", cwd: "/repo"}),
        );
        assert_eq!(
            AntigravityAdapter
                .read_request("PreToolUse", &absolute)
                .expect("a whole-file read")
                .path,
            "/elsewhere/lib.rs"
        );
        let bare = call(
            "run_command",
            serde_json::json!({command: "cat src/lib.rs"}),
        );
        assert_eq!(
            AntigravityAdapter
                .read_request("PreToolUse", &bare)
                .expect("a whole-file read")
                .path,
            "src/lib.rs"
        );

        for line in ["ls src | wc -l", "grep -r needle src", "npm test"] {
            let payload = call(
                "run_command",
                serde_json::json!({command: line, cwd: "/repo"}),
            );
            assert!(
                AntigravityAdapter
                    .read_request("PreToolUse", &payload)
                    .is_none(),
                "{line}"
            );
            assert!(
                AntigravityAdapter
                    .search_request("PreToolUse", &payload)
                    .is_none(),
                "{line}"
            );
        }
    }

    /// An edit is read as a site only where one site follows from the payload;
    /// the many-chunk tool is a change with no derivable site.
    #[test]
    fn edit_request_reads_the_pinned_fields() {
        let write = call(
            "write_to_file",
            serde_json::json!({
                pinned("write_to_file", "path"): "/repo/src/new.rs",
                pinned("write_to_file", "content"): "fn main() {}\n",
            }),
        );
        let request = AntigravityAdapter
            .edit_request("PreToolUse", &write)
            .expect("a write changes a file");
        assert_eq!(request.path, "/repo/src/new.rs");
        assert_eq!(
            request.kind,
            EditKind::Write {
                content_sha256: rift_agent::content_sha256(b"fn main() {}\n")
            }
        );

        let replace = call(
            "replace_file_content",
            serde_json::json!({
                pinned("replace_file_content", "path"): "/repo/src/lib.rs",
                pinned("replace_file_content", "old"): "one\ntwo",
                pinned("replace_file_content", "new"): "one\ntwo\nthree",
                pinned("replace_file_content", "replace_all"): true,
            }),
        );
        assert_eq!(
            AntigravityAdapter
                .edit_request("PreToolUse", &replace)
                .expect("a replacement changes a file")
                .kind,
            EditKind::Edit {
                new_string: "one\ntwo\nthree".to_owned(),
                old_string_lines: 2,
                replace_all: true,
            }
        );

        let many = call(
            "multi_replace_file_content",
            serde_json::json!({
                pinned("multi_replace_file_content", "path"): "/repo/src/lib.rs",
                "ReplacementChunks": [],
            }),
        );
        assert_eq!(
            AntigravityAdapter
                .edit_request("PreToolUse", &many)
                .expect("many replacements still change a file")
                .kind,
            EditKind::Other,
            "several non-contiguous edits carry no single before-and-after pair"
        );
    }
}

/// A sanitized replay of one installed client's pathological large-ledger
/// sequence. This is deliberately separate from the pinned vendor surfaces
/// above: it records a local observation worth reproducing, not a contract
/// Rift may use to infer which bytes reached the model.
#[cfg(test)]
mod content_offset_replay {
    use rift_agent::{AgentAdapter, ReadSpan};

    use super::AntigravityAdapter;

    const FIXTURE: &str = include_str!("../fixtures/antigravity-content-offset-replay.json");

    /// The screenshot sequence stays reproducible without retaining ledger
    /// content, absolute user paths, or session/account identifiers. A present
    /// adjacent result is still only an observation: its delivery semantics
    /// remain unpinned until the installed client is tested on both sides.
    #[test]
    fn the_sanitized_large_ledger_replay_locks_call_and_result_order() {
        let fixture: serde_json::Value =
            serde_json::from_str(FIXTURE).expect("the committed replay is valid JSON");
        assert_eq!(
            fixture["provenance"]["kind"],
            "sanitized_local_installed_client_observation"
        );
        assert_eq!(fixture["provenance"]["vendor_schema"], false);
        assert_eq!(fixture["scope"]["path"], "imp/LEDGER.md");
        assert!(
            !FIXTURE.contains("/Users/"),
            "no local home path is retained"
        );

        let observed = fixture["observations"]
            .as_array()
            .expect("the replay carries an ordered observation list")
            .iter()
            .map(|entry| {
                let request = &entry["request"];
                let result = entry["result_metadata"].as_object().map(|metadata| {
                    (
                        metadata["content_chars"]
                            .as_u64()
                            .expect("bounded result character count"),
                        metadata["content_lines"]
                            .as_u64()
                            .expect("bounded result line count"),
                    )
                });
                assert_eq!(entry["result_observed"].as_bool(), Some(result.is_some()));
                (
                    request["ContentOffset"].as_u64().expect("content offset"),
                    request["StartLine"].as_u64().expect("start line"),
                    request["EndLine"].as_u64().expect("end line"),
                    result,
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            observed,
            vec![
                (230_000, 270, 339, None),
                (150_000, 270, 339, Some((45_662, 27))),
                (185_000, 287, 341, None),
                (40_000, 287, 341, Some((46_578, 30))),
                (95_000, 287, 341, Some((46_729, 31))),
                (130_000, 308, 341, None),
                (0, 308, 341, Some((46_634, 32))),
                (135_000, 330, 341, None),
                (0, 330, 341, Some((34_165, 21))),
                (0, 341, 345, Some((655, 10))),
            ]
        );
    }

    /// Every observed content-offset call remains visible as read-shaped, but
    /// none can create line coverage until both the request unit and the
    /// delivered result channel are pinned.
    #[test]
    fn every_content_offset_replay_call_has_an_unknown_span() {
        let fixture: serde_json::Value =
            serde_json::from_str(FIXTURE).expect("the committed replay is valid JSON");
        for observation in fixture["observations"]
            .as_array()
            .expect("ordered observations")
        {
            let request = &observation["request"];
            let payload = serde_json::json!({
                "toolCall": {
                    "name": "view_file",
                    "args": {
                        "AbsolutePath": "/repo/imp/LEDGER.md",
                        "ContentOffset": request["ContentOffset"],
                        "StartLine": request["StartLine"],
                        "EndLine": request["EndLine"],
                    }
                }
            });
            let read = AntigravityAdapter
                .read_request("PreToolUse", &payload)
                .expect("content-offset view stays read-shaped");
            assert_eq!(read.span, ReadSpan::Unknown);
        }
    }
}
