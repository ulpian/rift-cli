# PINNED EVIDENCE ARTIFACT — Antigravity's hook **output** contract

This file is a **fixture**, not documentation. It settles what Rift is allowed
to write back on Antigravity's hooks, under the standing rule that a supported
agent's own published surface is the authority and is admitted only once it is
pinned into this tree with a source, a content-addressed digest and a verbatim
excerpt. The capability rows at the end are asserted against the code table by
`pinned_output_surface` in `crates/rift-agent-antigravity/src/lib.rs`.

This family is the one whose output shapes Rift already writes on every session
— a decision object is *required* on two of its verbs, so silence is not an
option here the way it is elsewhere — and until this file existed those shapes
rested on a reading no committed artifact carried.

## Artifact A — the published hooks documentation

    repository:     https://antigravity.google/docs/hooks/
    revision:       none published; the page carries no version or revision string
    path:           /docs/hooks.md
    sha256:         8b08cf1ea720762d27f89cfe2a4a4f9fa41f85530e10b83322eaed810bbc156c
    retrieved:      2026-09-03
    retrieved-from: https://antigravity.google/docs/hooks.md

Re-verify in two commands, with no judgement involved: fetch the
`retrieved-from` URL above to a scratch file, then

    shasum -a 256 <that file>   # must print the sha256 above

A living page with no version string leaves the digest and the retrieval date
as the whole of the pin. That is weaker than a tag, and it is why every row
below that could decline a tool call lands unconfirmed: a page can move without
saying so, and a recorded run is what turns a documented sentence into a fact
about the client on this machine. Line numbers are lines of the retrieved
`.md`.

### `PreToolUse`: the decision, and the sentence that leaves the reader guessing (upstream lines 195-206)

`reason` is documented as reaching "the agent or user". Which of the two is the
whole question for a declined read: a tool that fails with no explanation the
model can see is a tool the model is right to retry, so a deny on this family
waited for a recorded run rather than for a reading of the word "or". The
observation further down is that run, and it resolves the "or" to the agent.

```markdown
**Output Fields (stdout)**:

| Field | Type | Description |
| :-- | :-- | :-- |
| `decision` | string | **Required.** Controls how the tool call is gated:  
\- `"allow"`: Automatically allows the tool execution.  
\- `"deny"`: Hard blocks execution immediately.  
\- `"ask"`: Prompts the user, but respects “Always Allow” settings.  
\- `"force_ask"`: Always prompts the user, ignoring cached permissions.  
\- `"deny_unless_prior_grant"`: Denies execution unless the resource was previously approved in a prior user grant. |
| `reason` | string | **Optional.** The explanation shown to the agent or user for the decision. |
| `permissionOverrides` | array of strings | **Optional.** A list of resource strings (e.g. `["read_file(/path)", "command(args)"]`) to override default tool permissions. |
```

The output table is closed, and it holds no field that alters the call: three
fields, a decision, a reason and a permission override. So there is no input
substitution to be had here — a document read is redirected rather than
rewritten.

### `PostToolUse` answers with an empty object (upstream line 258)

```markdown
**Output Fields (stdout)**: Returns an empty JSON object `{}`.
```

### `PreInvocation`: the injection channel and its three step kinds (upstream lines 302-312, then 296-300)

`invocationNum` is on the input side of the same hook, and it is the reason
this channel is usable at all: the hook fires before *every* model invocation,
so an injection that ignored the counter would repeat itself through a whole
tool loop.

```markdown
**Output Fields (stdout)**:

| Field | Type | Description |
| :-- | :-- | :-- |
| `injectSteps` | array of objects | **Optional.** List of steps to inject into the conversation trajectory before the model is called. |

_Injected Step Schema_: Each object in the `injectSteps` array can have one of the following fields:

*   `toolCall` (object): A tool call to execute.
*   `userMessage` (string): A message from the user.
*   `ephemeralMessage` (string): A transient system message.
```

```markdown
| Field | Type | Description |
| :-- | :-- | :-- |
| `invocationNum` | integer | The 0-indexed sequence number of the current model invocation (the first invocation is 0). |
| `initialNumSteps` | integer | The number of steps currently in the trajectory. |
| _(Common Fields)_ |  | Includes `conversationId`, `workspacePaths`, `transcriptPath`, `artifactDirectoryPath`, `modelName`. |
```

