# PINNED EVIDENCE ARTIFACT — Claude Code's hook **output** contract

This file is a **fixture**, not documentation. The verb pin beside it
(`claude-code-hook-surface.md`) settled *which hooks exist*; this one settles
*what Rift is allowed to write back on them*. The standing rule that a
supported agent's own published surface is the authority — admitted only once
it is pinned into this tree with repository, tag or retrieval, path, a
content-addressed digest and a verbatim excerpt — is extended here from the
verb table to the response shapes. Nothing in `rift_agent::respond` may emit a
field this file does not quote, and the capability rows at the end of this file
are asserted against the code table by `pinned_output_surface` in
`crates/rift-agent-claude-code/src/lib.rs`.

Guessing a response schema is the one mistake with a blast radius: a malformed
hook response is a broken agent, while a channel Rift declines to use is only a
missed saving. So a channel enters the table by evidence or not at all.

**Two artifacts, and each answers a different question.** The reference page is
the only place that states *who sees what* — the sentence that decides whether
a denial reason reaches the model at all. The SDK's type file is the only place
that states the response object as a **closed** union of typed members, so an
absence in it (there is no `PreCompact` member) is evidence rather than a gap in
someone's prose.

## Artifact A — the published hooks reference

    repository:     https://docs.claude.com/en/docs/claude-code/hooks
    revision:       none published; the page carries no version or revision string
    path:           /en/docs/claude-code/hooks.md
    sha256:         e2462deb9fff29ce268122aba81b65e0454b759139901c3ed62e5a816e903a0f
    retrieved:      2026-09-03
    retrieved-from: https://docs.claude.com/en/docs/claude-code/hooks.md

Re-verify in two commands, with no judgement involved: fetch the
`retrieved-from` URL above to a scratch file, then

    shasum -a 256 <that file>   # must print the sha256 above

A living page has no tag to pin, so the digest and the retrieval date are the
pin: if the bytes move, this file's excerpts and the digest disagree and the
pin is re-taken deliberately. Line numbers below are lines of the retrieved
`.md`.

### The decision fields, and who sees the reason (upstream lines 1743-1745, 1747)

The `permissionDecisionReason` row is the sentence the whole design rests on —
for `"deny"`, and only for `"deny"`, the reason reaches the model:

```markdown
| :------------------------- | :---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `permissionDecision`       | `"allow"` skips the permission prompt, except for the [actions no mode auto-approves](/docs/en/permission-modes#actions-no-mode-auto-approves) and for `AskUserQuestion` and `ExitPlanMode`, which need [`updatedInput` paired with it](#allow-with-updatedinput). `"deny"` prevents the tool call. `"ask"` prompts the user to confirm. `"defer"` exits gracefully so the tool can be resumed later. [Deny and ask rules](/docs/en/permissions#manage-permissions) are still evaluated regardless of what the hook returns |
| `permissionDecisionReason` | For `"allow"` and `"ask"`, shown to the user but not Claude. For `"deny"`, shown to Claude. For `"defer"`, ignored                                                                                                                                                                                                                                                                                                                                                                                                |
```

The row between the two quoted here, upstream line 1746, states that
`updatedInput` replaces the whole input object, so a rewrite must carry every
unchanged field alongside the one it changes, and that permission rules are
evaluated against the input the hook returns rather than the one the model
sent. It is stated rather than quoted: one word of it is a lexeme this tree
screens out of tracked text, and altering a quotation to fit is worse than
naming the line and saying what it holds.

```markdown
| `additionalContext`        | String added to Claude's context alongside the tool result. Ignored when `permissionDecision` is `"defer"`. See [Add context for Claude](#add-context-for-claude)                                                                                                                                                                                                                                                                                                                                                 |
```

### Precedence when several hooks answer (upstream lines 1749-1751)

```markdown
When multiple PreToolUse hooks return different decisions, precedence is `deny` > `defer` > `ask` > `allow`.

A hook that blocks by exiting 2 routes the same way as `"deny"`: Claude sees the stderr message as the denial reason.
```

### Hooks run in parallel (upstream line 414)

```markdown
All matching hooks run in parallel. If you define the same handler in more than one settings file, it runs once. A plugin's or skill's copy of the same handler stays separate.
```

### `updatedInput` is a `PreToolUse` field and replaces the whole input (upstream lines 1027-1032)

