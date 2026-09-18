# PINNED EVIDENCE ARTIFACT — Claude Code's read, edit and search **tool fields**

This file is a **fixture**, not documentation. Two pins sit beside it and each
answers a different question: `claude-code-hook-surface.md` settles *which
hooks exist*, `claude-code-hook-output-surface.md` settles *what Rift may write
back on them*. This one settles the third question, which is about the payload
travelling the other way: **which argument of which tool names the file, the
range, the new bytes, or the search, and what the tool can have delivered for
them.**

It matters for one reason. A decision that a context window already holds
lines 40-90 of a file is only as sound as the claim "the tool delivered lines
40-90". Recording one line too many is a decision taken against a context that
never received it; recording one line too few costs a saving nobody notices.
So every range here is **what the tool can have delivered, never what the
caller asked for**, and an argument whose meaning no artifact states is
recorded as unknown rather than assumed.

The rows at the end of this file are asserted against the code table in
`rift_agent::tool_fields` by `pinned_tool_field_surface` in
`crates/rift-agent-claude-code/src/lib.rs`, and the extraction tests build
their payloads out of the field names the rows carry — so a name that moves
here moves in the code or the tests go red.

## Artifact A — the published tools reference

    repository:     https://docs.claude.com/en/docs/claude-code/tools-reference
    revision:       none published; the page carries no version or revision string
    path:           /en/docs/claude-code/tools-reference.md
    sha256:         ca7dc8d5b32b50b038690ce35ef1b40adbf9c5dccd46786579e75fadff2fe24f
    retrieved:      2026-09-03
    retrieved-from: https://docs.claude.com/en/docs/claude-code/tools-reference.md

Re-verify in two commands, with no judgement involved: fetch the
`retrieved-from` URL above to a scratch file, then

    shasum -a 256 <that file>   # must print the sha256 above

A living page has no tag to pin, so the digest and the retrieval date are the
pin. Line numbers below are lines of the retrieved `.md`.

### `Read`: the path, the two range arguments, and the file types that are not lines (upstream lines 433, 435, 437, 439, 443-445, 447)

```markdown
The Read tool takes a file path and returns the contents with line numbers. Claude is instructed to always pass absolute paths.

By default, Read returns the file from the start. When a whole-file read exceeds the token limit, Read returns the first page with a `PARTIAL view` notice that tells Claude how much of the file it received and how to read more with `offset` and `limit`. A read that passes an explicit `offset` or `limit` and still exceeds the token limit returns an error.

A read with an explicit `limit` stops as soon as the selected lines exceed what the token limit could ever fit and returns an error without loading the rest of the range. The error tells Claude to use a smaller `limit`, or to search for specific content with [Grep](#grep-tool-behavior) instead when a single line is that large. Before v2.1.208, Claude Code loaded the whole range into memory before rejecting it, so reading a file with an extremely long single line could run it out of memory.

Reading an empty file returns a notice that the file exists but its contents are empty, and an `offset` past the last line returns a notice giving the file's line count. Before v2.1.208, reading an empty file returned the past-the-end notice instead.
```

```markdown
* **Images**: PNG, JPG, and other image formats are returned as visual content that Claude can see, not as raw bytes. Claude Code resizes and recompresses large images to fit the model's image size limits before sending them, so Claude may see a downscaled version of a large screenshot. As of v2.1.196, an image that is still larger than 500KB after that resize is re-encoded as a JPEG at reduced quality with its pixel dimensions unchanged. If Claude misses fine pixel-level detail in a large image, ask it to crop the region of interest first, for example with ImageMagick via Bash.
* **PDFs**: Claude reads short `.pdf` files whole. For PDFs longer than 10 pages, it reads in ranges with a `pages` parameter, such as `"1-5"`, up to 20 pages at a time.
* **Jupyter notebooks**: `.ipynb` files return all cells with their outputs, including code, markdown, and visualizations. Claude Code refuses to read a notebook file over 100 MB; the error tells Claude how to read a portion of the notebook instead, such as a slice of cells, with a shell command.
```

```markdown
Read only reads files, not directories. Claude lists directory contents with a shell command such as `ls`.
```

**What this settles, and what it deliberately leaves unknown.**

- The path argument and the two range arguments are named: `file_path`,
  `offset`, `limit`.
- **The default window is not a line count; it is the whole file up to a token
  cap.** A read with no `limit` returns the file from the start and stops where
  a *token* budget runs out, so the last line it delivered is a function of the
  bytes, not of a number this page states. The observations below show six such
  reads stopping at six different lines, and name the cap they stop at: a count
  of tokens, not of lines.
  What follows for a record is settled by O7 rather than guessed: below the cap
  a limit-less read delivers **every line of the file**, and above it the result
  says so in the field O5 pins. So the range recorded for one before it runs is
  the file's own extent while the bytes are small enough that the cap cannot
  have been reached, and **one line** past that size — the same collapse an
  unbounded range has always had, now applied to a bound that is measured
  rather than absent. That collapse is the answer at the *pre-call* boundary,
  where no delivery has happened yet; it is not the last word. The result
  object states its own delivery in the three fields O5 and O7 pin, and the
  note at the end of O7 records what a window is credited from them once that
  statement arrives.