### `PostInvocation`: the same steps, plus control over the loop (upstream lines 348-356)

`terminationBehavior` is quoted and not claimed. Forcing an agent's loop to
continue or to stop is a decision about the user's session that no accounting
of tokens justifies, and it is the one field on this family that could turn a
hook into a block.

```markdown
**Output Fields (stdout)**:

| Field | Type | Description |
| :-- | :-- | :-- |
| `injectSteps` | array of objects | **Optional.** List of steps to inject after the invocation completes (same schema as `PreInvocation` inject steps). |
| `terminationBehavior` | string | **Optional.** Controls the execution flow after injection:  
\- `"force_continue"`: Forces the loop to continue.  
\- `"terminate"`: Forces the loop to terminate.  
\- `""` (or omitted): Default behavior. |
```

### `Stop` requires a decision, and anything but `continue` allows the stop (upstream lines 388-393)

```markdown
**Output Fields (stdout)**:

| Field | Type | Description |
| :-- | :-- | :-- |
| `decision` | string | **Required.** Set to `"continue"` to prevent the agent from stopping and re-enter the execution loop. Any other value allows the stop. |
| `reason` | string | **Optional.** If `decision` is `"continue"`, this message is injected as a system message into the conversation. |
```

### Matchers, and the verbs that ignore them (upstream lines 76-85)

```markdown
For `PreToolUse` and `PostToolUse`, you can use a regular expression in the `matcher` field to specify which tools trigger the hook:

*   `""` or `"*"`: Match all tools.
*   `"run_command"`: Match exactly `run_command`.
*   `"run_command|view_file"`: Match either tool.
*   `"browser_.*"`: Match any tool starting with `browser_`.

Note

**Note**: For `PreInvocation`, `PostInvocation`, and `Stop`, the structure is simpler (a list of handlers directly under the event key) and the matcher is ignored.
```

### The session identity every hook receives (upstream lines 162-166)

```markdown
All hooks receive the following system metadata fields in their input payload on `stdin`:

| Field | Type | Description |
| :-- | :-- | :-- |
| `conversationId` | string | The unique UUID of the active agent conversation. |
```

## Recorded observation — the installed client

    observed:  2026-09-03 on this machine
    command:   agy --version
    output:    1.1.23

The page names no version, so the client's own version line is recorded beside
it. Both are facts about a moment, and neither is a contract; that is why the
rows below that could decline a call rest on the two observations that follow
rather than on the page alone.

## Recorded observation — the declined call, and what the model read

    observed:  2026-09-03 on this machine
    client:    agy 1.1.23, model gemini-3.6-flash-medium
    runs:      3
    method:    a scratch repository outside this tree with its own hooks file,
               every handler exiting 0, declining the second view of one file
               with decision "deny" and a reason
    seen:      3 of 3 — the client delivered it to the model as a failed tool
               call whose message is the client's own prefix ("tool call denied
               by pre-tool hook: ") followed by the reason verbatim, and the
               model quoted the reason back in its answer in every run
    retried:   3 of 3 — every run immediately re-issued the identical call,
               which the one-decline-per-key rule allows, so the cost was one
               extra round trip per decline and no loop

The first number is what `confirmed=true` on the deny row means here: the
explanation reaches the model. The second is why nothing on this family
declines a call by default — that is a decision for a measured window with the
real reason text, which the run above did not use.

## Recorded observation — which injected step kind the model still holds

    observed:  2026-09-03 on this machine
    client:    agy 1.1.23, model gemini-3.6-flash-medium
    runs:      12
    method:    one step injected at the first invocation carrying a token, then
               a prompt asking the model to repeat the token without reading
               anything
    same invocation:  transient kind 3 of 3, user kind 3 of 3 — both arrive
    later invocations: transient kind 0 of 3, user kind 3 of 3 — after a couple
               of tool calls only the user kind is still in the model's reach
    also:      the step is in the trajectory file in both cases, so its presence
               there is not evidence the model can read it

A session pointer, a manifest of held state and a recovery map are all read
several invocations after they are handed over, so the row below names the kind
that is still there when they are.

