# 10. Agent integrations

Which files Rift writes into your repository, which it only ever *reads*, how
it proves a file is its own before touching it again, and why six of the nine
adapters it ships are refused. [01-quickstart.md](01-quickstart.md) wires a
repository end to end.

---

## 10.1 The shape: registered but refusing

Rift ships nine agent adapters — `claude-code`, `codex`, `gemini`,
`antigravity`, `cursor`, `copilot-cli`, `opencode`, `factory-droid`, `ollama` —
and all nine are **registered**: they appear in `rift recall status`, in
Canopy's Project Settings list, and in the adapter registry behind every
integration command. Exactly three can be **enabled**: `claude-code`, `codex`,
`antigravity`. Everything else is refused politely, by name, and *before a
single byte is written*, because both easier options are worse. **Hiding the
six** turns "not supported yet" into "does not exist", so a user with Cursor
installed gets silence — which is what a broken integration also looks like.
**Installing them** produces a file that looks like a working integration and
captures nothing, and a ledger with quiet holes is worse than none, because you
trust it.

So the refusal is loud and the row stays visible. `rift init` names the
coming-soon agents it detected and left alone; `rift enable` on one of them
prints the reason and the supported list rather than writing a descriptor for a
hook surface the agent does not send.

> Tip: detection is not eligibility. `rift init` wires the *supported* agents it
> finds and names the others; having Ollama installed does not fail the run.
> Name an unsupported agent explicitly and `init` refuses the whole run **up
> front**, before writing anything, naming every offender.

---

## 10.2 The three wired clients

For each wired agent Rift writes its own descriptor and the client's native
hook configuration:

| Client | Rift's descriptor | Client hook file Rift writes | Recall instructions channel |
|---|---|---|---|
| Claude Code | `.rift/agents/claude-code.json` | `.claude/settings.local.json` | `.claude/skills/rift-recall/SKILL.md` |
| Codex | `.rift/agents/codex.json` | `.codex/hooks.json` | a managed block inside `AGENTS.md` |
| Antigravity | `.rift/agents/antigravity.json` | `.agents/hooks.json` — explicit enable merges the reserved `rift-recall` bundle | `.agents/rules/rift-recall.md` |

Each of the three then needs one act from you, which Rift prints as a follow-up
and never performs itself (§10.3): Claude Code and Antigravity ask you to
restart and approve the project's or workspace's hooks when prompted, and Codex
asks you to trust the project and restart before its hook status is worth
reading. Until that act happens the wiring is present and inert, which is
exactly what `rift recall status` reports it as.

### The descriptor

`.rift/agents/<agent>.json` is Rift's record of what it wired, generated and
safe to delete — `rift enable` recreates it:

```json
{
  "schema_version": 1,
  "managed_by": "rift",
  "agent": "codex",
  "rift_bin": "/path/to/rift",
  "hooks": [
    { "verb": "SessionStart", "command": "/path/to/rift hooks codex SessionStart" }
  ],
  "compat": {
    "best_effort": false,
    "surfaces_token_usage": true,
    "lifecycle_gaps": ["token usage is optional and client-version dependent"]
  }
}
```

**The binary path is absolute** rather than trusting the agent's `PATH` when a
hook fires: agents launch with environments you did not choose, and an absolute
path is the only spelling true in all of them. It is also why moving or
rebuilding your `rift` binary is something Rift notices and reports — see the
binary-of-record rules in
[09-worktrees-and-families.md](09-worktrees-and-families.md).

**The verb list is not a second table.** The descriptor's hooks and the hooks
written into the client's configuration are both projections of the adapter's
declared verb list; there is exactly one list per agent, in the adapter,
because a second copy drifts silently and advertises captures no hook was
registered for. A test reads the fragment the installer *actually wrote*, not
the function meant to write it.

### The client hook file

Each registered verb becomes one command entry in the client's hook
configuration:

```json
{
  "type": "command",
  "command": "\"/path/to/rift\" hooks claude-code PostToolUse",
  "timeout": 10
}
```

Tool events (`PreToolUse`, `PostToolUse`, and their equivalents) are wrapped in
a group with `"matcher": "*"`; every other event takes the bare handler. The
verb counts are nine for Claude Code, ten for Codex, five for Antigravity —
[03-capture.md](03-capture.md) has the per-verb breakdown. Antigravity's file
differs: its whole bundle lives under a reserved `rift-recall` key with an
`"enabled": true` flag beside the events, because that is the contract its hook
API publishes, and the recogniser never reads one shape as the other.

