//! What an agent sends: the scripted hook events, and the fixture spellings a
//! payload is built out of.
//!
//! Separate from the catalogue because a payload is a claim about the agent's
//! wire shape rather than about what the harness measures — the body a verb
//! carries has to stay the one a live session would send, whatever the report
//! decides to time.

use std::path::{Path, PathBuf};

use crate::measure::Invocation;

use super::plan::PlanCtx;

/// A representative Claude Code hook payload in the native shape rift's wire
/// layer accepts (`session_id` + `hook_event_name`, no envelope).
///
/// The first event of the shared session, spelled out so the shape a hook row
/// really sends is readable without running one; [`HookEvent::ToolCallBash`]
/// reproduces it, pinned by test.
pub const HOOK_PAYLOAD: &str = concat!(
    r#"{"session_id":"01JBENCH000000000000000000","hook_event_name":"PreToolUse","#,
    r#""tool_name":"Bash","tool_input":{"command":"cargo test --workspace case_0"}}"#
);

/// The established session the warm hook rows share: 26 characters, ULID-shaped,
/// byte-stable across runs.
pub const BENCH_SESSION: &str = "01JBENCH000000000000000000";

/// The capture opt-in control file (`rift_shadow::inclusion::INCLUDE_FILE`).
///
/// Spelled once here rather than at each use: the harness drives the product
/// as a black box, so a rename in the product must break this constant's one
/// definition and not three string literals in three arms.
pub const INCLUDE_FILE: &str = ".worktreeinclude";

/// The fixture every hook row runs against: hot-path latency, where repository
/// size must not dominate.
pub const HOOK_FIXTURE: &str = "small";

/// The branch every generated fixture is on (`init.defaultBranch` in the
/// hermetic git config), which is the branch `session resume` is asked for.
pub const FIXTURE_BRANCH: &str = "main";

/// Capture's opt-in for an inline archive: with it set, the post-commit hook
/// condenses in-process instead of detaching a worker, so a prelude's commit
/// has landed its checkpoint shard by the time the timed verb runs. The env
/// name is the product's (`bins/rift/src/hooks.rs`), spelled once here.
pub const CAPTURE_SYNC_ENV: &str = "RIFT_CAPTURE_SYNC";

/// A distinct session per iteration, for verbs that can only happen once in a
/// session's life — its first hook, its last.
#[must_use]
pub fn iteration_session(iteration: usize) -> String {
    // 8 + 18 = the 26 characters a ULID-shaped id has.
    format!("01JBENCH{:018}", iteration % 1_000_000_000_000_000_000)
}

/// One scripted hook event: the verb a leg is invoked with, plus the payload
/// body the agent would have sent with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    /// The session's first hook.
    SessionStart,
    /// A user prompt — opens a turn.
    PromptSubmit,
    /// `PreToolUse` for a shell command: no worktree read.
    ToolCallBash,
    /// `PreToolUse` for a file read: worktree hashing plus tokenization.
    ToolCallRead,
    /// `PostToolUse` — the tool's result.
    ToolResult,
    /// `PostToolUse` for a file read: the event the read observation lands on.
    ///
    /// The bridge records a `RecordedRead` only from `EventKind::ToolResult`
    /// (`rift-shadow/src/bridge/mod.rs`), so a prelude that wants a serve to find
    /// a sibling's read must deliver the read's *result*, not just its
    /// `PreToolUse` — which is how Claude Code sends it, and how the landed
    /// compounding fixture (`bins/rift/tests/family_compounding.rs::agent_read`)
    /// delivers it.
    ToolReadResult,
    /// End of turn.
    Stop,
    /// End of session.
    SessionEnd,
    /// `PreToolUse` for a file read named **absolutely**, which is the only
    /// spelling the window journal stands behind without a stated working
    /// directory — and therefore the only one the gate decides about.
    ///
    /// Delivered first, so nothing about it is a repeat.
    ToolCallReadAbs,
    /// The same read again, in a payload of its own.
    ///
    /// This is the call the gate really pays for: the window already holds
    /// these lines at this revision, so the whole decision runs — hash,
    /// journal, predicate, the reason the model is answered with, and the
    /// accounting journalled for the detached worker. The payload differs
    /// from [`Self::ToolCallReadAbs`] in the field a live session varies, so
    /// the two are two events rather than a replay of one.
    ToolCallReadRepeat,
    /// `PostToolUse` for the absolute read: the result the projection records
    /// a read fact from.
    ToolReadAbsResult,
    /// `PostToolUse` for an edit: the file is hashed and what the window now
    /// holds is worked out from the agent's own payload.
    ToolEditRecord,
}