```markdown
A few events can also rewrite content rather than only allow or block it:

* `PreToolUse`: `updatedInput` directly under `hookSpecificOutput` replaces a tool's arguments before it runs. See [PreToolUse decision control](#pretooluse-decision-control)
* `PermissionRequest`: `updatedInput` inside the `decision` object. See [PermissionRequest decision control](#permissionrequest-decision-control)
* `PostToolUse`: `updatedToolOutput` replaces the tool's result. See [PostToolUse decision control](#posttooluse-decision-control)
* `UserPromptSubmit`: can't replace the prompt; it only injects `additionalContext` alongside it
```

### `additionalContext`: how it is delivered, and its cap (upstream lines 972, 913, 993)

The cap is why a boundary injection is fitted to a budget rather than sent
whole, and why an over-long frame degrades by dropping entries.

```markdown
The `additionalContext` field passes a string from your hook into Claude's context window. Claude Code wraps the string in a system reminder and inserts it into the conversation at the point where the hook fired. Claude reads the reminder on the next model request, but it doesn't appear as a chat message in the interface.

Hook output strings, including `additionalContext`, `systemMessage`, and plain stdout, are capped at 10,000 characters. Output that exceeds this limit is saved to a file and replaced with a preview and file path, the same way a large valid Bash result is handled under [Output limits](/docs/en/tools-reference#output-limits).

When several hooks return `additionalContext` for the same event, Claude receives all of the values. If a value exceeds 10,000 characters, Claude Code writes the full text to a file in the session directory and passes Claude the file path with a short preview instead.
```

### `PostToolUse` context, and the one result rewrite Rift takes (upstream lines 1965-1970)

Two rows are claimed below. `updatedToolOutput` is taken in one closed form,
`held_output_substitution`, for a `Bash` result only: a stretch of the result
that is a file's own lines byte for byte, which this window provably already
holds at that revision, is replaced by the one renderer's frame, and every
other byte of the result is kept as the tool wrote it. The value is the tool's
own `{stdout, stderr, interrupted, isImage}` object, because the run of
2026-10-11 (Claude Code 2.1.186) showed a string ignored with "does not match
Bash's output shape; using original output", and an accepted object becoming
the only text the transcript keeps. Nothing replaces a result unless the
`substitute_held_output` lever enforces for this family; it observes by default.

```markdown
| `decision`             | `"block"` adds the `reason` next to the tool result. Claude still sees the original output; to replace it, use `updatedToolOutput`                                                                                                                                                              |
| `reason`               | Explanation shown to Claude when `decision` is `"block"`                                                                                                                                                                                                                                        |
| `additionalContext`    | String added to Claude's context alongside the tool result. See [Add context for Claude](#add-context-for-claude)                                                                                                                                                                               |
| `classifierContext`    | Short note about this call's result for the [auto mode](/docs/en/permission-modes#eliminate-prompts-with-auto-mode) classifier rather than for Claude. See [Annotate a result for the auto mode classifier](#annotate-a-result-for-the-auto-mode-classifier). Requires Claude Code v2.1.236 or later |
| `updatedToolOutput`    | Replaces the tool's output with the provided value before it is sent to Claude. The value must match the tool's output shape                                                                                                                                                                    |
| `updatedMCPToolOutput` | Replaces the output for [MCP tools](#match-mcp-tools) only. Prefer `updatedToolOutput`, which works for all tools                                                                                                                                                                               |
```

### `SessionStart` carries the eviction signal (upstream line 1113)

```markdown
| `source`        | How the session started: `"startup"` for new sessions, `"resume"` for resumed sessions, `"clear"` after `/clear`, `"compact"` after compaction, or `"fork"` for a new session forked from an existing one     |
```

### `PreCompact`'s only documented output blocks compaction (upstream line 877)

The per-event exit-code table's `PreCompact` row is the whole of its output
contract: a hook can stop a compaction, and nothing else. There is no context
channel to answer on, so Rift answers `PreCompact` with silence and treats the
event purely as the eviction signal it is.

```markdown
| `PreCompact`          | Yes        | Blocks compaction                                                                                                                                                                                                                              |
```

## Artifact B — the Agent SDK's response types

    repository:     https://github.com/anthropics/claude-agent-sdk-python
    tag:            v0.2.152
    path:           src/claude_agent_sdk/types.py
    blob-sha:       308b76cb7fd928d124666c255b253c92c343f15d
    retrieved:      2026-09-03
    retrieved-from: https://raw.githubusercontent.com/anthropics/claude-agent-sdk-python/v0.2.152/src/claude_agent_sdk/types.py