---

## 10.3 What Rift never touches

Short on purpose, and none of it bends:

- **Global client configuration** — `~/.codex/config.toml`, home-directory
  settings, machine-wide profiles. Rift reads some (§10.5) and writes none;
  project configuration is the only thing it is authoritative over.
- **Trust and approvals** — Rift never marks its own hooks approved inside a
  client; it reports the state and tells you where to grant it.
- **Authentication, credentials, binaries, versions** — no logging in, no
  upgrades, no replacements.
- **Running processes** — nothing is restarted, killed or reconfigured while it
  runs; a needed restart is printed as a follow-up, not performed.
- **Anything Rift did not write** — covered next.

The reason is the same each time: these are your decisions and the client's own
state, and a capture tool that silently grants itself trust is one you cannot
audit ([17-invariants.md](17-invariants.md)).

---

## 10.4 Custody: proving a file is Rift's own

Every write into your repository answers one question first: *is the thing
already there mine?* The ownership manifest at
`.rift/state/native-integrations.json` records a content fingerprint for every
artefact Rift installed — descriptors by agent token, hook fragments by client
family, instructions blocks by agent token. A file whose
bytes still hash to the recorded fingerprint is Rift's own and may be
rewritten; one that does not is a **conflict**: reported, never replaced.

### When the manifest cannot vouch

`.rift/state/` is not committed, so a file Rift genuinely wrote can have no
manifest entry — a fresh clone with committed descriptors, a wiped state
directory, an install predating the manifest. Refusing those would strand users
on files Rift produced, so other proofs exist, any one sufficient.

**For descriptors**, the document is recognised by *shape*: its key set is
exactly the one Rift writes, `agent` matches this token, the binary path is
absolute, and every hook entry is precisely `{verb, command}` spelled
`"<binary> hooks <token> <verb>"`. That check deliberately ignores the verb
list — the alternative breaks the day a verb table is corrected, which is when
you most need it to work.

**For hook fragments**, the recogniser *re-emits* from the candidate's own
event keys: it pulls the absolute binary out of the first Rift command it
finds, then requires every group to equal exactly what Rift would write for
that event and that binary. A fragment from an older, smaller verb table still
recognises; a single altered byte — an edited timeout, a second handler in the
group, an event key disagreeing with its own command — does not.

Two read-only proofs remain for the Claude Code fragment, left by an older
installer: `.rift/agents/recall-install.json` and a sidecar copy of the
fragment. Nothing writes either today; they are read anyway, because a dead
file that can still prove a fragment is ours beats a tidy directory.

**Symlinks.** Both the hook file **and the project directory containing it**
are checked before anything is written through them, and a symlink at either is
refused outright — that is how a write meant for your repository lands
somewhere else.

### Automatic adoption never writes tracked files

Antigravity's `.agents/hooks.json` is a file repositories routinely commit.
The explicit `rift enable --agent antigravity` command fingerprint-merges only
the reserved `rift-recall` named bundle and preserves unrelated named hooks.
Malformed files, symlinks, and edited Rift bundles are refused without a
partial write. Automatic worktree adoption remains read-only: it verifies the
tracked hook and instructions files and reports missing or stale state instead
of dirtying a newly created worktree.

**A tracked bundle from another machine is stale, never foreign.** Because the
file is committed, the bundle a colleague's `enable` wrote travels with the
clone, naming *their* absolute `rift`. Ownership is proved by shape, and one
part of that proof is that the quoted binary is absolute — a check that used to
be answered by the reading platform, so a macOS path failed it on Windows and a
Windows path on macOS, and a bundle Rift itself wrote read as a foreign
**conflict** the moment a repository crossed platforms. The check now asks
whether the path is absolute on the platform that *wrote* it. The result is
the refreshable class (`hooks need refresh`), and re-pointing stays an explicit
act: `rift init` and the setup wizard report the foreign path and leave the
file alone, and only `rift enable --agent antigravity` or the Project Settings
toggle rewrites it — otherwise two machines' setups would commit the path back
and forth forever.

