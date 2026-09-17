# PINNED EVIDENCE ARTIFACT — Claude Code's own published hook surface

This file is a **fixture**, not documentation. An owner decision makes a
supported agent's OWN published hook surface the authority for its verb table,
and admits that authority only once the surface is pinned into this tree as a
committed artifact carrying repository, tag, path, blob sha and a verbatim
excerpt. Until this file existed, no row of
`crates/rift-shadow-hooks/tests/adapter_sweep.rs` was allowed to move on Claude
Code's account and its verb table was not allowed to change.

**Two artifacts, because neither one is the whole surface.** Anthropic
publishes the Claude Code hook events in two places with two different scopes,
and pinning only one of them gives a false answer in one direction:

* the **Agent SDK** enumerates the events a program may *register* a callback
  for — it carries `SubagentStart`, `PostToolUseFailure` and
  `PermissionRequest`, and does **not** carry `SessionStart`/`SessionEnd` in
  that union;
* the **hook-development skill** enumerates the events a `settings.json` or a
  plugin `hooks.json` may key — it carries `SessionStart`, `SessionEnd` and
  `Notification`, and does **not** carry `SubagentStart`.

The published surface is the **union** of the two, and each half is recorded
with its own scope so a reader can see which artifact decided which name. Every
field below is asserted by the `pinned_surface` tests in
`crates/rift-agent-claude-code/src/lib.rs`.

## What this pin overturned

`adapter_sweep.rs`'s `claude-code.subagent_boundary` row read: *"Rift declares
`SubagentStart`, which Claude Code does not publish."* Its evidence was a
third party's Go wrapper at its pinned tag. Artifact A is Anthropic's own
type definition and it lists `SubagentStart` outright. The sentence was true of
the wrapper and false of the agent, which is the failure mode this pin exists
to stop.

## Artifact A — the Agent SDK's `HookEvent` union

    repository:     https://github.com/anthropics/claude-agent-sdk-python
    tag:            v0.2.143
    path:           src/claude_agent_sdk/types.py
    blob-sha:       308b76cb7fd928d124666c255b253c92c343f15d
    retrieved:      2026-08-24
    retrieved-from: https://raw.githubusercontent.com/anthropics/claude-agent-sdk-python/v0.2.143/src/claude_agent_sdk/types.py

Re-verify in two commands, with no judgement involved: fetch the
`retrieved-from` URL above to a scratch file, then

    git hash-object <that file>   # must print the blob-sha above

Verbatim, upstream lines 263-274:

```python
HookEvent = (
    Literal["PreToolUse"]
    | Literal["PostToolUse"]
    | Literal["PostToolUseFailure"]
    | Literal["UserPromptSubmit"]
    | Literal["Stop"]
    | Literal["SubagentStop"]
    | Literal["PreCompact"]
    | Literal["Notification"]
    | Literal["SubagentStart"]
    | Literal["PermissionRequest"]
)
```

Verbatim, upstream lines 458-461. `SessionStart` is published as a hook event
name on the output side only, which is why the union above does not carry it:

```python
class SessionStartHookSpecificOutput(TypedDict):
    """Hook-specific output for SessionStart events."""

    hookEventName: Literal["SessionStart"]
```

## Artifact B — the hook-development skill's `## Hook Events` section

    repository:     https://github.com/anthropics/claude-code
    commit:         45bdfa96ca415da92e62b6ca85a1d6e29adf3c44
    path:           plugins/plugin-dev/skills/hook-development/SKILL.md
    blob-sha:       d1c0c199c70e8cef8fee8d3b792b0a27ff33046c
    retrieved:      2026-08-24
    retrieved-from: https://raw.githubusercontent.com/anthropics/claude-code/45bdfa96ca415da92e62b6ca85a1d6e29adf3c44/plugins/plugin-dev/skills/hook-development/SKILL.md

That repository publishes no release tag, so the pin names the commit the bytes
were read at. The blob sha is content-addressed and stays reproducible from the
bytes alone whatever `main` does afterwards.

Verbatim, upstream line 3 — the skill's own frontmatter enumerates the nine
settings-configurable events in one sentence:

```markdown
description: This skill should be used when the user asks to "create a hook", "add a PreToolUse/PostToolUse/Stop hook", "validate tool use", "implement prompt-based hooks", "use ${CLAUDE_PLUGIN_ROOT}", "set up event-driven automation", "block dangerous commands", or mentions hook events (PreToolUse, PostToolUse, Stop, SubagentStop, SessionStart, SessionEnd, UserPromptSubmit, PreCompact, Notification). Provides comprehensive guidance for creating and implementing Claude Code plugin hooks with focus on advanced prompt-based hooks API.
```

The `## Hook Events` section runs from upstream line 121 to line 277 and is
closed by `## Hook Output Format` at line 278. Its headings follow, each fence
verbatim at the upstream range named above it.

Upstream lines 121-126:

```markdown
## Hook Events

### PreToolUse

Execute before any tool runs. Use to approve, deny, or modify tool calls.

```

Upstream lines 155-157:

```markdown
### PostToolUse

Execute after tool completes. Use to react to results, provide feedback, or log.
```

Upstream lines 181-183:

```markdown
### Stop

Execute when main agent considers stopping. Use to validate completeness.
```

Upstream lines 211-219:

```markdown
### SubagentStop

Execute when subagent considers stopping. Use to ensure subagent completed its task.

Similar to Stop hook, but for subagents.

### UserPromptSubmit

Execute when user submits a prompt. Use to add context, validate, or block prompts.
```

Upstream lines 238-240:

```markdown
### SessionStart

Execute when Claude Code session begins. Use to load context and set environment.
```

Upstream lines 266-277:

```markdown
### SessionEnd

Execute when session ends. Use for cleanup, logging, and state preservation.

### PreCompact

Execute before context compaction. Use to add critical information to preserve.

### Notification

Execute when Claude sends notifications. Use to react to user notifications.

```

## The parse, and what it decides

`pinned_surface::published_verbs` reads this file and takes the union of

* every `Literal["…"]` inside a fenced `python` block, and
* every `### <Event>` heading inside a fenced `markdown` block,

which is a closed enumeration of both artifacts, and therefore answers both
conformance directions this pin owes: no verb Rift declares that Claude
Code does not publish, and no verb Claude Code publishes that Rift neither
declares nor names as a deliberate non-declaration.