/// The file every edit row changes, and the body it is left holding.
///
/// One path, rewritten per iteration rather than one path per iteration: a
/// row under `Isolation::Shared` that planted a new file each time would be
/// timing a bigger work tree at iteration thirty than at iteration one.
pub const EDIT_TARGET: &str = "bench/edit_target.txt";

/// What [`EDIT_TARGET`] holds after the `nth` edit.
#[must_use]
pub fn edit_body(nth: usize) -> String {
    format!("alpha\nedit {nth}\nomega\n")
}

/// The policy file a repository writes to ask for declines, and the tables it
/// asks with — the operator's own spelling, so what the row measures is the
/// route from a file on disk to a decision on stdout.
///
/// The line floor is written at one on purpose. The `small` fixture's files
/// are forty-one lines today, comfortably over the shipped floor, so the row
/// would decline either way — but this row times the decline path and its
/// probe fails the measure on anything but a decline, and neither the fixture
/// generator's line count nor the shipped floor is this row's to depend on.
/// Pinning the floor here keeps the row timing the arm it is named for however
/// those two numbers move.
pub const ENFORCE_POLICY: &str = "[interception]\nmode = \"enforce\"\nmin_slice_lines = 1\n\n\
     [interception.agents]\nclaude-code = \"enforce\"\n";

/// Where that file lives in a work tree.
#[must_use]
pub fn serve_policy_path(repo: &Path) -> PathBuf {
    repo.join(".rift").join("policies").join("serve.toml")
}

impl HookEvent {
    /// Every scripted event, for the tests that quantify over the set.
    #[cfg(test)]
    pub(crate) const fn all_for_test() -> [Self; 12] {
        [
            Self::SessionStart,
            Self::PromptSubmit,
            Self::ToolCallBash,
            Self::ToolCallRead,
            Self::ToolResult,
            Self::ToolReadResult,
            Self::Stop,
            Self::SessionEnd,
            Self::ToolCallReadAbs,
            Self::ToolCallReadRepeat,
            Self::ToolReadAbsResult,
            Self::ToolEditRecord,
        ]
    }

    /// The hook verb passed on the command line.
    #[must_use]
    pub const fn verb(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::PromptSubmit => "UserPromptSubmit",
            Self::ToolCallBash
            | Self::ToolCallRead
            | Self::ToolCallReadAbs
            | Self::ToolCallReadRepeat => "PreToolUse",
            Self::ToolResult
            | Self::ToolReadResult
            | Self::ToolReadAbsResult
            | Self::ToolEditRecord => "PostToolUse",
            Self::Stop => "Stop",
            Self::SessionEnd => "SessionEnd",
        }
    }

    /// Whether this verb's payload names the transcript. Every verb does: the
    /// session's first hook seeds the usage-settle offset at the transcript's
    /// current length, a mid-turn hook samples context from its tail, and any
    /// later hook may settle the turn that closed before it — so a payload
    /// without the address would measure a hook that cannot do its work.
    #[must_use]
    pub const fn carries_transcript(self) -> bool {
        let _ = self;
        true
    }

    /// The JSON body an agent would put on stdin for the `nth` such event.
    ///
    /// `nth` is what makes consecutive events **distinct**, and that is not
    /// cosmetic. A canonical event's identity is
    /// `sha256(agent ‖ session ‖ verb ‖ turn ‖ ordinal ‖ payload)` — deliberately
    /// clock-free, so re-delivering a byte-identical payload in the same turn is
    /// a *replay* and ingest is a true no-op. Timing that would measure the
    /// dedup short-circuit while calling it a hook round-trip. Each verb varies
    /// in the field a live session varies: the prompt text, the command, the
    /// file being read.
    #[must_use]
    pub fn payload(
        self,
        repo: &Path,
        session: &str,
        nth: usize,
        transcript: Option<&Path>,
    ) -> String {
        let mut body = serde_json::Map::new();
        let mut put = |k: &str, v: serde_json::Value| {
            body.insert(k.to_owned(), v);
        };
        put("session_id", session.into());
        put("hook_event_name", self.verb().into());
        for (key, value) in self.event_fields(repo, nth) {
            put(key, value);
        }
        if let (Some(path), true) = (transcript, self.carries_transcript()) {
            put("transcript_path", path.display().to_string().into());
        }
        serde_json::Value::Object(body).to_string()
    }

    /// The fields this verb alone contributes, in the order it contributes
    /// them: everything outside the envelope every payload carries.
    fn event_fields(self, repo: &Path, nth: usize) -> Vec<(&'static str, serde_json::Value)> {
        match self {
            Self::SessionStart => vec![("source", "startup".into())],
            Self::PromptSubmit => vec![(
                "prompt",
                format!("add test case {nth} for the parser").into(),
            )],
            Self::ToolCallBash | Self::ToolResult => bash_fields(self, nth),
            Self::ToolCallRead | Self::ToolReadResult => relative_read_fields(self, nth),
            Self::Stop => vec![("stop_hook_active", false.into())],
            Self::SessionEnd => vec![("reason", "clear".into())],
            Self::ToolCallReadAbs | Self::ToolCallReadRepeat | Self::ToolReadAbsResult => {
                absolute_read_fields(self, repo, nth)
            }
            Self::ToolEditRecord => edit_fields(repo, nth),
        }
    }

    /// The invocation that delivers the `nth` such event.
    #[must_use]
    pub fn invocation(self, ctx: &PlanCtx<'_>, session: &str, nth: usize) -> Invocation {
        Invocation::new(ctx.binary, ctx.repo)
            .args(["hooks", "claude-code", self.verb()])
            .stdin(self.payload(ctx.repo, session, nth, Some(&ctx.transcript_path(session))))
    }
}