**Conflicts are named, not resolved.** A refused write names the file and the
reason (`Codex hooks are not safely Rift-managed; repair .codex/hooks.json and
retry`), and unrelated entries are always preserved: Rift rewrites only the
groups it recognises as its own, and passes everything else through untouched.

---

## 10.5 Detection: one rule, four kinds of evidence

"Is this agent installed here?" is answered by **one predicate** that every
surface reads — `rift init`'s auto-wire, Canopy's pre-ticked setup rows,
`rift doctor` and `rift recall status`. It used to be three: `init` looked at
`PATH` alone, Canopy also counted a Codex editor extension, and doctor scanned
`PATH` through a second scanner that knew about `.exe` where the first did
not. So one clone read "Codex: detected" in the dashboard and "no new
selected agents" from `init`, and a Claude Code that lived only inside the
VS Code extension — whose bundled `claude` is nowhere on `PATH` — was
invisible everywhere. The rule is now the same for Claude Code, Codex and
Antigravity, and it reads four kinds of evidence, reported in this order:

| Evidence | What is looked for | Version |
|---|---|---|
| `PATH command` | An executable of the agent's name on `PATH` — `claude`, `codex`, `antigravity`/`agy` — in any of the three spellings a command has on disk: bare, `.exe`, and the `.cmd` shim npm writes on Windows. | none (Rift never runs what it finds here) |
| `vendor install` | The vendor's own install location: Claude Code's native installer in `~/.local/bin`; Antigravity's application bundle under the platform's application root, verified by its own `product.json`; Codex's desktop application bundle, verified by its bundle identifier. | from the bundle's manifest where it has one |
| `<host> extension` | A verified extension under each VS Code-compatible extension root (below), for Claude Code (`Anthropic.claude-code`) and Codex (`openai.chatgpt`). The bundled binary, where the extension ships one, is the runnable Rift reports. | the extension's |
| `JetBrains plugin` | Claude Code's plugin directory under each JetBrains IDE's plugin root, holding at least one jar. The plugin ships no binary, so this only ever says "installed". | from the jar's file name when it carries one |

| Editor | Extension root |
|---|---|
| VS Code | `~/.vscode/extensions` |
| VS Code Insiders | `~/.vscode-insiders/extensions` |
| Cursor | `~/.cursor/extensions` |
| Windsurf | `~/.windsurf/extensions` |

"Verified" means the directory's own package manifest declares the expected
publisher and extension name, not that it is merely named plausibly. Symlinked
extension directories and manifests are skipped, and where several versions sit
side by side the highest wins and is what gets reported. Every probe checks
every platform layout it knows whatever platform it runs on, so a fixture built
on Linux for the macOS bundle layout finds what the macOS box finds, and no
test reads the machine it runs on: the `PATH`, the home directory and the
application roots are injected.

**Detected is any of the four.** A single sentence spells the evidence on
every surface — `present via VS Code extension 2.1.267`, `present via PATH
command + vendor install`, or `absent` — so that when `init` wires an agent
you did not expect, the reason it believed the agent was installed is on the
screen, in the same words, wherever you look. Availability is all it claims:
nothing here says the agent is in use, and detection is still not
eligibility — a detected coming-soon agent is named and left alone (§10.1).

**Codex's third fact: hook trust.** Codex trusts a project's hooks **per absolute path, per hook and per hash**, recorded in its own home-directory configuration (`[hooks.state]` in `~/.codex/config.toml`, or under `$CODEX_HOME`).
A correct hook file is therefore not sufficient: Codex runs a hook only while the hash it recorded when you trusted it equals the hash of the hook as it now stands, and it skips any other hook without a message.
A hook Rift rewrites — a new field, a new binary path — is skipped from that moment until you trust it again.

Rift reproduces Codex's hash for every hook it wrote into `.codex/hooks.json` and compares it, read-only, against Codex's record.
Each hook gets one of four verdicts: **trusted** (the hashes match), **modified** (a hash is recorded and differs), **untrusted** (no hash is recorded), or **unverifiable**.
The hash is Codex's normalised identity of the hook — the timeout it would apply, the context limit only where the event can return context and only when it differs from Codex's default, the matcher only where the event dispatches on one — and Rift reproduces it only for the Codex versions its fixtures were recorded against.
Which version runs here, and whether it runs in a terminal or in the VS Code extension, is read from the newest Codex session Rift has recorded in the checkout: the session file Codex names on its hook payloads opens with Codex's own record of both.
For any other version, or before any Codex session has been recorded, every hook is unverifiable, and Rift says so in those words rather than guessing.