- `pages` addresses **pages of a PDF**, not lines of text, and a notebook read
  returns cells. Neither has a line range Rift can record, so a call carrying
  an argument outside the three pinned above is not read-shaped at all — it is
  recorded as nothing and allowed.

### `Glob` and `Grep`: the search arguments and their caps (upstream lines 264, 270, 272, 284, 286-288, 290, 292)

```markdown
The Glob tool finds files by name pattern. It supports standard glob syntax including `**` for recursive directory matching:
```

```markdown
Results are sorted by modification time and capped at 100 files. If the cap is hit, Claude sees a truncation flag in the result and can narrow the pattern.

Glob doesn't respect `.gitignore` by default, so it finds gitignored files alongside tracked ones. This differs from [Grep](#grep-tool-behavior), which skips gitignored files. To make Glob respect `.gitignore`, set `CLAUDE_CODE_GLOB_NO_IGNORE=false` before launching Claude Code.
```

```markdown
Three output modes control what comes back:

* `files_with_matches`: file paths only, no line content. This is the default.
* `content`: matching lines with file and line number. When the tool's `offset` parameter points past the last match for a pattern that has matches, Grep returns `No entries at this offset`, so Claude widens or resets the offset instead of concluding the pattern doesn't match.
* `count`: match count per file, followed by a total across all matching files. The total covers every match even when the tool's `head_limit` or `offset` parameters truncate the listed per-file entries. Before v2.1.208, the total only summed the listed entries.

Claude can scope results by file with the `glob` parameter, such as `**/*.tsx`, or by language with the `type` parameter, such as `py` or `rust`. By default, patterns match within a single line. Claude can set `multiline: true` to match across line boundaries.

Grep respects `.gitignore`, so gitignored files are skipped. To search a gitignored file, Claude passes its path directly.
```

The `count` row is the whole reason a result cap is safe to add to a search
that declared none: the tool states the true total even when a cap truncates
what it lists, so the caller learns there is more without Rift writing a byte
into the result. `head_limit`, `offset`, `glob`, `type`, `output_mode` and
`multiline` are named here; the single-letter ripgrep flags this tool also
accepts are not named on this page and no row claims them.

### `Edit` and `Write`: the arguments, and the shell commands that count as having read a file (upstream lines 218, 223-224, 226, 228, 574)

```markdown
The Edit tool performs exact string replacement. It takes an `old_string` and a `new_string` and replaces the first with the second. It doesn't use regex or fuzzy matching.
```

```markdown
* **Match**: `old_string` must appear in the file exactly as written. A single character of whitespace or indentation difference is enough to miss.
* **Uniqueness**: `old_string` must appear exactly once. When it appears more than once, Claude either supplies a longer string with enough surrounding context to pin down one occurrence, or sets `replace_all: true` to replace them all.
```

```markdown
A file that changed on disk after Claude last read it can still be edited when `old_string` matches the current content exactly and unambiguously and Claude Code can read the file without prompting. Matching against the file's current content keeps this safe, and the result notes that the file carries other changes so Claude re-reads it before edits that depend on surrounding content. In any other case, such as a stale `old_string` or one that matches more than once without `replace_all`, Claude reads the file again before editing. The relaxed handling of unread and changed files requires Claude Code v2.1.208 or later; before that, Claude Code refused any edit to a file it hadn't read in the conversation or that changed on disk after the read.
```

The uniqueness rule is what makes an edit site derivable at all: with
`replace_all` absent, the replaced text sat in exactly one place, so the new
text sits in exactly one place too — a claim Rift still verifies against the
new bytes rather than assuming.

The strict single-file shell grammar Claude Code itself treats as a read:

```markdown
Viewing a file with Bash also satisfies the read-before-edit requirement when the command is `cat`, `nl`, `bat`, `batcat`, `head`, `tail`, `sed -n 'X,Yp'`, `grep`, `egrep`, `fgrep`, or `rg` on a single file with no pipes or redirects. Piped output and other Bash commands don't count toward the read-before-edit check.
```

This grammar is the vendor's own answer to "which shell commands are a read",
which is why it is the grammar Rift recognises rather than one of Rift's
invention.

It is a list of **commands**, and the page says nothing about how one is
written on a line. O8 below records that, from a census of this project's own
`Bash` calls: a simple command standing alone is four percent of them, and the
rest are composed — a `cd` in front, a narrowing pipe behind, or several reads
joined. The page's own exclusion is about a *pipe* into something else, and it
is kept: a member piped into anything but `head -N` or `tail -N` is not a read
here either.

### Where a shell command line arrives, and the second shell tool beside it (upstream lines 378, 380)

```markdown
Your [PreToolUse hooks](/docs/en/hooks#powershell) receive the tool's command string in `tool_input.command`, with the same fields as the Bash tool.
```

```markdown
Match `Bash|PowerShell` in hooks that inspect shell commands; the [PowerShell hook input section](/docs/en/hooks#powershell) explains why matching `Bash` alone is not enough.
```

The first line is the only place this page names the argument a shell command
line travels in, and it names it by stating that PowerShell carries **the same
fields as the Bash tool** — so `tool_input.command` is pinned for both.

The second is why only one of them is read here. The grammar above is a list
of POSIX commands, and `PowerShell` runs a different language in which several
of those names are aliases for other programs; recognising the list inside a
PowerShell command line would be reading a grammar that page never claimed for
it. A `PowerShell` call is therefore left alone, which costs a saving and can
never record a range the tool did not deliver. The page's warning is about
hooks that *act* on shell commands; the direction Rift acts in is to do
nothing with the ones it cannot vouch for.

`Write`'s single content argument:

```markdown
The Write tool creates a new file or overwrites an existing one with the full content provided. It doesn't append or merge.
```

### The inline result threshold a large slice is measured against (upstream lines 164-167, 171)

```markdown
| Result  | What Claude gets                                                                                                                                                                                                               |
| :------ | :----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Valid   | Inline up to roughly 30,000 characters; past that, the path of a file saved to the session directory, truncated past 64 MiB, plus a short preview from the start, and Claude reads or searches the file when it needs the rest |
```

```markdown
[`BASH_MAX_OUTPUT_LENGTH`](/docs/en/env-vars) sets how many characters of output Claude Code reads back from the working file into a command's result: 30,000 by default, up to a hard ceiling of 150,000. Raise it when your commands routinely overflow that window, such as a verbose build or a full test-suite log. Raising it enlarges the read-back window, and the window a failing command's excerpt is cut from. It does not raise the inline ceilings above: a valid result over roughly 30,000 characters arrives as a file path plus preview regardless of this variable.
```

The table states this for a command's result, and the hooks reference points at
the same section for a hook's own output; **no artifact states it for a `Read`
result**, and O4 below shows a read behaving differently — it is truncated in
place at a token cap and never replaced by a file path. So the number is the
bound on a **shell** read: the size at which a command's result stops arriving
inline at all. That bound is load-bearing rather than incidental, because the
grammar this fixture pins recognises `head -n N` and `sed -n 'A,Bp'`, which
name their lines — a shell read is *not* an unbounded range, and past this
threshold the range it names is a range the model was never handed. O6 records
what such a result looks like from a hook. The bound on a `Read` is O4's own:
a
25,000-token truncation cap, in place, with no file ever written. That is what
`rift_agent::DEFAULT_MAX_SLICE_BYTES` is sized against: 96 KiB, under the
25,000-token cap at the four-bytes-a-token density the tokenizer measures, with
room for the numbering the tool adds, above which a slice is recorded but never
treated as held. A context that got a page instead of the range does not hold
the range.

## Artifact B — the Agent SDK's subagent attribution fields

    repository:     https://github.com/anthropics/claude-agent-sdk-python
    tag:            v0.2.152
    path:           src/claude_agent_sdk/types.py
    blob-sha:       308b76cb7fd928d124666c255b253c92c343f15d
    retrieved:      2026-09-03
    retrieved-from: https://raw.githubusercontent.com/anthropics/claude-agent-sdk-python/v0.2.152/src/claude_agent_sdk/types.py

Re-verify: fetch the `retrieved-from` URL to a scratch file, then
`git hash-object <that file>`; it must print the blob-sha above.

### `agent_id` on the tool-lifecycle hooks (upstream lines 287-310, 312-318)

A subagent's hooks arrive under the parent's session id, so a journal keyed by
session alone would let a subagent's read answer for a context window that
never saw it. This is the field that separates them, and the comment says
plainly both that the tool-lifecycle hooks are the ones that carry it and that
its absence means the main thread.

```python
# agent_id/agent_type are present on BaseHookInput in the CLI's schema but are
# declared per-hook here because SubagentStartHookInput/SubagentStopHookInput
# need them as *required*, and PEP 655 forbids narrowing NotRequired->Required
# in a TypedDict subclass. The four tool-lifecycle types below are the only
# ones the CLI actually populates (the other BaseHookInput consumers don't
# have a toolUseContext in scope at their build site).
class _SubagentContextMixin(TypedDict, total=False):
    """Optional sub-agent attribution fields for tool-lifecycle hooks.

    agent_id: Sub-agent identifier. Present only when the hook fires from
    inside a Task-spawned sub-agent; absent on the main thread. Matches the
    agent_id emitted by that sub-agent's SubagentStart/SubagentStop hooks.
    When multiple sub-agents run in parallel their tool-lifecycle hooks
    interleave over the same control channel — this is the only reliable
    way to attribute each one to the correct sub-agent.

    agent_type: Agent type name (e.g. "general-purpose", "code-reviewer").
    Present inside a sub-agent (alongside agent_id), or on the main thread
    of a session started with --agent (without agent_id).
    """

    agent_id: str
    agent_type: str
```

```python
class PreToolUseHookInput(BaseHookInput, _SubagentContextMixin):
    """Input data for PreToolUse hook events."""

    hook_event_name: Literal["PreToolUse"]
    tool_name: str
    tool_input: dict[str, Any]
    tool_use_id: str
```

## Recorded observations, marked as such

Where no artifact states a fact, this section records runs instead of
inventing one. Each is a tool result from a transcript on this machine under
`~/.claude/projects/`, quoted for its **shape only**; the client version is the
`version` field of the record that carried it. The client on `PATH` reports
`claude --version` → `2.1.186`; the running clients that wrote these records
report 2.1.215 through 2.1.251, which is the honest state of a self-updating
tool and the reason each observation carries its own version.

### O1 — `offset` counts from line 1, and `limit` is a count, not an end line

Ninety-two `Read` calls carrying an `offset` were paired with their results
across the sessions in this project directory; in every one the first numbered
line of the result equals the `offset` and the number of lines returned equals
the `limit`, clamped at end of file. Three of them:

```text
session 75aa16cc, client 2.1.229: offset=555 limit=350 → lines 555…904 (350 lines)
session 75aa16cc, client 2.1.229: offset=736 limit=45  → lines 736…780 (45 lines)
session 2d62a601, client 2.1.251: offset=869 limit=868 → lines 869…1109 (241 lines, end of file)
```

So the last line a read can have delivered is `offset + limit - 1`, and the
first is `offset`. The third observation is the clamp: the file ended first, so
the recorded range is intersected with the file's line count when the bytes are
in hand.

### O2 — a read with no `limit` stops at a token budget, not at a line count

The `PARTIAL view` notice a truncated whole-file read returns, from four
sessions (the file paths are elided; nothing else is changed):

```text
PARTIAL view — <path>: showing lines 1-586 of 720 total (26096 tokens, cap 25000). Call Read with offset=587 limit=586 for the next page
PARTIAL view — <path>: showing lines 1-916 of 1261 total
PARTIAL view — <path>: showing lines 1-932 of 1171 total
PARTIAL view — <path>: showing lines 1-868 of 1109 total
```

Four different stopping lines, one cap in tokens. **There is no default window
in lines to pin**, and none is guessed here. What is pinned instead, by O7, is
the delivery either side of the cap: the whole file below it, and a stated
truncation above it.

### O3 — the `Edit` and `Write` result text, and what it does *not* contain

Every `Edit` and `Write` result in this project's twenty-four transcripts
(1,196 `Edit` calls, 126 `Write` calls) is one of these lines. The path is the
only variable part:

```text
The file <path> has been updated successfully. (file state is current in your context — no need to Read it back)
The file <path> has been updated. All occurrences were successfully replaced. (file state is current in your context — no need to Read it back)
The file <path> has been updated successfully. (note: the file had been modified on disk since you last read it — the edit applied cleanly, but the file contains other changes not in your context. Read it before edits that depend on surrounding content.)
File created successfully at: <path> (file state is current in your context — no need to Read it back)
```

Counts and provenance: the first form 586 records (earliest 2026-08-18, client
2.1.222 onward); the second 2 records, the `replace_all` wording (clients
2.1.215 and 2.1.229); the third 12 records (client 2.1.223); the fourth 112
records. Ten `Edit` results and one `Write` result carry the same first
sentence with no parenthetical at all.

**The result carries no echo of the edited lines.** Searching every transcript
under `~/.claude/projects/` for a tool result containing "result of running
`cat -n`" returns none: the two files that contain that phrase carry it in
message prose, not in a tool result. Whatever earlier clients did, the client
writing these records answers an edit with one sentence and an explicit
instruction not to read the file back — which is a stronger statement of "you
hold this" than an echo would be, and it is stated by the tool rather than
assumed by Rift. Anything that later reasons about what an edit result showed
the model must start here, not from the echo.

### O4 — where a limit-less read actually stops, and what happens past it

A recorded spike drove the installed client over the cap deliberately and
captured the result. The logs are held at
`/Users/ulpian/rift-worktrees/imp-spikes/d/logs/`; two hints, verbatim from
`d2.stream.jsonl`:

```text
[Truncated: PARTIAL view — showing lines 1-1180 of 1991 total (35826 tokens, cap 25000). Call Read with offset=1181 limit=1180 for the next page, or Grep to find a specific section. Do NOT answer from this page alone if the answer may be further in the file.]
[Truncated: PARTIAL view — showing lines 1-1181 of 1501 total (27006 tokens, cap 25000). Call Read with offset=1182 limit=1181 for the next page, or Grep to find a specific section. Do NOT answer from this page alone if the answer may be further in the file.]
```

Two facts follow, and both matter here.

**A limit-less read stops at a token cap, not at a line count.** Two files,
two different stopping lines, one cap of 25,000 tokens — the same shape O2
records from ordinary sessions, now with the cap named. There is still no
window to pin: how many lines fit under a token cap is a property of the
bytes.

**A read result is not persisted to a file.** The truncation is announced in
place and the delivered lines follow it. In the same spike, a *shell* result
over the threshold behaved differently: `Output too large (30.3KB). Full
output saved to: …` and `Output too large (39.1KB). Full output saved to: …`.
So the pointer-instead-of-bytes case belongs to command output, and the
ceiling below sits under the smaller of the two numbers either way.

### O5 — how a truncated read *says* it was truncated, and how much it delivered

O4 settles that a read can be truncated. This settles what the result carries
when it was, because the recorded range for such a read must come down to what
the tool actually handed over — and that correction can only be made from a
statement the tool makes about itself, never from a guess.

Four recorded results, from four sessions and two clients, each a `Read`
result whose delivery stopped at the token cap. The field names are the
result object's own; `content` is elided by length. Marked as observations,
not as a published schema — the reference page states that a truncated
whole-file read returns a `PARTIAL view` notice and states nothing about the
fields of the result object.

```text
spike (d), client 2.1.186, logs/d2.stream.jsonl record 14 (tool_use_result):
  {"type":"text","file":{"filePath":"…/g150000.txt","content":<118099 chars>,
   "numLines":1181,"startLine":1,"totalLines":1501,"truncatedByTokenCap":true}}
spike (d), client 2.1.186, logs/d2.stream.jsonl record 15 (tool_use_result):
  {"type":"text","file":{"filePath":"…/g199000.txt","content":<117999 chars>,
   "numLines":1180,"startLine":1,"totalLines":1991,"truncatedByTokenCap":true}}
session 2d62a601, client 2.1.251, record 974 (toolUseResult):
  {"type":"text","file":{"filePath":"…/b918k6zu2.txt","content":<54553 chars>,
   "numLines":868,"startLine":1,"totalLines":1109,"truncatedByTokenCap":true}}
session 9cc1d0e5, client 2.1.251, record 79 (toolUseResult):
  {"type":"text","file":{"filePath":"…/LEDGER.md","content":<31073 chars>,
   "numLines":64,"startLine":1,"totalLines":276,"truncatedByTokenCap":true}}
```

The spike's two logs are held at
`/Users/ulpian/rift-worktrees/imp-spikes/d/logs/d2.stream.jsonl`; the two
session records are transcripts under `~/.claude/projects/`. A search of every
transcript on this machine for `truncatedByTokenCap` returns these four and no
fifth, so the flag is rare and the sample is the whole of it.

**Three facts follow, and the recording rule needs all three.**

**The flag is the reliable channel; the notice is not.** Two places carry the
truncation and they are not the same place. The result *object* above — the
one a `PostToolUse` payload carries as `tool_response` — states it as a
boolean field. The `tool_result` *content block*, which is the text the model
itself reads, states it as a sentence: both 2.1.186 records open that block
with the notice, before the first numbered line, and neither 2.1.251 record
carries a notice there at all.

```text
<system-reminder>[Truncated: PARTIAL view — showing lines 1-1181 of 1501 total (27006 tokens, cap 25000). Call Read with offset=1182 limit=1181 for the next page, or Grep to find a specific section. Do NOT answer from this page alone if the answer may be further in the file.]</system-reminder>
```

On 2.1.251 that block begins at the file's own first numbered line, while the
result object still carries `truncatedByTokenCap: true`. A reader that looked
only for the sentence would have called both of those reads complete. The dash
in the notice is an em dash (U+2014); the wording moved between clients (O2's
ordinary-session form names the path and omits the `[Truncated: …]` wrapper)
and the field name did not. So the field is what a hook reads, and the
sentence is read only where a result reaches a hook as text rather than as the
object above.

**The delivered range is stated, not inferred.** `startLine` is the first line
handed over and `numLines` how many followed it, so the tool delivered
`startLine ..= startLine + numLines - 1` — 1…1181 of a 1,501-line file, 1…64
of a 276-line one. `totalLines` is the file, not the delivery, and the
difference between them is exactly the range a record must stop claiming.

**The cap is a token cap and the density is not four bytes.** 118,099
characters priced at 27,006 tokens is 4.37 bytes a token on padded ASCII;
31,073 characters at 64 lines of this repository's own Markdown is a quite
different number, and the slices this repository's fixtures measure run 1.90
to 2.22. So a byte ceiling derived at four bytes a token is not a bound on what a
read delivered, and what is recorded as *held* is bounded at the smallest
density measured rather than at the nominal one.

**Where this leaves absence.** A result with no flag and no notice is not
evidence of a complete delivery — it is silence, and this fixture does not
turn silence into a fact. The downgrade fires on a positive statement only,
which is why it is a backstop and not the primary guard: the primary guard is
that a slice too large to have survived the cap is never recorded as held in
the first place.

The flag set to **`false`** is not that absence; it is the opposite statement,
made in the same field by the same client. So is a `numLines` that reaches the
result's own `totalLines`, which is the shape O7 records for a read the cap
never touched and the only one of the two that any recorded run actually
carries. Both are the client vouching for its own delivery, and the note at the
end of O7 says what is credited from them.

### O6 — what a command result looks like when the client keeps it in a file

O4 notes in passing that a *shell* result over the threshold is replaced by a
pointer. This settles what that replacement looks like from where a hook
stands, because the recording rule that answers it — a shell read past the
threshold is credited with one line — needs to be checkable at
`PostToolUse` as well as guessable from the size of the slice.

Three recorded results, marked as observations rather than as a published
schema. The reference page above states the behaviour; it names none of these
fields. `stdout` is elided by length.

```text
spike (d), client 2.1.186, session 09e6dffb record 14 (toolUseResult):
  {"stdout":<29999 chars>,"stderr":"","interrupted":false,"isImage":false,
   "noOutputExpected":false,
   "persistedOutputPath":"…/09e6dffb-…/tool-results/bawxzim8j.txt",
   "persistedOutputSize":31000}
spike (d), client 2.1.186, session 09e6dffb record 19 (toolUseResult):
  {"stdout":<29999 chars>,…,"persistedOutputPath":"…/bwpt8q8fl.txt",
   "persistedOutputSize":40000}
session 688d5677, client 2.1.251, record 2575 (toolUseResult):
  {"stdout":<29876 chars>,…,"persistedOutputPath":"…/b9jder91k.txt",
   "persistedOutputSize":59958}
```

The spike's stream logs are held at
`/Users/ulpian/rift-worktrees/imp-spikes/d/logs/`; the third is a transcript
under `~/.claude/projects/`. A sweep of every transcript on this machine finds
**30** results carrying `persistedOutputPath`, every one of them a `Bash`
result, the smallest `persistedOutputSize` among them 31,000 — and **no** `Bash`
result over 31,000 characters that lacks the field. So the field is what the
spill looks like, and nothing on this machine spilled without it.

**Three facts follow.**

**The pointer is a structured field, not the sentence.** What the *model* reads
is an envelope in the `tool_result` content block, verbatim from record 14:

```text
<persisted-output>
Output too large (30.3KB). Full output saved to: /Users/ulpian/.claude/projects/-Users-ulpian-rift-worktrees-imp-spikes-d/09e6dffb-2051-4b53-9d47-b7ccbb602ebe/tool-results/bawxzim8j.txt

Preview (first 2KB):
```

That envelope is **not** in the result object a `PostToolUse` payload carries
as `tool_response`; the object carries the two fields instead. Same split as
O5: the sentence is for the model, the field is for the hook, and a hook that
went looking for the sentence would find nothing.

**`stdout` is the read-back window, not the delivery.** All three carry about
29,999 characters of it while `persistedOutputSize` states 31,000, 40,000 and
59,958 — the client reads a bounded window back from its working file, and
neither number is what the model received. The model received the preview.

**The size is the threshold O4's table names.** 31,000 is the smallest spill
recorded here and the spike walked the boundary a thousand characters at a
time — 29,999 inline, 30,999 persisted — so the bound a shell read is held to
is 30,000, the smaller side of the interval, and it is measured in characters
because that is the unit the client's own rule is written in.

### O7 — what a limit-less read delivers when it stays under the cap

O4 and O5 settle where a limit-less read *stops* and how it says so. This
settles the other side of the same boundary, which is the side almost every
real read falls on: what such a read hands over when the cap is never reached.
It is recorded because the answer decides whether a whole-file read can be
remembered as a whole file, and that must come from a run rather than from the
plausible assumption that "no truncation flag" means "everything".

Six `Read` calls from the same spike as O4 and O5, each with `file_path` and
**no `offset` and no `limit`**, paired with the result object they produced.
The `content` field is elided by length; nothing else is changed.

```text
spike (d), client 2.1.186, logs/d1.stream.jsonl
  in  record 13: Read {"file_path":".../f25000.txt"}
  out record 14: {"filePath":".../f25000.txt","numLines":251,"startLine":1,"totalLines":251}
  in  record 15: Read {"file_path":".../f29000.txt"}
  out record 16: {"filePath":".../f29000.txt","numLines":291,"startLine":1,"totalLines":291}
  in  record 17: Read {"file_path":".../f31000.txt"}
  out record 18: {"filePath":".../f31000.txt","numLines":311,"startLine":1,"totalLines":311}
  in  record 19: Read {"file_path":".../f40000.txt"}
  out record 20: {"filePath":".../f40000.txt","numLines":401,"startLine":1,"totalLines":401}
spike (d), client 2.1.186, logs/d2.stream.jsonl
  in  record 8:  Read {"file_path":".../g60000.txt"}
  out record 9:  {"filePath":".../g60000.txt","numLines":601,"startLine":1,"totalLines":601}
  in  record 10: Read {"file_path":".../g100000.txt"}
  out record 11: {"filePath":".../g100000.txt","numLines":1001,"startLine":1,"totalLines":1001}
```

The two logs are held outside this repository at
`/Users/ulpian/rift-worktrees/imp-spikes/d/logs/`, and a reader rechecks the
rows above from the bytes alone:

    sha256(d1.stream.jsonl) = 746b3a568eb6948a741348bb9098f312ea951b06f883ad6302057a61c5f512fc
    sha256(d2.stream.jsonl) = a508dcfeee26f19e7fe4bf0f799551f3e4c7ae8b66e84968e3d79e06df21727a

A search of both logs for `truncatedByTokenCap` returns the two O5 rows and
these six not at all, so the absence here is a searched absence rather than an
unexamined one — and it is read together with the positive statement, not
instead of it: these six are the reads whose `numLines` **equals** their own
`totalLines`, which is a statement the result makes about itself.

**Three facts follow.**

**Below the cap, the delivery is the file.** `startLine` is 1 and `numLines`
equals `totalLines` in all six — 251 of 251, 1001 of 1001 — across files from
25,000 to 100,000 bytes. There is no page, no window and no line count in
between: the read either delivered everything or said it did not.

**The client counts one line more than the bytes do.** Every one of the six
files ends in a newline and holds one fewer newline-terminated line than the
`totalLines` the client reports (250 lines of text against `totalLines` 251;
1,000 against 1,001; and in O5's truncated pair, 1,500 against 1,501 and 1,990
against 1,991). The client counts the empty stretch after the final newline as
a line; Rift's own count, taken from the bytes while they are being hashed,
does not. So a whole-file range recorded from Rift's count is one line
**shorter** than what the client says it handed over, which is the direction
that costs a saving rather than a wrong decision.

**The size bound is a bound on what a guess may claim, not on what a window
holds.** Two of the six files (60,000 and 100,000 bytes) were delivered whole
and are nonetheless over the byte ceiling O5 derives, so nothing is credited
for them before the call runs. That stays deliberate: the ceiling is the
largest slice that could *still* have been cut off at the smallest density
measured, not the largest one that ever survives, and a rule sized to the
observed survivals would over-claim the first time a denser file came along.
What these two rows settle is the other half — that a whole delivery over the
ceiling is ordinary rather than exotic, and that the result says as much about
itself. The note below is what is done with that.

#### Recorded observation — what a window is credited from these three fields

The observations above pin three numbers and a boolean. This records what is
credited from them, because the rule is the reason the pin exists and a reader
left to reconstruct it from the code has been handed the harder half of the
job.

At the tool's own result boundary — the `PostToolUse` payload, where the object
O5 and O7 quote arrives as `tool_response` — the delivered run is
`startLine ..= startLine + numLines - 1`, and it is credited **whatever the
slice weighs**:

- `truncatedByTokenCap: true` — the client says it stopped, and the run it
  names is what the window holds. This was already the correction; it is now
  the same rule read in the other direction too, so a delivery the size bound
  refused outright is credited with the run the client names rather than with
  nothing at all.
- `truncatedByTokenCap: false`, **or** the field absent with `numLines`
  reaching `totalLines` — the client says the call was answered in full. The
  second shape is the one the six rows above record: an untruncated result
  carries no flag, and `numLines == totalLines` is the statement it makes
  instead.
- anything else — a run stopping short of `totalLines` with no flag beside it,
  a result carrying none of the fields, a family whose result shape no run has
  pinned — is **silence**, and silence stays what O5 says it is. The size bound
  is then the only guard, exactly as before.

Two clamps and one exclusion, each of them a fact from the observations above
rather than a caution:

- the run is intersected with the range the call could have delivered, which is
  already clamped to Rift's own line count. That is where the second fact above
  lands: the client counts the empty stretch after a final newline, so its
  `totalLines` is one larger than the newline-terminated lines the bytes hold,
  and the intersection spends that line on a saving rather than on a claim;
- a run sharing no line with the call it answers is no run of it at all, and
  the window loses the claim rather than gaining a strange one;
- a **command's** result is never credited this way. O6 settles why: past its
  own threshold a shell result is a pointer and a preview, the fields above
  belong to the read tool and not to the shell, and the bound a shell read is
  held to stays the 30,000 characters O6 measures.

**What it costs.** The statement arrives one hook after the call, so nothing is
credited at the pre-call boundary and two identical reads issued in the same
turn both run — a saving not taken, which is the direction every rule in this
file leans.

### O8 — the shapes a shell read is actually written in

The excerpt above is the vendor's list of *commands*. It says nothing about
how an agent composes one, and the adapter read it as though a command always
arrived alone. This records what the composition actually looks like, because
the difference decides whether a shell read is seen at all.

A census of the `Bash` calls in eight sessions of this project directory —
**counts and shapes only; no command line is reproduced here**, and no path
from one is either. Every call whose command line ran one of the read commands
of the excerpt above was classified by the *shape* it was written in:

```text
864 shell reads, 8 sessions, clients 2.1.215-2.1.251
   35  one simple command, alone on the line
  350  `cd <dir> &&` followed by several commands
  373  a command piped onward (`| head -N`, `| tail -N`, `| grep …`)
   20  `cd <dir> &&` followed by one read
   86  other compound forms
```

In both `cd` classes the directory is double-quoted and spelled with forward
slashes. The pipe counts are of the *trailing* stage, and the three commands
named are the ones that fill it.

**Four facts follow.**

**A single simple command is the exception, not the rule.** 35 of 864 — four
percent. The other 829 were reads of this repository that no journal recorded,
so they could never be declined and never entered the denominator any savings
share divides by. That is an accounting hole, not a conservative choice: the
tokens were spent either way.

**A `cd` is a prefix, not a command.** 370 of the 864 open with one, and it is
where the line opens that makes it a prefix: what follows is read from that
directory, and nothing about the rest of the line changes. So the directory
can be carried onto each path lexically, and the caller resolves the result
exactly as it resolves every other relative path. The grammar admits it only
in that position and only once, which is narrower than the census and is
deliberately so.

**A narrowing pipe is arithmetic on the lines the command produced.** `head -N`
and `tail -N` in the trailing position take a prefix or a suffix of the run
their member printed. A `tail` is the one that needs the file's own length,
which is why it travels as a count from the end rather than as a line number
nothing on the line states. Every other pipe target — a content search, a word
count, a pager — either changes what the lines are or hands the window
something that is not lines, and is not a read.

**A chain is a chain of reads or it is not read-shaped.** The 86 "other
compound" calls are the remainder — every shape the three rows above do not
cover — and the common case among them is a read standing beside something
that is not one. Those stay outside the grammar altogether: a line one of
whose members is a `cargo`, a redirect or a glob is a line nothing here can
answer for, and refusing all of it is the only refusal that cannot be wrong.
(O9 below is the one member this sentence originally swept up and should not
have: a member that reads nothing is not a member nothing can be said about.)

The census is of call *shapes*, so it is recorded here rather than pinned as an
excerpt: no vendor page states it, and the reason it belongs in this file is
that the row below reads the command argument under a grammar, and the grammar
has to be the one agents write.

### O9 — a chain captions its own output, and the caption is not a read

The three compositions of O8 shipped, and the census was taken again over the
owner's frozen corpus to see what was still invisible. Recognised shell calls
had risen from **29 to 83 of 1,252**. The largest remaining shape was one O8
had counted under "other compound" without looking at it: a chain whose reads
are separated by `echo` lines the agent writes to caption its own output.

```text
≈ 90 commands, shapes only
  sed > echo > sed
  cd > echo > sed
  cd > sed > echo > sed
  …and the bare `echo` that prints a blank line between two reads
```

Taking them in roughly doubles what the grammar sees. Again: shapes and counts
only, and no command line or path is reproduced here.

**Two facts follow.**

**The caption is a member that reads nothing.** `echo` writes its operands and
`printf` writes them through a format; neither opens a file under any argument.
So such a member has no range to record and nothing to be declined about — and
the safety question is not what it delivered but whether Rift can be *sure*
that is all it did. The word grammar already answers that: an operand reaches
the recogniser only as a bare word or a whole-word quoted literal, and every
way a shell could turn one into something else — `$`, a backtick, a backslash
outside a single-quoted word, a redirect, a substitution — is a character no
word may hold. A caption carrying one of those is not a caption; it takes the
whole line out, as any unreadable member does.

**Reading nothing is not the same as disqualifying nothing, and both are
wanted.** A label adds no request, so a line of labels alone is not a read at
all; and it is no reason to leave the reads around it unaccounted for, so a
chain of reads and labels is decided on its reads exactly as the same chain
without them would be. One consequence is worth stating plainly: declining
such a call suppresses the captions along with the reads. The captions are
text the agent composed in the command it just wrote, so it holds them
already — which is why this is a cost worth paying and not a fact withheld.

## Tool fields

Each row states one tool this adapter recognises and what it recognises it as.
A row exists only because an excerpt above pins it, and the
`pinned_tool_field_surface` tests assert this block and the code table are the
same set in both directions, so neither can grow alone.

`shape=read|edit|search` is what the call is; `shape=none` is a tool this
adapter deliberately reads nothing from, named so a later reader can tell
abstention from oversight. `shape=shell` is the one shape that depends on the
argument's *content*: the command line is a read or a listing only when it
matches the grammar excerpted above, exactly, composed in one of the three
ways O8 records — a leading `cd <dir> &&`, a trailing `| head -N` or
`| tail -N` on a read, and members joined by `&&` or `;` of which every one is
itself a read or one of O9's labels — and is otherwise a call this adapter
reads nothing from.
`grammar=composed-read-and-listing` names that pair: the vendor's vocabulary
and those three compositions, and nothing else. One such line is as many reads
as it spells. `lo_base=1` and `hi_kind=count` are O1;
`default_window=to-end-of-file` is O7 — a limit-less read delivers the file's
last line, and the size at which that stops being vouchable is the ceiling O5
derives, not a line number.

```rift-tool-field-rows
tool=Read shape=read path=file_path lo=offset lo_base=1 hi=limit hi_kind=count default_window=to-end-of-file
tool=Glob shape=search kind=glob pattern=pattern
tool=Grep shape=search kind=grep head_limit=head_limit
tool=Edit shape=edit kind=edit path=file_path new=new_string old=old_string replace_all=replace_all
tool=Write shape=edit kind=write path=file_path content=content
tool=MultiEdit shape=edit kind=other path=file_path
tool=NotebookEdit shape=edit kind=other path=file_path
tool=mcp__acp__Write shape=edit kind=other path=file_path
tool=mcp__acp__Edit shape=edit kind=other path=file_path
tool=Bash shape=shell command=command grammar=composed-read-and-listing
```

`kind=other` is not a gap in the pin: those four tools change a file, so the
coverage of its old bytes must stop being coverage, but none of them carries a
single before-and-after pair from which one edit site follows. They are
recorded as a change with no derivable site, which is the honest reading and
the safe one.
