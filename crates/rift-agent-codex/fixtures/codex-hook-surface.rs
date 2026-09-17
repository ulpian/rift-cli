// PINNED EVIDENCE ARTIFACT — Codex's own published hook surface.
//
// This file is a **fixture**, not a module: nothing includes it as Rust and it
// is never compiled. It exists because an owner decision makes a supported
// agent's OWN published hook surface the authority for its verb table, and
// admits that authority only once it is pinned into this tree as a committed
// artifact a reviewer can re-fetch and re-hash in one step. Until this file
// existed, no row of `crates/rift-shadow-hooks/tests/adapter_sweep.rs` was
// allowed to move and no verb table was allowed to change.
//
// PROVENANCE (every field is asserted by the `pinned_surface` tests in
// `crates/rift-agent-codex/src/lib.rs`):
//   repository:   https://github.com/openai/codex
//   tag:          rust-v0.149.1
//   path:         codex-rs/hooks/src/schema.rs
//   blob-sha:     3847cc91818a9a141c066e1fc6d734adb60f9bde
//   retrieved:    2026-08-24
//   retrieved-from: https://raw.githubusercontent.com/openai/codex/rust-v0.149.1/codex-rs/hooks/src/schema.rs
//
// HOW TO RE-VERIFY, in two commands and no judgement: fetch the
// `retrieved-from` URL above to a scratch file, then
//   git hash-object <that file>   # must print the blob-sha above
// The blob sha is git's own sha1 of the WHOLE upstream file, so it is
// reproducible offline from the downloaded bytes and does not depend on
// GitHub's API agreeing with itself.
//
// WHAT THIS PIN DECIDES, and what it does not. The excerpt below is a CLOSED
// enumeration: every hook event Codex publishes has exactly one
// `*_hook_event_name_schema` function whose body is `string_const_schema("…")`,
// and the upstream file's own generated-fixture constants list the same set one
// `.schema.json` at a time. So this pin answers BOTH conformance directions for
// Codex — "is a declared verb published?" and "is a published verb declared?" —
// which is why `rift_agent_codex` asserts both against it.
// `session_end_reason_schema` is deliberately kept in the excerpt: it is the
// nearest thing to a false positive (a `string_const_schema("other")` that is
// NOT an event), and a reader should be able to see that the parse excludes it
// by naming the `_hook_event_name_schema` suffix rather than by luck.
//
// Nothing here is executed, linked, or shipped. It is quoted for evidence under
// the standing rule for any consulted source: consult behaviour, never copy
// code.

// ---------------------------------------------------------------------------
// VERBATIM EXCERPT — upstream lines 17-38 (the generated-schema fixture names,
// one input file per published hook event, plus outputs where one exists).
// ---------------------------------------------------------------------------

const GENERATED_DIR: &str = "generated";
const POST_TOOL_USE_INPUT_FIXTURE: &str = "post-tool-use.command.input.schema.json";
const POST_TOOL_USE_OUTPUT_FIXTURE: &str = "post-tool-use.command.output.schema.json";
const PERMISSION_REQUEST_INPUT_FIXTURE: &str = "permission-request.command.input.schema.json";
const PERMISSION_REQUEST_OUTPUT_FIXTURE: &str = "permission-request.command.output.schema.json";
const POST_COMPACT_INPUT_FIXTURE: &str = "post-compact.command.input.schema.json";
const POST_COMPACT_OUTPUT_FIXTURE: &str = "post-compact.command.output.schema.json";
const PRE_TOOL_USE_INPUT_FIXTURE: &str = "pre-tool-use.command.input.schema.json";
const PRE_TOOL_USE_OUTPUT_FIXTURE: &str = "pre-tool-use.command.output.schema.json";
const PRE_COMPACT_INPUT_FIXTURE: &str = "pre-compact.command.input.schema.json";
const PRE_COMPACT_OUTPUT_FIXTURE: &str = "pre-compact.command.output.schema.json";
const SESSION_START_INPUT_FIXTURE: &str = "session-start.command.input.schema.json";
const SESSION_START_OUTPUT_FIXTURE: &str = "session-start.command.output.schema.json";
const SESSION_END_INPUT_FIXTURE: &str = "session-end.command.input.schema.json";
const USER_PROMPT_SUBMIT_INPUT_FIXTURE: &str = "user-prompt-submit.command.input.schema.json";
const USER_PROMPT_SUBMIT_OUTPUT_FIXTURE: &str = "user-prompt-submit.command.output.schema.json";
const SUBAGENT_START_INPUT_FIXTURE: &str = "subagent-start.command.input.schema.json";
const SUBAGENT_START_OUTPUT_FIXTURE: &str = "subagent-start.command.output.schema.json";
const SUBAGENT_STOP_INPUT_FIXTURE: &str = "subagent-stop.command.input.schema.json";
const SUBAGENT_STOP_OUTPUT_FIXTURE: &str = "subagent-stop.command.output.schema.json";
const STOP_INPUT_FIXTURE: &str = "stop.command.input.schema.json";
const STOP_OUTPUT_FIXTURE: &str = "stop.command.output.schema.json";