The comparison runs at the two moments you are looking: every write of the hook file (`rift init`, `rift enable`, `rift recall init`, Project Settings' setup dialog, worktree adoption) and every status read (`rift status`, `rift doctor`, `rift recall status`, `rift worktree status`).
It never runs on a hook's decision path.
When any of Rift's hooks is modified or untrusted, the Codex row reads `error` rather than `configured`, and one remedy sentence is printed (and returned in `--json` as `codex_trust_remedy`):

> Codex is skipping 2 of Rift's hooks because they changed after you approved them, so Rift cannot see your Codex sessions start. In a terminal in this repository run `codex`, type `/hooks`, and trust the hooks marked 'Modified since last trusted'.

When the recorded session ran in the VS Code extension, the sentence adds: *Then reload the VS Code window and start a new Codex conversation.*
Rift cannot trust a hook on your behalf and never writes Codex's configuration; the sentence is how the write tells you, at the write.

> Tip: this is what surprises people in linked worktrees. A new worktree's
> `.codex/hooks.json` is a **new absolute path**, so it starts untrusted even
> though Rift wired it perfectly. Approve it once in Codex, in that worktree.
> [09-worktrees-and-families.md](09-worktrees-and-families.md) covers the rest
> of the member lifecycle.

**When a Codex hook is skipped, the session and its turns still open.** Codex
drops a hook it no longer trusts without an error and runs the rest, and it runs
its `SessionStart` hook only once a turn starts, so a session can reach Rift as
tool events alone, or as a lone `SessionEnd`. The session opens on its first
captured event (the transition is recorded as a `synthesized-start`), and a turn
opens on first activity when no prompt hook opened one: a tool call or a turn
end at turn 0 opens turn 1. After that, Codex's own `turn_id` on each hook is
the turn's identity — a new `turn_id` opens the next turn with no prompt hook in
sight — so its turns are counted and closed like any other agent's. A lone `SessionEnd` opens and ends the session in one pass.
[03-capture.md](03-capture.md) §3.6 has the state machine this rests on.

---

## 10.6 The six that are refused, and exactly why

The six unwired adapters are two groups with two reasons, and conflating them
is how a rationale goes stale.

### Four are refused by measurement

Cursor, Copilot CLI, OpenCode and Factory Droid each declare a hook surface the
agent does not send — not a partial overlap, a mismatch:

| Adapter | What Rift declares | What the agent actually publishes |
|---|---|---|
| Cursor | three lower-case verbs (session start, edit, session end) | seven camel-case events |
| Copilot CLI | four verbs (session start, prompt, tool, session end) | eight camel-case events |
| OpenCode | four dotted verbs (session start, message, tool execute, session end) | five plugin events, with no tool boundary at all |
| Factory Droid | five lower-case verbs (session start, task start, tool use, task end, session end) | nine capitalised events, Claude-Code-shaped |

An install for any of these would succeed, write files, report green, and
capture *nothing* — every hook fired would land as an unrecognised verb. The
refusal follows from a fact about the agent, not an opinion about it.

### One is refused because it has no lifecycle at all

Ollama is a local model runner, not a coding assistant: no sessions, no turns,
no tool boundaries, no compaction. Its adapter maps the only two things a proxy
in front of it can observe — a request and a response — and says so rather than
implying a session shape it cannot see. Absence is the correct outcome, not a
gap to close.

### One is refused by scope decision

Gemini CLI is out of the supported set by decision, not by measurement. Its
adapter has two real defects — it maps the one event carrying the model
identifier to zero canonical events, so a Gemini session is captured with no
record of which model ran, and it declares nine of the eleven hook events
Gemini publishes, so two land as unrecognised verbs — ordinary bugs that would
be fixed if the agent were in scope.

They are **not the reason** Gemini is unwired, and saying they were would be a
sentence that outlives its facts: fix the bugs and the row is still unwired, at
which point the stated reason is a lie in the tree. Both defects stay **counted
as open** ([18-known-gaps.md](18-known-gaps.md)), because declaring an agent
unsupported withdraws the promise to fix something without fixing it.

---

## 10.7 Deterministic context delivery

Rift computes one transport-independent `ContextPlan` at native hook
boundaries. The dispatcher, not the model and not a rules-file prompt, chooses
between a pending read replacement, recovery map, session pointer, and delta
manifest in deterministic priority order. Each adapter only encodes that plan
into its client's documented response schema.

### A channel exists only where a fixture pins it

An adapter may answer on a verb only when a committed fixture carries that
client's **published response schema** for it: the repository, the tag or
commit, the file path, the blob hash, and a verbatim excerpt a reader can go
and re-check. A conformance test asserts the bytes Rift emits per client and
verb against those rows, and a table in the tree lists the channels answered
on, checked by its own build gate. The rule this enforces is short — *no
response on a channel no published artifact pins* — and it is why the
per-client differences below are facts about documents rather than opinions
about clients.

Pinning is cheap and being wrong is bounded: a client that does not understand
a response ignores it, and the hook has already exited successfully.

### Every lever, per client

The three supported clients do not offer the same surface, and Rift uses each
one to the full extent of what it documents and no further:

| Lever | Claude Code | Codex | Antigravity |
|---|---|---|---|
| Reads recorded with ranges | its read tool's own fields | strict-grammar shell reads | its file-view tool's range fields |
| Edits recorded as coverage | yes | patch application | its two write tools |
| Declining a re-read | documented decision + reason | documented decision + reason | documented decision + reason |
| Naming the changed lines instead | yes | yes | yes |
| Boundary context | prompt submission, session start, and the hook that opens a subagent's window | prompt submission, session start, with a declared limit | before each model invocation |
| Recovery after a compaction | its own compaction source | after its compaction signal | at the first invocation of a window |
| Amending a tool call | documented on its read and search tools | documented on a shell command | not offered — nothing is amended |
| Bounding a content search | its search tool's own result count | not offered — no bound is added | not offered |
| Answering a name search | its glob tool, and shell listings in the published grammar | shell listings in the published grammar | its file-finding and directory-listing tools |
| Documents | the read is amended to the sidecar | proactive sidecars, redirected once | proactive sidecars, redirected once |
| Pull verbs and instructions | all | all | all |

Two asymmetries are worth stating plainly, because they explain the whole
shape. **Only one client documents amending its file-read tool's input**, so
document substitution happens there and nowhere else: the second client's
amendment applies to a shell command, where it replaces a redundant read with
the bounded result the shared plan already chose, and the third amends nothing
at all. Documents reach both of those as sidecars that already exist plus a
decline that names them. And **one client's boundary
channel declares its own size limit**, written by Rift when it installs the
hooks: over that limit the client keeps a head and a tail, annotates the
truncation and spills the rest to a temporary file, so Rift fits its text to
whichever is smaller, its own budget or the limit it declared.

### What was measured on the real clients

Each channel above was exercised against the installed client before anything
shipped on it, in a scratch repository outside this tree, with the hooks
installed only there. What those runs settled:

- **The reason reaches the model on all three.** On one it arrives as the
  cancelled call's own result, byte-identical to what Rift emitted, with no
  wrapper. On the second it arrives as a tool error prefixed by the client's
  own sentence. On the third the client wraps Rift's text front and back and
  refuses an empty reason outright — so a golden test asserting the model sees
  *exactly* the frame would be false there, and the assertion is containment
  instead.
- **The repeat escape is used differently by different families.** Eleven runs
  on the first re-issued nothing after a decline and answered correctly from
  context; ten runs each on the other two re-issued the identical call at once
  and were allowed it. The loop stayed bounded to one extra round trip
  everywhere, which is the property that matters; the rate is telemetry.
- **A subagent has exactly one boundary, and it is not either of the two a
  session has.** Two headless runs on the first client: the hook that opens a
  subagent's window fired with the *parent's* session id beside the child's own
  agent id, the context field on it reached the subagent, and the text was still
  held after the subagent's own tool calls — while neither the session-start nor
  the prompt-submission hook fired for that window at all. Which is why the
  pointer's claim, the manifest baseline and the window journal are all keyed by
  the same (session, agent) pair rather than by the session.
  The same sharing means a subagent's hooks write the parent session's cache
  and sidecars at the same moment as the parent's own.
  That is safe because every writer renames a temp file of its own, so the
  last rename publishes one whole file, and a cache that is torn anyway is
  moved aside by the next hook rather than failing it (see
  [03-capture.md](03-capture.md) §3.6).
- **An injected step's kind decides whether it survives.** On the
  per-invocation channel, an ephemeral message reaches the model at the
  invocation it is injected into and is gone by later ones, while a user
  message persists — so anything a later turn must still know is injected as
  the persistent kind, and presence in a transcript is not evidence of
  visibility.
- **Amendments were not dropped on the version measured**, ten runs of ten with
  a second, unrelated hook registered on the same verb — which is a fact about
  one client version rather than a schema guarantee, so the verification and
  redirect fallback of [07-serving-and-injection.md](07-serving-and-injection.md)
  stay load-bearing.
- **A reason that is seen is not a reason that is followed.** Three runs on the
  installed client whose documents reach it as a decline rather than an
  amendment: the frame reached the model in all three, one of the three then
  read the Markdown sidecar the frame named, and no run of the three ended up
  booking a saving. The channel works and the lever is honest about what it is
  worth there, which is why nothing is defaulted on the strength of a channel
  existing — the run is evidence that the model *can* be told, not that being
  told changes what it does.
- **Two clients cannot open a document at all.** One refuses the format
  outright; the other returns a PDF's raw bytes as numbered text lines. That is
  the whole case for proactive sidecars rather than a conversion offered on
  request.

### The instructions file

Every wired agent gets a short managed block in whichever file its client reads
for project instructions:

| Client | File | How it is written |
|---|---|---|
| Claude Code | `.claude/skills/rift-recall/SKILL.md` | Rift creates the file, with the leading metadata block its client uses to discover the skill. |
| Codex | `AGENTS.md` | Appended as a marked region; the rest of your file is untouched. |
| Antigravity | `.agents/rules/rift-recall.md` | Rift creates the file. |

The block is delimited by markers naming the agent —
`<!-- RIFT-MANAGED:codex:begin -->` and its `:end` twin — and carries the same
short fallback text: it identifies Rift, points to `rift agent-help` and
`rift recall status --json`, and explains degraded-mode recovery.

Re-installing rewrites **only what is between the markers**, so in `AGENTS.md`
everything outside them is yours and in a file Rift created a description you
edited outside the fence survives. Removing the integration removes the region,
and deletes the file only when Rift created it and nothing else is left. If an
unowned, unmarked file already sits at a path Rift wants (`SKILL.md`, the rules
file), initialization **stops there** rather than adopting it.

No capture, context delivery, interception, or recovery behavior depends on
the block. Removing or ignoring it changes documentation only.

### Native boundary hooks

Claude Code and Codex plan context automatically at `SessionStart` and
`UserPromptSubmit`; Antigravity does so at `PreInvocation`. Antigravity's
tool-call step execution/result semantics are not pinned, so bounded prepared
reads use its persistent `userMessage` step after the raw call is declined.
If a replacement cannot be delivered, the identical raw call is allowed once.
Hook failures always fail open and make support status degraded.

For Codex `PreToolUse`, Rift rewrites only a strict recognized redundant shell
read into a shell-safe `printf` of the bounded result chosen by the shared
`ContextPlan`. Recorded Codex 0.153.1 probes show that every sibling hook sees
the original input and the first configured `updatedInput` wins, so explicit
enablement places Rift's owned group first while preserving the relative order
of every unrelated group. Ambiguous shell commands and reads without proven
coverage run untouched.

---

## 10.8 Enabling and disabling

```bash
rift enable  --agent claude-code
rift disable --agent claude-code
```

`enable` does five things, in this order:

1. Refuses if the agent is unknown, declares no verbs, or is not supported —
   **before** any file is created, including the directory.
2. Writes the descriptor and the client hook file, refusing on the first
   conflict, so a refusal leaves the tree exactly as it was.
3. Writes the Recall instructions block, if Recall is enabled here.
4. Records the change as an append-only enablement event in the projection.
5. Carries the same wiring to every other worktree in the family.

Its output names each file written and each worktree reached. `disable` is the reverse and removes **only** fingerprint-matching artefacts; a
descriptor you edited is refused and left byte-identical. Captured data and the
Recall projection are never touched — disabling stops future capture, it does
not erase the past
([05-ledger-and-verification.md](05-ledger-and-verification.md)).

### Why enabling reaches other worktrees

`rift enable` records the **family's** intent: enablement is one relation for
the whole repository, not one per checkout, so an existing linked worktree is
wired by the same command. Three rules the fan-out does not bend:

- The binary written into another member is the family's **binary of record**,
  never the executable that ran the command: enabling from a throwaway build
  must not wire the family to a build that will be deleted.
- A member that refuses — a conflicted fragment, an unowned descriptor — is
  **named, not overwritten**, and does not fail an enable that already
  succeeded locally, because the local act happened and is recorded truthfully.
- Tracked files and git hooks are untouched, because the fan-out goes through
  the same installer and inherits its refusals.

`disable` is symmetric: from any member it unwires every member, so an agent
cannot stay live in a worktree nobody is looking at.

---

## 10.9 The same lifecycle from Canopy

Project Settings (the Repository card, `s`, or `/settings`) lists every
registered adapter in registry order and toggles them through **exactly the
same** code path the CLI uses — there is no second installer behind the
dashboard. A not-yet-supported row says so rather than implying a wiring the
toggle is guaranteed to refuse:

```
claude-code · VS Code extension · managed
gemini · PATH command · coming soon
cursor · not installed · conflict · coming soon
```

Note the third: a leftover artefact's state stays visible *beside* the scope
fact, so a conflict in your tree reaches you even for an agent Rift will not
wire. The refusal is predicted in the row's description rather than discovered
by pressing the key, and checked before the projection opens, so the toggle
fails as a plain precondition, not a storage error naming a file nothing wrote.
[11-canopy-tour.md](11-canopy-tour.md) walks the surface.

---

## 10.10 Reading the state

**`rift recall status`** prints one row per adapter — all nine, always, in
registry order — under three columns, independent on purpose:

| Column | What it means |
|---|---|
| descriptor | Rift's own file: `missing`, `managed`, `conflict`, or `invalid`. |
| hooks | The client's native hook file: `missing`, `managed`, `stale`, `conflict`, or `invalid`. |
| installed | The detection sentence of §10.5 — `absent`, or `present via` each piece of evidence found, with its version where a manifest declared one. Availability only; it says nothing about wiring. |

The JSON view also separates `support`, `hook_configuration`,
`runtime_connection`, `context_delivery`, `requested_mode`, `effective_mode`,
and `degraded_reason`. A descriptor alone is never enough for `full` support:
the native hook must be configured, a runtime receipt observed, and a boundary
response observed. Until then the row is `configured-unverified`; a hook
failure makes it `degraded`. The six placeholder adapters report
`coming-soon` and cannot be enabled.

`stale` is recoverable — an earlier Rift bundle a re-run refreshes safely;
`conflict` needs you.

**`rift doctor`**'s integrations leg is coarser and older (§10.11); its useful
signal is the per-client follow-up line (`action: repair Codex hook
configuration`).

**`rift worktree status`** reports the same wiring per member, read-only: where
each agent's hooks point, whether a recall skill is present, and whether Codex
trust is approved or pending
([09-worktrees-and-families.md](09-worktrees-and-families.md)).
**`rift agent-help`** renders the agent-facing verbs from the installed
binary's own command tree, and is what the instructions block points agents at
([07-serving-and-injection.md](07-serving-and-injection.md)).

---

## 10.11 Rough edges

**`rift doctor`'s integrations leg reports four clients, not nine**, and is not
the registry-driven view: its `probe` and `connection` columns are vestigial,
describing a server handshake and a connection lease that no longer exist, so
their values are whatever was last recorded or a permanent "never seen". It
also asks Codex for a version at or above a floor today's releases do not
reach. Neither gates anything — `rift recall status` is the real picture, and
`rift enable` ignores the floor.

Two artefacts of a retired four-client installer are still read as ownership
proofs (§10.4) and written by nothing, because a machine that ran it still
carries them. The instructions-file paths name exactly the three adapters whose
blocks can be written; there is no path defined for an adapter Rift refuses.

---

## Related chapters

[01-quickstart.md](01-quickstart.md) ·
[02-configuration.md](02-configuration.md) for the `.rift/agents/` and
`.rift/state/` roots · [03-capture.md](03-capture.md) ·
[07-serving-and-injection.md](07-serving-and-injection.md) ·
[09-worktrees-and-families.md](09-worktrees-and-families.md) ·
[18-known-gaps.md](18-known-gaps.md).
