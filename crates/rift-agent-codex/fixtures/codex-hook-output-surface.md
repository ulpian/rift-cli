# PINNED EVIDENCE ARTIFACT — Codex's hook **output** contract

This file is a **fixture**, not documentation. The verb pin beside it
(`codex-hook-surface.rs`) settled *which hooks exist*; this one settles *what
Rift is allowed to write back on them*. The standing rule that a supported
agent's own published surface is the authority — admitted only once it is
pinned into this tree with repository, tag, path, blob sha and a verbatim
excerpt — is extended here from the verb table to the response shapes. The
capability rows at the end are asserted against the code table by
`pinned_output_surface` in `crates/rift-agent-codex/src/lib.rs`.

**What this pin overturned.** Codex's response contract had been carried at
second hand: a deny was believed to be the top-level `decision: "block"` with
`reason`, and a per-payload `additionalContextLimit` was believed to bound an
injection. The generated schemas and the event handler at the newest stable tag
say otherwise on both counts, and each correction is quoted below rather than
argued. This is the second time a Codex row moved on a secondary source, which
is why the rule exists.

Nothing here is executed, linked or shipped. It is quoted for evidence under
the standing rule for any consulted source: consult behaviour, never copy code.

## Artifact A — the generated `PreToolUse` output schema

    repository:     https://github.com/openai/codex
    tag:            rust-v0.152.1
    path:           codex-rs/hooks/schema/generated/pre-tool-use.command.output.schema.json
    blob-sha:       6730b27fd4fc80f8075d64346a554a1cfc94470a
    retrieved:      2026-09-03
    retrieved-from: https://raw.githubusercontent.com/openai/codex/rust-v0.152.1/codex-rs/hooks/schema/generated/pre-tool-use.command.output.schema.json

Re-verify in two commands, with no judgement involved: fetch the
`retrieved-from` URL above to a scratch file, then

    git hash-object <that file>   # must print the blob-sha above

`rust-v0.152.1` is the newest release the repository marks stable, published
2026-09-01. The schema is generated from the wire types, and it declares
`additionalProperties: false` at both levels, so it is a **closed**
enumeration: a field it does not list is a field the client rejects.

