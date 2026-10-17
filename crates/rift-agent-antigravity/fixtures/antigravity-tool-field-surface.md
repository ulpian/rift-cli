# PINNED EVIDENCE ARTIFACT — Antigravity's read, edit and search **tool fields**

This file is a **fixture**, not documentation. `antigravity-hook-output-surface.md`
beside it settles what Rift may write back on this family's hooks; this one
settles the payload travelling the other way — **which argument of which tool
names the file, the range, the new bytes, or the search.**

Antigravity is the family whose tool surface is stated most plainly and whose
*semantics* are stated least: the hooks page lists every argument by name, and
says nothing about what a line number counts from or what a view returns when
the range is left out. Both halves are recorded below, and the second half is
why the ranges this adapter derives are narrower than the ones a confident
reader would derive.

The rows at the end of this file are asserted against the code table in
`rift_agent::tool_fields` by `pinned_tool_field_surface` in
`crates/rift-agent-antigravity/src/lib.rs`, and the extraction tests build
their payloads out of the field names the rows carry.

## Artifact — the published hooks documentation

    repository:     https://antigravity.google/docs/hooks
    revision:       none published; the page carries no version or revision string
    path:           /docs/hooks.md
    sha256:         8b08cf1ea720762d27f89cfe2a4a4f9fa41f85530e10b83322eaed810bbc156c
    retrieved:      2026-09-03
    retrieved-from: https://antigravity.google/docs/hooks.md

Re-verify in two commands: fetch the `retrieved-from` URL above to a scratch
file, then

    shasum -a 256 <that file>   # must print the sha256 above

Same digest as the output-surface pin beside it, and deliberately so: one page
carries both contracts, and pinning it twice means neither fixture can be
re-taken without the other being re-checked. Line numbers below are lines of
the retrieved `.md`. The installed client reports `agy --version` → `1.1.23`,
recorded as an observation because the page states no version of its own.

### Where the arguments live on the wire (upstream lines 187-193)

```markdown
| Field | Type | Description |
| :-- | :-- | :-- |
| `toolCall` | object | Details of the proposed tool call. |
| `toolCall.name` | string | The name of the tool being executed (e.g., `run_command`). |
| `toolCall.args` | object | Arguments passed to the tool call. |
| `stepIdx` | integer | The 0-based index of the current step in the trajectory. |
```

Nested, not flat: the tool's own arguments are one object deeper than on the
other two families. The worked example on the same page shows it (upstream
lines 213-221):

```json
{
  "toolCall": {
    "name": "run_command",
    "args": {
      "CommandLine": "npm test",
      "Cwd": "/workspace/project",
      "WaitMsBeforeAsync": 5000
    }
  },
```

### The file and directory tools (upstream lines 93-104)

```markdown
*   **`view_file`**: View the contents of a file.
    *   Arguments: `AbsolutePath`, `StartLine` (optional), `EndLine` (optional), `IsSkillFile` (optional)
*   **`write_to_file`**: Create new files.
    *   Arguments: `TargetFile`, `Overwrite`, `CodeContent`, `Description`, `IsArtifact` (optional), `ArtifactMetadata` (optional)
*   **`replace_file_content`**: Edit a single contiguous block of text in a file.
    *   Arguments: `TargetFile`, `Instruction`, `Description`, `AllowMultiple`, `TargetContent`, `ReplacementContent`, `StartLine`, `EndLine`, `TargetLintErrorIds` (optional)
*   **`multi_replace_file_content`**: Make multiple, non-contiguous edits to the same file.
    *   Arguments: `TargetFile`, `Instruction`, `Description`, `ReplacementChunks` (array of chunks), `TargetLintErrorIds` (optional), `ArtifactMetadata` (optional)
*   **`list_dir`**: List the contents of a directory.
    *   Arguments: `DirectoryPath`
*   **`find_by_name`**: Search for files and directories using glob patterns.
    *   Arguments: `SearchDirectory`, `Pattern`, `Type` (optional), `Excludes` (optional), `Extensions` (optional), `FullPath` (optional), `MaxDepth` (optional)
```

### The content search (upstream lines 108-109)

```markdown
*   **`grep_search`**: Fast text searches within specific paths.
    *   Arguments: `SearchPath`, `Query`, `IsRegex` (optional), `CaseInsensitive` (optional), `Includes` (optional), `MatchPerLine` (optional)
```

**No result-cap argument exists on this tool.** The list is the whole list, and
nothing in it bounds how many matches come back — so the additive cap that is
available on another family has nothing to attach to here, and no row below
offers one. A content search on this family is recorded and left alone.

### The shell tool (upstream lines 117-118)

```markdown
*   **`run_command`**: Propose a bash command to run.
    *   Arguments: `CommandLine`, `Cwd`, `WaitMsBeforeAsync`, `RunPersistent` (optional), `RequestedTerminalID` (optional)
```

Named so a later reader can see that the argument carrying a command line is
pinned, and that nothing in this commit reads it.

## What the page does not state, and what follows from that

**The line numbering's base.** `StartLine` and `EndLine` are named as optional
arguments of `view_file`; nothing says whether the first line of a file is line
0 or line 1. The page states a base exactly once, for something else — `stepIdx`
is "the 0-based index" — which shows the page states a base when it means to,
and is silent here.

Rift therefore records the range **both readings agree on**. Under 1-based
numbering the view delivers lines `StartLine` through `EndLine`; under 0-based
it delivers `StartLine + 1` through `EndLine + 1`. Every line in
`[StartLine + 1, EndLine]` is delivered under either reading, and that is what
is recorded — one line narrower at the top than the confident reading, and
never a line the tool did not deliver. This is not a guess split down the
middle: it is the intersection of the only two possibilities, so it stays
correct whichever one is true, and a re-pin that settles the base can only
widen it.