/// A shell command and, for the result event, the output it came back with.
fn bash_fields(event: HookEvent, nth: usize) -> Vec<(&'static str, serde_json::Value)> {
    let mut fields = vec![
        ("tool_name", "Bash".into()),
        (
            "tool_input",
            serde_json::json!({ "command": format!("cargo test --workspace case_{nth}") }),
        ),
    ];
    if event == HookEvent::ToolResult {
        fields.push((
            "tool_response",
            serde_json::json!({ "stdout": "test result: ok. 12 passed" }),
        ));
    }
    fields
}

/// A read named the way an agent names one by default: repo-relative, with a
/// page offset.
///
/// A live session reads different files, and pages within them: both are real
/// `Read` parameters, and together they keep every iteration a genuinely new
/// event.
fn relative_read_fields(event: HookEvent, nth: usize) -> Vec<(&'static str, serde_json::Value)> {
    let mut fields = vec![
        ("tool_name", "Read".into()),
        (
            "tool_input",
            serde_json::json!({ "file_path": read_target(nth), "offset": nth }),
        ),
    ];
    if event == HookEvent::ToolReadResult {
        fields.push((
            "tool_response",
            serde_json::json!({ "file": { "filePath": read_target(nth) } }),
        ));
    }
    fields
}

/// A read named **absolutely**, which is the only spelling the window journal
/// stands behind without a stated working directory.
///
/// Both the first read and the repeat ask for the whole file — every `limit`
/// here is past any fixture file's length, and a read is clamped to the lines
/// the file has — so the two resolve to the same run of the same file, which is
/// what makes the second one a repeat. They differ in the field itself, and
/// each varies with the iteration, so no two calls in a run are the same event.
fn absolute_read_fields(
    event: HookEvent,
    repo: &Path,
    nth: usize,
) -> Vec<(&'static str, serde_json::Value)> {
    let file = repo.join(read_target(nth)).display().to_string();
    let limit = match event {
        HookEvent::ToolCallReadRepeat => 200_000 + nth,
        _ => 100_000 + nth,
    };
    let mut fields = vec![
        ("tool_name", "Read".into()),
        (
            "tool_input",
            serde_json::json!({ "file_path": file, "offset": 1, "limit": limit }),
        ),
    ];
    if event == HookEvent::ToolReadAbsResult {
        fields.push((
            "tool_response",
            serde_json::json!({ "file": { "filePath": repo.join(read_target(nth)).display().to_string() } }),
        ));
    }
    fields
}

