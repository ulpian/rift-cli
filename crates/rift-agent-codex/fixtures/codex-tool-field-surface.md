# PINNED EVIDENCE ARTIFACT — Codex's read, edit and search **tool fields**

This file is a **fixture**, not documentation. `codex-hook-output-surface.md`
beside it settles what Rift may write back on this family's hooks; this one
settles the payload travelling the other way — **which argument of which tool
names the file, the range, the new bytes, or the search.**

For this family the answer is unusually short, and the short answer is the
finding. Codex fires its tool hooks under three canonical names, and neither a
structured read tool nor a search tool is among them: a read, a listing and a
grep all arrive as a shell command line, and every edit arrives as one patch
document. There is nothing here to key a line range on, so this adapter derives
**no read request and no search request at all** — not as a gap, but as the
pinned shape.

The rows at the end of this file are asserted against the code table in
`rift_agent::tool_fields` by `pinned_tool_field_surface` in
`crates/rift-agent-codex/src/lib.rs`.

## Artifact — the Codex source at a stable tag

    repository:     https://github.com/openai/codex
    tag:            rust-v0.152.1
    retrieved:      2026-09-03
    retrieved-from: git clone --depth 1 --branch rust-v0.152.1 https://github.com/openai/codex

Five files at that one tag, each pinned by its own blob sha:

    path:           codex-rs/core/src/tools/hook_names.rs
    blob-sha:       92ebe8aa56de1ad7fe62be417666eb169c2c3b24

    path:           codex-rs/core/src/tools/handlers/unified_exec/exec_command.rs
    blob-sha:       6b54a2beb543c9498419502ee23bb39d834568e6

    path:           codex-rs/core/src/tools/handlers/apply_patch.rs
    blob-sha:       f1559f540702567cf446b2107ed00f9bbb706f98

    path:           codex-rs/core/src/hook_runtime.rs
    blob-sha:       f3a0ec11c90d9115a95b658f1882a231c36e9197

    path:           codex-rs/apply-patch/src/parser.rs
    blob-sha:       c400d075a684fda29c10269aef1958a27faf90aa

Re-verify with no judgement involved: clone the tag above, then
`git hash-object <path>` for each; every one must print the sha beside it. The
same tag carries the output-surface pin beside this file, so the two fixtures
stand or fall on one clone. The installed client reports `codex --version` →
`codex-cli 0.25.0`, recorded as an observation: the published tags and the CLI's
own version string are numbered separately.

### The canonical tool names, and the two that are only matcher aliases (`hook_names.rs`, upstream lines 1-6, 8-12, 29-39, 53-56)

```rust
//! Hook-facing tool names and matcher compatibility aliases.
//!
//! Hook stdin exposes one canonical `tool_name`, but matcher selection may also
//! need to recognize names from adjacent tool ecosystems. Keeping those two
//! concepts together prevents handlers from accidentally serializing a
//! compatibility alias, such as `Write`, as the stable hook payload name.
```

```rust
/// Identifies a tool in hook payloads and hook matcher selection.
///
/// `name` is the canonical value serialized into hook stdin. Matcher aliases are
/// internal-only compatibility names that may select the same hook handlers but
/// must not change the payload seen by hook processes.
```

```rust
    /// The serialized name remains `apply_patch` so logs and policies can key
    /// off the actual Codex tool. `Write` and `Edit` are accepted as matcher
    /// aliases for compatibility with hook configurations that describe edits
    /// using Claude Code-style names.
    pub(crate) fn apply_patch() -> Self {
        Self {
            name: "apply_patch".to_string(),
            matcher_aliases: vec!["Write".to_string(), "Edit".to_string()],
        }
    }
```

```rust
    /// Returns the hook identity historically used for shell-like tools.
    pub(crate) fn bash() -> Self {
        Self::new("Bash")
    }
```

**`Write` and `Edit` never arrive as a `tool_name`.** They select a handler and
are then dropped; the payload always says `apply_patch`. A table that recognises
an edit by those two names on this family recognises nothing — the names are
real, but they live on the matcher side of the wire, and this is the sentence
that says so.

### What `tool_input` holds for each of them (`exec_command.rs` upstream lines 489-500, `apply_patch.rs` upstream lines 469-474)

```rust
    fn pre_tool_use_payload(&self, invocation: &ToolInvocation) -> Option<PreToolUsePayload> {
        let ToolPayload::Function { arguments } = &invocation.payload else {
            return None;
        };

        parse_arguments::<ExecCommandArgs>(arguments)
            .ok()
            .map(|args| PreToolUsePayload {
                tool_name: HookToolName::bash(),
                tool_input: serde_json::json!({ "command": args.cmd }),
            })
    }
```