// ---------------------------------------------------------------------------
// VERBATIM EXCERPT — upstream lines 99-121 (the wire enum; ten of the eleven —
// SessionEnd is carried by its own const schema below, because it has no
// output wire type).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub(crate) enum HookEventNameWire {
    #[serde(rename = "PreToolUse")]
    PreToolUse,
    #[serde(rename = "PermissionRequest")]
    PermissionRequest,
    #[serde(rename = "PostToolUse")]
    PostToolUse,
    #[serde(rename = "PreCompact")]
    PreCompact,
    #[serde(rename = "PostCompact")]
    PostCompact,
    #[serde(rename = "SessionStart")]
    SessionStart,
    #[serde(rename = "UserPromptSubmit")]
    UserPromptSubmit,
    #[serde(rename = "SubagentStart")]
    SubagentStart,
    #[serde(rename = "SubagentStop")]
    SubagentStop,
    #[serde(rename = "Stop")]
    Stop,
}

// ---------------------------------------------------------------------------
// VERBATIM EXCERPT — upstream lines 754-800 (the closed enumeration this fixture
// is parsed from: one `*_hook_event_name_schema` per published hook event).
// ---------------------------------------------------------------------------

fn session_start_hook_event_name_schema(_gen: &mut SchemaGenerator) -> Schema {
    string_const_schema("SessionStart")
}

fn session_end_hook_event_name_schema(_gen: &mut SchemaGenerator) -> Schema {
    string_const_schema("SessionEnd")
}

fn session_end_reason_schema(_gen: &mut SchemaGenerator) -> Schema {
    string_const_schema("other")
}

fn post_tool_use_hook_event_name_schema(_gen: &mut SchemaGenerator) -> Schema {
    string_const_schema("PostToolUse")
}

fn pre_compact_hook_event_name_schema(_gen: &mut SchemaGenerator) -> Schema {
    string_const_schema("PreCompact")
}

fn post_compact_hook_event_name_schema(_gen: &mut SchemaGenerator) -> Schema {
    string_const_schema("PostCompact")
}

fn pre_tool_use_hook_event_name_schema(_gen: &mut SchemaGenerator) -> Schema {
    string_const_schema("PreToolUse")
}

fn permission_request_hook_event_name_schema(_gen: &mut SchemaGenerator) -> Schema {
    string_const_schema("PermissionRequest")
}

fn user_prompt_submit_hook_event_name_schema(_gen: &mut SchemaGenerator) -> Schema {
    string_const_schema("UserPromptSubmit")
}

fn subagent_start_hook_event_name_schema(_gen: &mut SchemaGenerator) -> Schema {
    string_const_schema("SubagentStart")
}

fn subagent_stop_hook_event_name_schema(_gen: &mut SchemaGenerator) -> Schema {
    string_const_schema("SubagentStop")
}

fn stop_hook_event_name_schema(_gen: &mut SchemaGenerator) -> Schema {
    string_const_schema("Stop")
}