/// An edit, with the sites a later read is decided from spelled in the agent's
/// own payload.
fn edit_fields(repo: &Path, nth: usize) -> Vec<(&'static str, serde_json::Value)> {
    vec![
        ("tool_name", "Edit".into()),
        (
            "tool_input",
            serde_json::json!({
                "file_path": repo.join(EDIT_TARGET).display().to_string(),
                "old_string": "alpha\n",
                "new_string": format!("alpha\nedit {nth}\n"),
                "replace_all": false,
            }),
        ),
        (
            "tool_response",
            serde_json::json!({ "filePath": repo.join(EDIT_TARGET).display().to_string() }),
        ),
    ]
}

/// The repo-relative file the `nth` read-shaped tool call names: the generated
/// fixtures' own files, cycled, so the path always resolves and the read path
/// really hashes a blob.
#[must_use]
pub fn read_target(nth: usize) -> String {
    let spec = crate::fixture::FixtureSpec::preset(HOOK_FIXTURE)
        .unwrap_or_else(crate::fixture::FixtureSpec::small);
    let total = spec.total_files().max(1);
    crate::fixture::generated_path(&spec, nth % total)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::leg::LegId;
    use crate::scenario::tests::*;
    use crate::scenario::*;

    #[test]
    fn the_hook_payload_is_the_native_claude_code_shape() {
        let value: serde_json::Value = serde_json::from_str(HOOK_PAYLOAD).unwrap();
        assert!(value["session_id"].is_string());
        assert_eq!(value["hook_event_name"], "PreToolUse");
        let Plan::Run(inv) = plan(ScenarioId::HookRoundTrip, &probe(LegId::Rift)) else {
            panic!("hook round-trip is wired")
        };
        assert_eq!(inv.args, ["hooks", "claude-code", "PreToolUse"]);
        // The baseline payload, key for key: `hook-round-trip` numbers stay
        // comparable with the first recorded run. Only `transcript_path` is
        // added, and it is inert until a transcript exists on disk.
        let mut built = payload_of(&inv);
        assert!(built
            .as_object_mut()
            .unwrap()
            .remove("transcript_path")
            .is_some());
        assert_eq!(built, value);
    }

    #[test]
    fn a_read_shaped_call_names_a_file_the_fixtures_actually_contain() {
        // If this drifts, the read path resolves to no fact and the measure stops
        // covering worktree hashing without failing.
        let spec = crate::fixture::FixtureSpec::preset(HOOK_FIXTURE).unwrap();
        for nth in [0, 1, 11, 12, 97] {
            let target = read_target(nth);
            let expected = crate::fixture::generated_path(&spec, nth % spec.total_files())
                .display()
                .to_string();
            assert_eq!(target, expected, "nth={nth}");
        }
        assert_eq!(read_target(0), "src/mod_0000/file_0000.rs");
        let Plan::Run(inv) = plan(ScenarioId::HookReadTool, &probe(LegId::Rift)) else {
            panic!("wired")
        };
        assert_eq!(payload_of(&inv)["tool_input"]["file_path"], read_target(0));
        assert_eq!(payload_of(&inv)["tool_name"], "Read");
    }

    #[test]
    fn consecutive_iterations_deliver_genuinely_new_events() {
        // The load-bearing property of the whole hook set. A canonical event's
        // identity is clock-free — same session, same turn, same payload bytes
        // means *replay*, and ingest is a designed no-op. A row that repeats its
        // payload therefore times the dedup short-circuit while claiming to time
        // a round-trip. (This is exactly what the first recorded
        // `hook-round-trip` baseline did: 33 identical payloads, one ingest.)
        for id in ScenarioId::hooks() {
            let s = spec(id);
            let event = s.hook_event.unwrap();
            let mut seen = std::collections::BTreeSet::new();
            for iteration in 0..64 {
                let ctx = PlanCtx {
                    iteration,
                    ..probe(LegId::Rift)
                };
                let session = ctx.session_for(&s);
                let nth = s.nth_event(iteration);
                seen.insert(event.payload(ctx.repo, &session, nth, None));
            }
            match s.delivery {
                // An event is new when its payload differs — or when its prelude
                // has opened a new turn, since the turn ordinal is part of the
                // identity too. `hook-stop` is the second kind: every Stop is
                // byte-identical and every one closes a different turn.
                Delivery::Distinct => assert!(
                    seen.len() == 64 || opens_a_turn(id),
                    "{} delivers the same event twice",
                    id.as_str()
                ),
                Delivery::Replay => assert_eq!(seen.len(), 1, "{} varies", id.as_str()),
            }
            assert_eq!(s.requires_ingest(), s.delivery == Delivery::Distinct);
        }
    }
}