```rust
    fn pre_tool_use_payload(&self, invocation: &ToolInvocation) -> Option<PreToolUsePayload> {
        apply_patch_payload_command(&invocation.payload).map(|command| PreToolUsePayload {
            tool_name: HookToolName::apply_patch(),
            tool_input: serde_json::json!({ "command": command }),
        })
    }
```

One argument, named `command`, on both — a shell command line for `Bash`, a
patch document for `apply_patch`. The hook runtime treats the two the same way
when it reports a declined call, which is the third file confirming the field
name from the other side (`hook_runtime.rs`, upstream lines 229-231):

```rust
    if (tool_name.name() == "Bash" || tool_name.name() == "apply_patch")
        && let Some(command) = tool_input.get("command").and_then(Value::as_str)
    {
```

### The patch document's grammar (`parser.rs`, upstream lines 1-25)

```rust
//! This module is responsible for parsing & validating a patch into a list of "hunks".
//! (It does not attempt to actually check that the patch can be applied to the filesystem.)
//!
//! The official Lark grammar for the apply-patch format is:
//!
//! start: begin_patch environment_id? hunk+ end_patch
//! begin_patch: "*** Begin Patch" LF
//! environment_id: "*** Environment ID: " filename LF
//! end_patch: "*** End Patch" LF?
//!
//! hunk: add_hunk | delete_hunk | update_hunk
//! add_hunk: "*** Add File: " filename LF add_line+
//! delete_hunk: "*** Delete File: " filename LF
//! update_hunk: "*** Update File: " filename LF change_move? change?
//! filename: /(.+)/
//! add_line: "+" /(.+)/ LF -> line
//!
//! change_move: "*** Move to: " filename LF
//! change: (change_context | change_line)+ eof_line?
//! change_context: ("@@" | "@@ " /(.+)/) LF
//! change_line: ("+" | "-" | " ") /(.+)/ LF
//! eof_line: "*** End of File" LF
//!
//! The parser below is a little more lenient than the explicit spec and allows for
//! leading/trailing whitespace around patch markers.
```

**What Rift takes from this grammar, and what it refuses to take.** One hunk
header names one file, and the marker is a fixed prefix at the start of a line,
so a patch carrying exactly one `*** Add File: ` or `*** Update File: ` header
and no `*** Move to: ` names exactly one path — that path is read.

An update's `change` lines are read for one thing only: the new side of each
chunk. A chunk runs from one `change_context` line to the next (or to
`eof_line`), and its context and `+` lines, in order, are the text that chunk
leaves in the file; its context and `-` lines are what it took out. No line
number is taken from the patch — the patch states none. The new side is looked
for in the bytes on disk after the patch landed, under the same exact-match
rule a single replacement is held to, and a chunk that cannot be found exactly
once, or that overlaps another, leaves the whole patch recorded as *this file
changed, site unknown*. A line in an update body that is none of the grammar's
forms does the same, and so does an add section: its whole new file is not
read here.

A patch that moves a file, or touches two, is not recorded at all: one request
carries one path, and the safe direction when a payload carries more is to say
nothing rather than to pick one.

## What this family has no tool for

- **A read.** There is no structured read tool; a read is a `Bash` command
  line, and recognising one means recognising a shell grammar. The grammar
  recognised is the one another vendor publishes for its own read-before-edit
  check, pinned in `claude-code-tool-field-surface.md` — a single simple
  command with no pipes or redirects — so `Bash` is `shape=shell` below: read
  when the line matches that grammar exactly, left alone otherwise.
- **A search.** Likewise: a glob, a listing and a grep are all `Bash`. Only
  the three listing forms are recognised; a content search stays one.
- **A result cap.** With no search tool there is no cap argument, so the
  additive rewrite available on another family has nothing to attach to.

Each of those is a pinned absence rather than a missing pin, and each is the
reason this family's savings come from what it is told at a turn boundary
rather than from what it is declined mid-turn.

## Tool fields

Each row states one tool this adapter recognises and what it recognises it as.
A row exists only because an excerpt above pins it, and the
`pinned_tool_field_surface` tests assert this block and the code table are the
same set in both directions.

```rift-tool-field-rows
tool=apply_patch shape=edit kind=regions command=command path=from-a-single-file-header regions=update-chunks
tool=Bash shape=shell command=command grammar=composed-read-and-listing
```

Two rows, and the second one is the finding.