Re-verify in two commands: fetch the `retrieved-from` URL to a scratch file,
then `git hash-object <that file>`; it must print the blob-sha above. The file
is byte-identical to the one the verb pin quotes at `v0.2.143`, which is why
the sha is the same — the surface did not move between the two tags.

### The `PreToolUse` response object (upstream lines 416-423)

```python
class PreToolUseHookSpecificOutput(TypedDict):
    """Hook-specific output for PreToolUse events."""

    hookEventName: Literal["PreToolUse"]
    permissionDecision: NotRequired[Literal["allow", "deny", "ask", "defer"]]
    permissionDecisionReason: NotRequired[str]
    updatedInput: NotRequired[dict[str, Any]]
    additionalContext: NotRequired[str]
```

### The context-only response objects (upstream lines 426-431, 451-462)

```python
class PostToolUseHookSpecificOutput(TypedDict):
    """Hook-specific output for PostToolUse events."""

    hookEventName: Literal["PostToolUse"]
    additionalContext: NotRequired[str]
    updatedToolOutput: NotRequired[Any]

class UserPromptSubmitHookSpecificOutput(TypedDict):
    """Hook-specific output for UserPromptSubmit events."""

    hookEventName: Literal["UserPromptSubmit"]
    additionalContext: NotRequired[str]


class SessionStartHookSpecificOutput(TypedDict):
    """Hook-specific output for SessionStart events."""

    hookEventName: Literal["SessionStart"]
    additionalContext: NotRequired[str]
```

### The `SubagentStart` response object (upstream lines 472-476)

The union below already names this member. Here it is in full, so the row that
claims it rests on the field rather than on the name: it is the same
`additionalContext` the two turn boundaries travel on, which is why its row
carries no limit key and no second shape.

```python
class SubagentStartHookSpecificOutput(TypedDict):
    """Hook-specific output for SubagentStart events."""

    hookEventName: Literal["SubagentStart"]
    additionalContext: NotRequired[str]
```

The matching **input** type is what makes the window separable at all (upstream
lines 383-388). `agent_id` and `agent_type` are *required* on it, while the
conversation's `session_id` arrives on `BaseHookInput` — so one payload names
both the session and the window inside it, which is exactly the pair the
window journal is already keyed by.

```python
class SubagentStartHookInput(BaseHookInput):
    """Input data for SubagentStart hook events."""

    hook_event_name: Literal["SubagentStart"]
    agent_id: str
    agent_type: str
```

### The union is closed, and `PreCompact` is not in it (upstream lines 486-494)

An enumeration is only evidence of an absence when it is closed. This one is:
every hook-specific response object the SDK defines is a member, and no
`PreCompact` member exists.

```python
HookSpecificOutput = (
    PreToolUseHookSpecificOutput
    | PostToolUseHookSpecificOutput
    | PostToolUseFailureHookSpecificOutput
    | UserPromptSubmitHookSpecificOutput
    | SessionStartHookSpecificOutput
    | NotificationHookSpecificOutput
    | SubagentStartHookSpecificOutput
    | PermissionRequestHookSpecificOutput
```

## The `updatedInput` caveat, recorded as observations rather than as a contract

Neither artifact states what happens to `updatedInput` when a second
`PreToolUse` hook matches the same call. The published record is a bug report
and its thread, so it is recorded here as observations, marked as such, and
never as a schema fact. It is the reason every rewrite Rift asks for is checked
at `PostToolUse` against the input that actually executed, instead of being
assumed to have taken effect.

    repository:     https://github.com/anthropics/claude-code
    issue:          15897
    state:          closed (not_planned) 2026-05-14
    retrieved:      2026-09-03
    retrieved-from: https://api.github.com/repos/anthropics/claude-code/issues/15897

1. The report's own title, 2025-12-31, against Claude Code 2.0.76:

```text
[BUG] updatedInput PreToolUse response does not work when multiple PreToolUse hooks are executed
```

2. A second reporter, 2026-04-04:

```text
+1 — hit this with RTK's PreToolUse rewrite hook: adding any second matching hook causes the first hook's updatedInput to be silently dropped.
```

3. A third party's re-test, 2026-06-08, after the issue was closed:

```text
This appears to be **fixed as of 2.1.168** (macOS), even though it was closed `not planned`.
```