## Recorded observation — the same call declined with the reason Rift really writes

    observed:  2026-09-04 on this machine
    client:    agy 1.1.25, model gemini-3.6-flash-medium
    runs:      13 — 10 declining a repeat, 3 declining the same repeat with
               the richer frame that names the changed lines (no edit was made)
    method:    a scratch repository outside this tree with its own hooks file,
               every handler exiting 0, declining the second view of one file
               with decision "deny" and a reason that is the byte-for-byte
               output of Rift's own renderer, written to a file and read back
               by the hook so no transcription could drift
    seen:      10 of 10 and 3 of 3 — asserted by comparison rather than by
               reading: the text the model was handed is the emitted reason
               character for character, behind the client's own fixed prefix
    retried:   10 of 10 and 3 of 3 — every run re-issued the identical call
    pulled:    0 of 10 and 0 of 3 — no run ran the cheaper command the reason
               names, although every answer quoted that command back
    answered:  10 of 10 and 3 of 3 correct, from what the context already held

The earlier run confirmed the channel with a reason written for the occasion;
this one repeats it with the sealed frame, on a newer client, and reaches the
same two answers: the explanation arrives, and the call is made again anyway.
Which is why the row below being `confirmed=true` changes nothing about what
this family is declined on by default — the flag says a decision here can be
understood, and the retry number says it is not yet worth taking.

## The declined call as bytes, and the prefix that is not Rift's

Two facts the page does not carry, both from the runs above: the object Rift
writes, and what the client makes of it before the model reads it. The prefix
is the client's own and is recorded here so that no one adds it, trims it or
reasons around it — one renderer writes what a model reads, and a second place
that shapes those bytes is a second renderer.

```rift-decline-response
response={"decision":"deny","reason":"<REASON>"}
client_prefix="tool call denied by pre-tool hook: "
```

`<REASON>` stands for the rendered frame, spliced in as a JSON string. The
frame itself is family-independent: the renderer that writes it is handed a
path, a digest and a range, knows nothing about which agent asked, and the
tests below hold this family's reason bytes against the ones Claude Code is
handed for the same call.

## Recorded observation — the second route, measured and left unbuilt

    observed:  2026-09-03 on this machine
    client:    agy 1.1.23, model gemini-3.6-flash-medium
    runs:      3, alongside the declined calls above
    method:    the decline recorded that an explanation was owed and the next
               pre-invocation handler injected it as a transient step
    emitted:   3 of 3
    evidenced: 1 of 3, and that one run had first searched and read its own
               transcript file, so it is not evidence of anything reaching a
               context

The route exists on this family and it is not built, for the plainest reason:
the synchronous field above already reaches the model, so a second delivery
would spend a second message on a fact the first one carried. Recorded rather
than dropped, because a route that was measured and rejected is worth more to
a later reader than a silence — and if it is ever wanted, the kind it must use
is the persistent one, not the transient one that run used; the step kinds are
measured a few sections above.

## Capability rows

Each row states one channel Rift may answer on, in the form the code table
carries it. A row exists only because an excerpt above pins it; the
`pinned_output_surface` tests assert this block and the code table are the same
set, in both directions, so neither can grow alone.

```rift-capability-rows
verb=PreToolUse channel=DenyWithReason reason_field=reason confirmed=true
verb=PreInvocation channel=InjectSteps kind=user_message
verb=PostToolUse channel=Neutral
verb=PostInvocation channel=Neutral
verb=Stop channel=Neutral
```

The injection row names `userMessage` of the three documented kinds. The
transient kind was the first guess — a fact about what a session already read is
worth nothing once it is stale — but the measurement above moved the row: the
transient kind is out of the model's reach a few invocations later, and every
fact this channel carries is read later than that. The transient kind keeps its
one use, a notice meant for the invocation it is injected into.

`PostInvocation` carries the same step kinds and is deliberately **not** a
second injection point: one model loop, injected into twice, spends tokens
twice for one fact. Its row is `Neutral`, so the empty object Rift already
writes after an invocation is a shape this pin covers rather than an
unexamined habit — and `terminationBehavior`, the one field on this family
that could force a loop to continue or stop, stays quoted above and unwritten.

The three `Neutral` rows are the shapes Rift already writes and this pin now
covers: an empty object after a tool, an empty object after an invocation, and
a `Stop` decision that is not `"continue"`, which the excerpt above says allows
the stop. Naming them keeps a reader from mistaking a required acknowledgement
for a channel.