**A view with no range.** With `StartLine` absent, both readings start at the
file's first line, so the interval starts at 1 — but nothing states where such
a view *stops*, and no observation of this client exists in this tree. The end
is recorded as unknown, which means one line: the read is remembered and can
never satisfy a later request for more.

**What an edit result shows the model.** Nothing on the page says what
`replace_file_content` or `write_to_file` returns. Until that is pinned or
observed, this family records the edits it sees — the coverage of a file's old
bytes must stop being coverage the moment they change — and nothing is ever
declined on the strength of them. Removing an agent's only sight of its own
edit is not a saving.

## Recorded observations, marked as such

The page's argument lists are not closed, and only a run shows it. These are
`PreToolUse` payloads a hook of Rift's own captured from the installed client
during a recorded spike, held at
`/Users/ulpian/rift-worktrees/imp-spikes/g/logs/`, quoted for their **shape
only**. The client reports `agy --version` → `1.1.23`.

### O1 — the arguments are nested, and the payload carries two the page omits

One `view_file` call, verbatim from `bE1.hook.jsonl`:

```json
{
  "artifactDirectoryPath": "/Users/ulpian/.gemini/antigravity-cli/brain/f7c9fdf2-9408-405f-b657-c00c98593fa2",
  "conversationId": "f7c9fdf2-9408-405f-b657-c00c98593fa2",
  "modelName": "gemini-3.6-flash-medium",
  "stepIdx": 5,
  "toolCall": {
    "args": {
      "AbsolutePath": "/Users/ulpian/rift-worktrees/imp-spikes/g/alpha.txt",
      "toolAction": "Viewing alpha.txt first time",
      "toolSummary": "View alpha.txt"
    },
    "name": "view_file"
  },
  "transcriptPath": "/Users/ulpian/.gemini/antigravity-cli/brain/f7c9fdf2-9408-405f-b657-c00c98593fa2/.system_generated/logs/transcript_full.jsonl",
  "workspacePaths": [
    "/Users/ulpian/rift-worktrees/imp-spikes/g"
  ]
}
```

`transcriptPath` is the session transcript's address — the client's
`transcript_full.jsonl` under its `brain/<conversation id>/` directory. The
bridge copies it to the session cache and the hook path never opens it; the
archive reads the file and seals it as `transcript.jsonl` /
`transcript.compact.jsonl` through the redaction gate, like every other agent's
transcript.

`toolAction` and `toolSummary` are on the wire and on no published list. They
are the client's own description of what it is about to do, they say nothing
about a range, and they arrive on **every** tool call the spike recorded:
`view_file`, `find_by_name` (with `Excludes`, `Extensions`, `FullPath`,
`MaxDepth`, `Pattern`, `SearchDirectory`, `Type`), and `grep_search` (with
`Query`, `SearchPath`). They therefore join the accepted set as an envelope
that is read past, and the row below names them.

There is also **no `tool_input` on this family's payload at all** — the other
two families' spelling — which the excerpt above and this run agree on.

### O2 — what the run does not show

No recorded call carries `StartLine` or `EndLine`, and no run exercised the
three file-changing tools or the directory listing. So the range arithmetic
below rests on the page alone, and the two envelope keys are the only
undocumented arguments anyone here has seen. **The page's list is open, which
this rule now knows**: an argument outside the row's set takes the call out of
the read shape, so the next undocumented argument costs recording rather than
correctness, and is a re-pin away from costing nothing.

## Tool fields

Each row states one tool this adapter recognises and what it recognises it as.
A row exists only because an excerpt above pins it, and the
`pinned_tool_field_surface` tests assert this block and the code table are the
same set in both directions.

`args=toolCall.args` records the nesting once, on the row of every tool whose
arguments Rift reads. `lo_base=undocumented` is the intersection rule above;
`default_window=unpinned` is the range-less view.

`shape=shell` on `run_command` is the one row whose meaning depends on the
argument's *content*: the command line is read as a file read or a listing
only when it matches, exactly, the read-and-listing grammar another vendor
publishes for its own read-before-edit check, together with the three
compositions that grammar is spelled with — a leading `cd`, a trailing
narrowing pipe, and a chain of reads — and is left alone otherwise. Both are
pinned in `claude-code-tool-field-surface.md`, the compositions under its
recorded observations. `Cwd` is on the row because this family states the
working directory beside the command, which is what makes a relative path in
one resolvable at all.

```rift-tool-field-rows
tool=view_file shape=read args=toolCall.args path=AbsolutePath lo=StartLine lo_base=undocumented hi=EndLine hi_kind=line flag=IsSkillFile envelope=toolAction,toolSummary default_window=unpinned
tool=write_to_file shape=edit kind=write args=toolCall.args path=TargetFile content=CodeContent
tool=replace_file_content shape=edit kind=edit args=toolCall.args path=TargetFile new=ReplacementContent old=TargetContent replace_all=AllowMultiple
tool=multi_replace_file_content shape=edit kind=other args=toolCall.args path=TargetFile
tool=find_by_name shape=search kind=listing args=toolCall.args dir=SearchDirectory pattern=Pattern
tool=list_dir shape=search kind=listing args=toolCall.args dir=DirectoryPath
tool=grep_search shape=search kind=grep head_limit=none
tool=run_command shape=shell args=toolCall.args command=CommandLine cwd=Cwd grammar=composed-read-and-listing
```

`head_limit=none` on the content search is the pin's own answer, not an
omission: the tool has no such argument to add.