Three observations, no vendor statement, and they disagree. That is exactly the
state in which a design must not assume: the rewrite is asked for, then
verified, and the fallback is a channel the artifacts above do pin.

## Recorded observation — the declined call, and what the model read

    observed:  2026-09-03 on this machine
    client:    Claude Code 2.1.186, model claude-sonnet-4-6
    runs:      11
    method:    a scratch repository outside this tree, one PreToolUse handler on
               the Read matcher, always exiting 0, declining the second read of
               one key with permissionDecision "deny" and a reason
    seen:      11 of 11 — the reason arrived as the declined call's own result,
               marked as an error, byte-identical to what the handler wrote,
               with no client prefix or wrapper around it
    retried:   0 of 11 — no run re-issued the declined read; every run answered
               correctly from what it already held

This is the observation the `confirmed=true` on the deny row below rests on. It
is a fact about one client version on one machine, not a contract: the page
above says the reason is shown to the model, and this says a model read it.

## Recorded observation — the only boundary a subagent has

    observed:  2026-09-15 on this machine
    client:    Claude Code 2.1.272 on Windows
    runs:      2, headless
    method:    a settings-keyed hook on the `SubagentStart` matcher, in a
               scratch directory outside this tree, logging every payload's
               key set and answering that one verb with
               `hookSpecificOutput.additionalContext` carrying a code word
    seen:      2 of 2 — the code word was `RIFT-SENTINEL-7731`, and the
               subagent, asked for any code word in its context, replied with
               it exactly; in run 2 it still held the word after a `Read` and
               a `Bash` call of its own
    absent:    2 of 2 — no `SessionStart` and no `UserPromptSubmit` fired for
               the subagent in either run

The payload key sets, as the hook logged them. Session ids are elided after
their first group, and the `agent_id` values are the client's own; a subagent's
`PreToolUse` is included because it is the same window key the window journal is
already written under.

```text
event=SessionStart     session=e166f86e-… keys=cwd,hook_event_name,scratchpad_dir,session_id,source,transcript_path
event=UserPromptSubmit session=e166f86e-… keys=cwd,hook_event_name,permission_mode,prompt,prompt_id,scratchpad_dir,session_id,transcript_path
event=PreToolUse       session=e166f86e-… tool=Agent keys=cwd,description,…,prompt_id,scratchpad_dir,session_id,subagent_type,tool_input,tool_name,tool_use_id,transcript_path
event=SubagentStart    session=e166f86e-… agent_id=a0e79504… agent_type=general-purpose keys=agent_id,agent_type,cwd,hook_event_name,prompt_id,scratchpad_dir,session_id,transcript_path
event=PreToolUse       session=173b0c74-… agent_id=a2f52a50… agent_type=general-purpose tool=Read keys=agent_id,agent_type,cwd,file_path,hook_event_name,permission_mode,prompt_id,scratchpad_dir,session_id,tool_input,tool_name,tool_use_id,transcript_path
event=SubagentStop     session=173b0c74-… agent_id=a2f52a50… agent_type=general-purpose keys=agent_id,agent_transcript_path,agent_type,background_tasks,cwd,hook_event_name,…
```

Four facts, and the artifacts above state none of them. A `settings.json` hook
on this verb **fires** (Artifact A of the verb pin lists `SubagentStart` in the
SDK's registerable union, the hook-development skill does not, and this is the
settings side answering). Its input carries the **parent's** `session_id`
alongside `agent_id` and `agent_type`, so a marker keyed by the session alone
is one marker for two windows. The text on `additionalContext` **reaches the
subagent** and survives its later tool calls. And the two verbs Rift already
answers on **never fire** inside that window, which is why nothing reached a
subagent before this row existed.

It is a fact about one client version on two runs, not a contract. The SDK's
type above says the field exists; this says a subagent read one.

## Capability rows

Each row states one channel Rift may answer on, in the form the code table
carries it. A row exists only because an excerpt above pins it; the
`pinned_output_surface` tests assert this block and the code table are the same
set, in both directions, so neither can grow alone.

`confirmed` on a deny row does not mean "documented" — artifact A documents it
plainly. It means a recorded run has shown the reason reaching the model on the
installed client. The row below carries `true` because the observation directly
above it exists, and for no other reason; a row without one carries `false`.