### The response object, verbatim (upstream lines 1-50)

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "additionalProperties": false,
  "definitions": {
    "PreToolUseDecisionWire": {
      "enum": [
        "approve",
        "block"
      ],
      "type": "string"
    },
    "PreToolUseHookSpecificOutputWire": {
      "additionalProperties": false,
      "properties": {
        "additionalContext": {
          "default": null,
          "type": "string"
        },
        "hookEventName": {
          "const": "PreToolUse",
          "type": "string"
        },
        "permissionDecision": {
          "allOf": [
            {
              "$ref": "#/definitions/PreToolUsePermissionDecisionWire"
            }
          ],
          "default": null
        },
        "permissionDecisionReason": {
          "default": null,
          "type": "string"
        },
        "updatedInput": {
          "default": null
        }
      },
      "required": [
        "hookEventName"
      ],
      "type": "object"
    },
    "PreToolUsePermissionDecisionWire": {
      "enum": [
        "allow",
        "deny",
        "ask"
      ],
      "type": "string"
```

### The top-level decision fields, verbatim (upstream lines 51-80)

```json
    }
  },
  "properties": {
    "continue": {
      "default": true,
      "type": "boolean"
    },
    "decision": {
      "allOf": [
        {
          "$ref": "#/definitions/PreToolUseDecisionWire"
        }
      ],
      "default": null
    },
    "hookSpecificOutput": {
      "allOf": [
        {
          "$ref": "#/definitions/PreToolUseHookSpecificOutputWire"
        }
      ],
      "default": null
    },
    "reason": {
      "default": null,
      "type": "string"
    },
    "stopReason": {
      "default": null,
      "type": "string"
```

So two routes are declared, not one: `hookSpecificOutput.permissionDecision`
with `permissionDecisionReason`, and a top-level `decision` of `approve` or
`block` with `reason`. Artifact D decides which of the two Rift writes.

`updatedInput` is declared here, and artifact D shows the handler applying it.
Rift still does not use it on this family: input substitution is kept to the
one family whose reference page also documents a reason reaching the model, so
a substitution that silently fails to apply has a documented fallback rather
than none. The field is quoted because pinning half a schema is how a later
reader mistakes an abstention for an oversight.

## Artifact B — the generated `UserPromptSubmit` output schema

    repository:     https://github.com/openai/codex
    tag:            rust-v0.152.1
    path:           codex-rs/hooks/schema/generated/user-prompt-submit.command.output.schema.json
    blob-sha:       97875c108b9345bbe5518bb1968ce12c67db7484
    retrieved:      2026-09-03
    retrieved-from: https://raw.githubusercontent.com/openai/codex/rust-v0.152.1/codex-rs/hooks/schema/generated/user-prompt-submit.command.output.schema.json

### The context object, verbatim (upstream lines 11-26)

```json
    "UserPromptSubmitHookSpecificOutputWire": {
      "additionalProperties": false,
      "properties": {
        "additionalContext": {
          "default": null,
          "type": "string"
        },
        "hookEventName": {
          "const": "UserPromptSubmit",
          "type": "string"
        }
      },
      "required": [
        "hookEventName"
      ],
      "type": "object"
```

## Artifact C — the generated `SessionStart` output schema

    repository:     https://github.com/openai/codex
    tag:            rust-v0.152.1
    path:           codex-rs/hooks/schema/generated/session-start.command.output.schema.json
    blob-sha:       1dd51e001030cb6b260114da3461cbe9d77ba944
    retrieved:      2026-09-03
    retrieved-from: https://raw.githubusercontent.com/openai/codex/rust-v0.152.1/codex-rs/hooks/schema/generated/session-start.command.output.schema.json

### The context object, verbatim (upstream lines 5-20)

```json
    "SessionStartHookSpecificOutputWire": {
      "additionalProperties": false,
      "properties": {
        "additionalContext": {
          "default": null,
          "type": "string"
        },
        "hookEventName": {
          "const": "SessionStart",
          "type": "string"
        }
      },
      "required": [
        "hookEventName"
      ],
      "type": "object"
```

Neither boundary schema declares a limit field of any kind. See artifact E for
where the limit that does exist actually lives.

## Artifact D — the `PreToolUse` handler, and which decisions it honours

    repository:     https://github.com/openai/codex
    tag:            rust-v0.152.1
    path:           codex-rs/hooks/src/events/pre_tool_use.rs
    blob-sha:       137731357a5f37278d164e9a16e95375d99c024c
    retrieved:      2026-09-03
    retrieved-from: https://raw.githubusercontent.com/openai/codex/rust-v0.152.1/codex-rs/hooks/src/events/pre_tool_use.rs

A schema says what parses; this file says what *acts*. Its own tests are a
closed statement of the four cases, and they are quoted at their upstream
ranges. The test names are upstream's, not Rift's.

### `permissionDecision: "deny"` blocks, and carries the reason (upstream lines 356-366, 375-384)

```rust
    #[test]
    fn permission_decision_deny_blocks_processing() {
        let parsed = parse_completed(
            &handler(),
            run_result(
                Some(0),
                r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"do not run that"}}"#,
                "",
            ),
            Some("turn-1".to_string()),
        );
```

```rust
            }
        );
        assert_eq!(parsed.completed.run.status, HookRunStatus::Blocked);
        assert_eq!(
            parsed.completed.run.entries,
            vec![HookOutputEntry {
                kind: HookOutputEntryKind::Feedback,
                text: "do not run that".to_string(),
            }]
        );
```

### `"ask"` is refused, and an `"allow"` that amends nothing is refused too (upstream lines 441-443, 459-466, 555-561, 576-583)

This heading read "allow and ask are refused as unsupported" until the decline
run recorded below made the distinction matter, and it overstated its own
excerpts. What the first test refuses is named in the test itself: an
`"allow"` carrying **no** amended input. So the enum names three decisions and
these excerpts settle two of them — `"ask"` is refused outright, and an
`"allow"` is refused where it amends nothing. What an `"allow"` that does
carry an amendment is answered with is not quoted here, and is therefore not a
fact this tree holds; nothing Rift writes on this family carries one, for the
reason given under artifact A.

The consequence for Rift is unchanged either way: a neutral acknowledgement on
this family is **silence**, never an `"allow"`, because writing the bare one
would put an error entry in the user's transcript on every tool call Rift
observes.

```rust
    #[test]
    fn permission_decision_allow_without_updated_input_fails_open() {
        let stdout = r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","additionalContext":"preserved"}}"#;
```

```rust
        assert_eq!(parsed.completed.run.status, HookRunStatus::Failed);
        assert_eq!(
            parsed.completed.run.entries,
            vec![HookOutputEntry {
                kind: HookOutputEntryKind::Error,
                text: "PreToolUse hook returned unsupported permissionDecision:allow".to_string(),
            }]
        );

    #[test]
    fn unsupported_permission_decision_fails_open() {
        let parsed = parse_completed(
            &handler(),
            run_result(
                Some(0),
                r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask","permissionDecisionReason":"please confirm"}}"#,
```

```rust
        assert_eq!(parsed.completed.run.status, HookRunStatus::Failed);
        assert_eq!(
            parsed.completed.run.entries,
            vec![HookOutputEntry {
                kind: HookOutputEntryKind::Error,
                text: "PreToolUse hook returned unsupported permissionDecision:ask".to_string(),
            }]
        );
```

### The top-level `decision: "block"` route still blocks, and upstream calls it deprecated (upstream lines 484-493, 505-512)

```rust
    #[test]
    fn deprecated_block_decision_blocks_processing() {
        let parsed = parse_completed(
            &handler(),
            run_result(
                Some(0),
                r#"{"decision":"block","reason":"do not run that"}"#,
                "",
            ),
            Some("turn-1".to_string()),
```

```rust
        assert_eq!(parsed.completed.run.status, HookRunStatus::Blocked);
        assert_eq!(
            parsed.completed.run.entries,
            vec![HookOutputEntry {
                kind: HookOutputEntryKind::Feedback,
                text: "do not run that".to_string(),
            }]
        );
```

Both routes block. Rift's row names the `permissionDecision` pair because that
is the pair upstream's own naming keeps current, and because it carries the
reason in a field of its own rather than in a top-level one shared with other
events.

## Artifact E — where `additionalContextLimit` actually lives

    repository:     https://github.com/openai/codex
    tag:            rust-v0.152.1
    path:           codex-rs/config/src/hook_config.rs
    blob-sha:       7575fa5fcb1042cc380812308f62b8378540ad2a
    retrieved:      2026-09-03
    retrieved-from: https://raw.githubusercontent.com/openai/codex/rust-v0.152.1/codex-rs/config/src/hook_config.rs

It is not a field of any payload and not a field of any response. It is a key
on the hook handler's own configuration entry — a value the hook's author
writes once, next to the command, and the client honours thereafter. So the
boundary text is bounded by a number Rift itself declares when it installs its
hooks, not by one it reads at run time.

### The handler configuration entry, verbatim (upstream lines 163-185)

```rust
pub enum HookHandlerConfig {
    #[serde(rename = "command")]
    Command {
        command: String,
        #[serde(default, rename = "commandWindows", alias = "command_windows")]
        command_windows: Option<String>,
        #[serde(default, rename = "timeout")]
        timeout_sec: Option<u64>,
        #[serde(default)]
        r#async: bool,
        #[serde(default, rename = "statusMessage")]
        status_message: Option<String>,
        /// Approximate token threshold for spilling this hook's `additionalContext` to disk.
        /// Unset uses 2,500 tokens; `0` disables spilling for this hook. The threshold is
        /// evaluated against the original context; a spilled preview also includes recovery
        /// metadata.
        #[serde(
            default,
            rename = "additionalContextLimit",
            skip_serializing_if = "Option::is_none"
        )]
        additional_context_limit: Option<usize>,
    },
```

## Recorded observation — the installed client

    observed:  2026-09-03 on this machine
    command:   codex --version
    output:    codex-cli 0.25.0

The pin is at the repository's newest stable tag; the client installed on that
day reported a different version line, older than the tag and without the
surface at all. Nothing was emitted on this family until a recorded run
confirmed the reason reaches the agent on a client that actually has it, so the
gap was a fact to close rather than a blocker, and the two observations that
closed it follow.

## Recorded observation — the confirmation that could not be attempted

    observed:  2026-09-03 on this machine
    client:    codex-cli 0.25.0, older than the tag the artifacts above pin
    result:    no hook surface — the binary's own strings contain no occurrence
               of the handler event names it would have to parse, and the
               command has no subcommand or configuration key for handlers,
               while control strings of the same kind are present
    run:       a scratch repository outside this tree with its own handler file
               reached the provider and failed there, and the handler's log
               file was never created, so no handler ran

There was nothing here to promote the pinned schema with, and while this was
the newest observation the row below stayed unconfirmed and this family was
never declined — a schema is what a response would look like, and a run is what
a model would read. The run arrived a day later, on an upgraded client, and is
recorded below in its own block. This one is kept because a row nobody could
try to confirm and a row a client refused are different states, and because the
reading that "the family has no hook surface" was true of a client rather than
of the family.

## Recorded observation — the boundary channel on a client that has the surface

    observed:  2026-09-04 on this machine
    client:    codex-cli 0.153.1 (npm @openai/codex 0.153.1)
    model:     gpt-5.4-mini
    runs:      3, in a scratch repository outside this tree, hooks installed
               only in that repository's own `.codex/hooks.json`, hook trust
               supplied per invocation by the client's own documented bypass
               flag so that nothing was persisted
    result:    the text written on `UserPromptSubmit`'s `additionalContext`
               reached the MODEL in 3 of 3 runs, arriving ahead of the user's
               turn as a message in the `developer` role; each run quoted its
               marker back verbatim

    with `additionalContextLimit: 500` on that handler and an injection of
    27,644 bytes (6,911 tokens by the client's own count), the message the model
    received was 2,101 characters — a head and a tail of the text, with the
    interior gone — opening

        Warning: truncated output (original token count: 6911)

    and closing with a line naming a file under the client's temporary
    directory where the whole output had been saved.

This is artifact E's doc comment observed rather than inferred, and it is the
reason the boundary text is fitted before it is written: past the limit the
model reads a preview of a frame plus a path it will not open, which is worse
than a shorter frame that is whole. The limit Rift declares for its own
handlers is `rift_agent::respond::DECLARED_CONTEXT_LIMIT_TOKENS`, and the
installer writes that number into the two handlers whose rows below name the
key.

The earlier observation stands as recorded: the client installed on 2026-09-03
was 0.25.0 and had no hook surface at all. Both are kept, because "the schema
was never exercised" and "the schema was exercised on a client that has it" are
different states and the second does not erase the first.

## Recorded observation — the decline channel on the client that has the surface

    observed:  2026-09-04 on this machine
    client:    codex-cli 0.153.1 (npm @openai/codex 0.153.1)
    model:     gpt-5.4-mini
    runs:      12 declining runs, in the same scratch repository outside this
               tree, hooks installed only in that repository's own
               `.codex/hooks.json`, hook trust supplied per invocation by the
               client's own documented bypass flag so that nothing was persisted
    payload:   a shell read arrives as `tool_name: "Bash"` with
               `tool_input.command` a single string, exactly as the tool-field
               pin beside this file states
    result:    with the supported pair (10 runs), the second read of the same
               lines was declined 10 times of 10 and the reason reached the
               MODEL 10 times of 10, arriving synchronously as the declined
               call's own `function_call_output`; with the deprecated
               top-level route (2 runs) the client still blocks and delivers
               the same text

The client does not hand the reason over as it was written. It **wraps** it,
and the wrapper is the whole of what the model reads — identical in 12 runs of
12:

    Command blocked by PreToolUse hook: [rift] Read skipped: … Command: cat alpha.txt

A fixed prefix `Command blocked by PreToolUse hook: ` precedes Rift's frame and
a `. Command: <the command line>` suffix follows it. Rift's own bytes are intact
and are never the whole message, so an assertion that this family's model sees
*exactly* the frame would be false; what this tree asserts of the wire is the
object it writes, and what it asserts of the model is containment.

The client also refuses a decline that says nothing, in as many words:

    PreToolUse hook returned permissionDecision:deny without a non-empty permissionDecisionReason

so a reason is mandatory on this route rather than merely advisable — the same
rule the composer already applies to its own reasons.

Two numbers this observation deliberately does **not** promote. Every one of
the 10 runs re-issued the identical command the moment it was declined, which
the one-decline-per-key rule allowed: a measured retry after a decline of 10 of
10. And a declined call produces no item at all in the client's `--json` event
stream — it appears only in the rollout transcript. Retry rate remains an
efficiency signal and never changes support or policy mode; the bounded raw
retry is the safety escape. The second fact remains relevant to anyone reading
these runs back.

## Recorded observation — `updatedInput` sibling ordering

    observed:  2026-09-05 on this machine
    client:    codex-cli 0.153.1 (npm @openai/codex 0.153.1)
    model:     gpt-5.6-sol
    runs:      2, in a scratch repository outside this tree, with two
               PreToolUse command hooks and hook trust supplied only by the
               client's documented per-invocation bypass flag
    request:   one shell command whose input was `printf original > result.txt`
    hook one:  records stdin, returns allow + updatedInput.command writing
               `hook-one`
    hook two:  records stdin, returns allow + updatedInput.command writing
               `hook-two`
    result A:  order [one, two] executed the hook-one command
    result B:  order [two, one] executed the hook-two command
    inputs:    in both orders, both hooks received the original command

On this client, sibling hooks do not form a rewrite pipeline: every sibling
sees the original input and the first configured `updatedInput` wins. That is a
deterministic rule Rift can use only if its PreToolUse group is installed first.
The installer therefore preserves every unrelated group and its relative order
but places Rift's group before them for this event. Only a strict recognized
shell read may use the capability; a parse ambiguity still runs raw, and the
confirmed deny channel remains the fallback when no replacement plan exists.

## Capability rows

Each row states one channel Rift may answer on, in the form the code table
carries it. A row exists only because an excerpt above pins it; the
`pinned_output_surface` tests assert this block and the code table are the same
set, in both directions, so neither can grow alone.

`confirmed=true` on the deny row rests on the run recorded above and on
nothing else: the schema and the handler were pinned first, and the flag moved
only once a client with the surface had shown a model reading the reason. It
licenses exactly one thing — that a decline here is a decision the model can
act on rather than an unexplained tool failure. Retry measurements are
telemetry, not a support-tier or mode gate: supported repositories default to
active enforcement and retain one identical raw retry. Both earlier states are
kept above rather than overwritten, because "the schema was never exercised",
"the attempt was made and the client had no surface" and "the run happened"
are three different facts and the last does not erase the first two.

```rift-capability-rows
verb=PreToolUse channel=DenyWithReason reason_field=permissionDecisionReason confirmed=true
verb=PreToolUse channel=UpdatedInput forms=shell_read_replacement
verb=UserPromptSubmit channel=AdditionalContext limit_field=additionalContextLimit
verb=SessionStart channel=AdditionalContext limit_field=additionalContextLimit
verb=PreCompact channel=Neutral
verb=PostCompact channel=Neutral
```

`limit_field` names artifact E's configuration key rather than a payload field,
because that is where the bound is set; the boundary text is fitted to the
smaller of Rift's own budget and the number Rift wrote there.

The two compaction rows carry no channel. They are the eviction signal, and
naming them keeps a later reader from reading their silence as an omission.