```rift-capability-rows
verb=PreToolUse channel=DenyWithReason reason_field=permissionDecisionReason confirmed=true
verb=PreToolUse channel=UpdatedInput forms=document_sidecar_path,grep_head_limit
verb=PostToolUse channel=AdditionalContext limit_field=none
verb=PostToolUse channel=UpdatedToolOutput forms=held_output_substitution
verb=UserPromptSubmit channel=AdditionalContext limit_field=none
verb=SessionStart channel=AdditionalContext limit_field=none
verb=SubagentStart channel=AdditionalContext limit_field=none
verb=PreCompact channel=Neutral
```

The two `UpdatedInput` forms are the whole of it: a `Read` whose path becomes a
Rift-owned Markdown sidecar, and a `Grep` given the result cap it left out.
Both add a field the caller omitted or point a read at bytes Rift produced;
neither changes what the agent asked to do, and no deny ever carries one.

## Artifact C — the hooks reference's common input fields

    repository:     https://code.claude.com/docs/en/hooks
    revision:       none published; the page carries no version or revision string
    path:           /docs/en/hooks.md
    sha256:         1ecadd63b50a0081ce6c59772d843fa62f720066e1773908f87f386871cbceca
    retrieved:      2026-10-10
    retrieved-from: https://code.claude.com/docs/en/hooks.md

The reference moved hosts after artifact A was taken; its old address answers
with a permanent redirect to this one. Re-verify exactly as for artifact A.
These excerpts are the whole basis of one link Rift makes: the call that
spawns a subagent and the subagent's opening are matched by the id of the user
prompt both are made under, plus the subagent's type, and by nothing else.

### Every hook carries the prompt being processed (upstream lines 726-733)

```text
### Common input fields

Hook events receive these fields as JSON, in addition to event-specific fields documented in each [hook event](#hook-events) section. For command hooks, this JSON arrives via stdin. For HTTP hooks, it arrives as the POST request body.

| Field | Description |
| :- | :- |
| `session_id` | Current session identifier |
| `prompt_id` | UUID identifying the user prompt currently being processed. Matches the [`prompt.id` attribute on OpenTelemetry events](/docs/en/monitoring-usage#event-correlation-attributes), so you can correlate hook output with telemetry for a single prompt. Absent until the first user input |
```

### `SubagentStart` carries those fields, and fires again on a resume (upstream lines 2484, 2490)

```text
Runs when Claude spawns a subagent with the Agent tool, when Claude [resumes a subagent](/docs/en/sub-agents#resume-subagents), and each time an in-process [agent team](/docs/en/agent-teams) teammate handles a new message. Supports matchers to filter by agent type name. For built-in agents, this is the agent name like `general-purpose`, `Explore`, or `Plan`. For [custom subagents](/docs/en/sub-agents), this is the `name` field from the agent's frontmatter, not the filename.
```

```text
In addition to the [common input fields](#common-input-fields), SubagentStart hooks receive `agent_id` with the unique identifier for the subagent and `agent_type` with the agent name that the matcher filters on.
```

Three facts, each one load-bearing. The spawning `PreToolUse` and the
subagent's `SubagentStart` both carry `prompt_id`, the prompt **currently being
processed** — the one the spawning call was made under, since a spawn is part
of processing it — so the two ends share it. The opening carries the
`agent_type` a matcher filters on, which is the type the spawning call asked
for. And the verb fires again on a resume and on each message a teammate
handles, so the opening claims once per window and a window that has opened
before claims nothing.

The pairing is exact only where one unclaimed spawn under that prompt asked
for that type; two siblings started under one prompt with one type are never
told apart, and neither inherits anything by name.

```rift-input-fields
verb=PreToolUse fields=prompt_id,tool_use_id,tool_input
verb=SubagentStart fields=prompt_id,agent_id,agent_type
```

### Observation — the pairing on recorded sessions

    observed:  2026-10-10 on this machine
    corpus:    398 subagent windows of a frozen transcript corpus, 468 of the
               live transcript directory
    method:    each window's first record's `promptId` against the `promptId`
               of the turn its spawning call was made in (named by the
               client's own sidecar), and a claim by prompt id and type in the
               order the windows opened
    seen:      398 of 398 and 467 of 468 share the spawning turn's prompt id;
               a claim names the spawning call in 395 of 398 and 462 of 468,
               names none in the rest, and names a wrong call in 0

It is a fact about one client and these sessions, not a contract; the replay
publishes the same claim count over its corpus on every run.
