# 3. Capture

Capture is the part of Rift that watches. It turns two streams of small,
untrusted signals — the ones your git client emits and the ones your coding
agent emits — into one immutable record of what happened in this repository:
canonical events, sessions, and checkpoints on a ledger you can verify.

This chapter walks the whole path — a hook fires, an event is derived, a
session advances, a commit condenses into a checkpoint — and the constraint
shaping almost every choice here: **capture is not allowed to be slow, and it
is not allowed to fail your work**. Run [`rift init`](01-quickstart.md) first;
this chapter describes what that command wired up.

---

## 3.1 The shape of capture

Everything in this chapter is one pipeline, from raw capture to a sealed
checkpoint on the ledger.

**The ledger is the record; the database is a projection.** Checkpoints are
git objects on a ref of Rift's own; the local `projection.db` is a queryable
view over that record and can be deleted and rebuilt.

**Every hook is a short-lived process.** No background service: a hook starts,
reads at most one JSON object from standard input or a handful of arguments
from git, writes what it can, and exits. Work that cannot fit is *journaled* —
written down durably — and performed by a detached one-shot child, a deliberate
refusal to ship a daemon whose costs §3.10 sets out.

> **Tip:** To watch the pipeline run, try `RIFT_HOOK_TIMING=1 git commit` in a
> repository with a live session: each instrumented hook prints one line to
> standard error naming where its milliseconds went. Unset — what every real
> commit runs — the timer takes no clock reading at all.

---

## 3.2 Two kinds of hook, one rule

Rift installs into two different hook systems, and they behave differently.

| | **git hooks** | **agent hooks** |
|---|---|---|
| Installed into | the repository's hooks directory | your coding agent's project config |
| Invoked as | `rift hooks git <name> …` (or a direct link) | `rift hooks <agent> <verb>` |
| Input | git's positional arguments, sometimes stdin | one JSON object on stdin |
| Fires on | commit, push, checkout, merge, rewrite | session start, prompts, tool calls, turn end, compaction, session end |
| Can refuse? | only `pre-push`, and only for one thing | never |

They share the important rule: **a hook never blocks the work that triggered
it.** Every commit-path git hook runs its body inside a guard that catches even
a panic and exits zero, so a Rift bug can defer capture but never fail your
`git commit`; agent hooks do the same and always exit zero, swallowing a
capture error and reporting the work as deferred. The one exception is
`pre-push`, which may exit non-zero — and only to refuse pushing Rift's own
internal refs, never your branches. Not politeness: capture is a side channel,
and a side channel that can break the main channel is worse than none, because
you uninstall it the first time it costs you a commit and then you have
nothing.

---

## 3.3 The five git hooks

`rift init` (or `rift hook install`) writes five entries. The list *is* the
dispatch table — a direct link is routed by the name it was invoked under — so
name and behaviour cannot drift apart.

| Hook | What Rift does |
|---|---|
| `prepare-commit-msg` | Adds the `Rift-Checkpoint` and `Rift-Attribution` trailers when a session is live; the checkpoint trailer is a claim, made before the commit exists. Only rewrites the message file; never rejects. |
| `commit-msg` | Normalizes those trailers idempotently — deduplicated, canonical order. Never rejects. |
| `post-commit` | The main capture beat: records which commit and which session this was, then hands the heavy work to a detached worker. |
| `pre-push` | Screens the refs about to leave the machine and refuses Rift's internal ones, treats the push as the "work is leaving" signal for the on-push strategy, and drains the push queue. |
| `post-checkout` | The one moment git tells Rift a worktree was born, from inside the new tree. Used to adopt a new family member — see [09-worktrees-and-families.md](09-worktrees-and-families.md). |

Every entry has a body, and there is no entry for an amend, a rebase or a
merge. A name in this list *is* an installed, routed hook, so one with nothing
behind it reads as coverage Rift does not have. What that costs is stated
rather than implied: a rebase that moves the commits a session recorded leaves
the ledger holding pre-rewrite shapes
([18-known-gaps.md](18-known-gaps.md)).

The `Rift-Checkpoint` trailer names the checkpoint the commit is expected to land, predicted from what the session holds when the message is written.
It is a claim, not a record: the archive mints the real id after the commit, and the two agree when the session has no transcript in the shard and nothing lands between `prepare-commit-msg` and `post-commit`.
A transcript, which the archive binds into the id and the prediction never reads, lands the checkpoint under another id, and so does a serve or a settled turn that arrives in between.
The prediction leaves the transcript out because it has to finish inside git's hook budget, and making the hook heavier to close the gap would trade a commit's latency for a claim no reader depends on alone.
Readers never take the trailer alone: a commit resolves through the union of the checkpoints its trailer claims and the checkpoints recorded against it, so `rift checkpoint explain <commit>` names the checkpoint that landed whichever way the claim went ([05-ledger-and-verification.md](05-ledger-and-verification.md) §5.3).

### How they are installed

The mechanics matter more than they look, because the failure mode they prevent
is silent. Entries name the binary by **absolute path**, since a desktop git
client can run hooks with a sparse `PATH` that never heard of your shell
profile. On Unix an unoccupied hook slot becomes a **direct symlink** to the
binary — no shell process per commit — with a sidecar file recording what
Rift's link points at, so a later install can tell its own link from yours
without following it. **Your hooks are never clobbered**: an occupied slot gets
a tiny portable shell shim that runs *your* hook first and then Rift's, with
your original moved aside under a suffix rather than deleted. And the installer
asks git **where hooks actually resolve** rather than assuming
`<git dir>/hooks`, so a configured hooks path cannot leave one directory
looking healthy while git runs from another.

An install also **removes** an entry Rift wrote under a name it no longer
installs: a repository set up while the list was longer goes on running that
entry on every merge and every rebase step, against a binary that no longer
answers to the name. It removes only entries Rift wrote — a hook of your own
under the same name is left exactly where it is, and one an older install moved
aside under the suffix goes back to its own name. `rift doctor` names a retired
entry, and `--fix` removes it.

### Keep-if-healthy, and the binary of record

Git resolves hooks through the repository's *common* directory, so one install
covers every worktree — and before this rule existed, any worktree's `rift
init` re-pointed every worktree's hooks at whatever binary that process
happened to be, which once left every hook pointing at a deleted temporary
build. Git skips a dangling hook **silently**, and capture stopped for four
days without one error line. Two rules close that.

1. **Keep-if-healthy is the default** for any install that is a side effect of
   something else (`rift init`, the Project Settings surface, worktree
   adoption). A healthy family keeps its hooks and its recorded binary; only a
   dangling link or a vanished binary re-points anything, and that repair is
   reported, never silent.
2. **The binary of record** is a fact with a history: a small file in the
   family's directory recording which binary the hooks point at, when that was
   recorded and by what act, with superseded records kept behind it.

Moving a family from a debug to a release build is therefore an explicit act —
`rift hook install --repoint`, the only spelling that does it. A binary under a
temporary directory, a test-build directory, or inside a linked worktree is
refused unless you pass `--allow-transient`, because all three vanish while the
repository lives on.

> **Tip:** If capture has gone quiet, check the hooks first:
> `ls -l "$(git rev-parse --git-path hooks)"`. A dangling link is the failure
> that produces no message at all; `rift doctor` reports it too.

---

## 3.4 Agent hooks: what your client sends

The agent side is where the interesting content comes from. Rift ships adapters
for nine coding assistants and **captures from three** today: Claude Code,
Codex and Antigravity. The other six stay visible in listings and Project
Settings — naming an integration as not-yet-supported beats hiding it — but
enabling them is refused;
[10-agent-integrations.md](10-agent-integrations.md) has the per-client detail
and reasons. The capture-relevant shape:

Claude Code registers nine verbs in `.claude/settings.local.json` (session
start, prompt submit, pre/post tool use, turn end, subagent start/stop,
pre-compaction, session end); Codex registers ten in `.codex/hooks.json` — the
same shape plus a post-compaction verb; Antigravity registers five (pre/post
tool use, pre/post invocation, turn end) and Rift writes **no** hook file for
it, because the tracked `.agents/hooks.json` is verified rather than written.

Each registered verb becomes a command line
`<absolute rift path> hooks <agent> <verb>`, and the registered set is read from
the adapter's own declared list, so a corrected verb table changes what gets
written and there is no second copy to drift.

Three custody rules apply to those files. **Rift owns only what it wrote**: a
manifest under `.rift/state/` fingerprints each fragment it installed, an
unrecognized bundle is reported as a conflict and left alone, and unrelated
groups in the same file are preserved. **Symlinks are refused**, for the hook
file and the project directory around it, before anything is written through
either. And **a tracked file is never written**: Antigravity's
`.agents/hooks.json` is commonly committed, so Rift writes no hook file for
that agent at all — enabling it owns only the descriptor under `.rift/agents/`,
adoption verifies the tracked file rather than rewriting it, and a bundle whose
ownership cannot be proved is reported rather than replaced.
[10-agent-integrations.md](10-agent-integrations.md) has the per-client custody
detail.

Beside them, `.rift/agents/<agent>.json` is Rift's own descriptor: what it
wired, which binary, and that adapter's compatibility notes (whether the agent
surfaces token usage, which lifecycle events it lacks).

### The wire

An agent hook reads **one JSON object from stdin — never from arguments**: a
security boundary as much as a convenience, because argument lists leak into
process listings and stdin does not. The envelope is bounded before it is
parsed:

- at most 1 MiB of input;
- at most 64 levels of JSON nesting, so deeply nested input cannot exhaust the
  stack;
- the session id must be usable as a file name — non-empty, at most 200 bytes of
  ASCII letters, digits, `-`, `_` and `.`, not starting with `.`.

An oversized, malformed or unusably-named payload records nothing and exits
zero; a hook naming no session at all is recorded as such (see receipts) rather
than dropped in silence. `rift hooks --schema` prints the whole contract: the
canonical-event field list, the accepted session-id field spellings, and the
fallback rule for a client whose payload carries none of the envelope keys.

### Receipts

Every agent hook writes a receipt row, whatever happened, with one of four
outcomes:

| Outcome | Meaning |
|---|---|
| `recorded` | The adapter mapped the verb and the canonical events landed. |
| `recorded-no-event` | The adapter knows the verb and it carries nothing to capture — Antigravity's later invocations of a prompt are the case. Recorded so the hook still counts as having arrived, and kept apart from an unknown verb so a gap in the adapter is not confused with a verb that has nothing to say. |
| `skipped-unknown-verb` | The adapter does not know this verb. Not an error — a new agent version may add verbs before Rift does — but counted, so the gap is visible. |
| `deferred-no-session` | The hook carried no usable session id. Nothing downstream can be written, because every durable row is addressed by its session. The receipt exists precisely so the loss is countable instead of silent. |

A receipt is addressed to the session's **owner** agent, not necessarily the
agent whose process fired the hook — the two differ when one conversation
drives two hook systems, and the owner is the label every other row for that
session carries, so a receipt keyed to the firing agent would fail to join its
own session. The firing agent is not lost: it is what the per-hook idempotency
key derives from, so a redelivery still deduplicates against the client that
delivered it. The exception is `deferred-no-session`, which has no owner and so
names the agent that fired.

---

## 3.5 Canonical events

An adapter translates one client-native verb into zero or more **canonical
events**. The vocabulary is agent-independent, so every downstream consumer —
verification, cost, recall, the dashboard — reads one shape:

| | | |
|---|---|---|
| `session_start` | `session_end` | `session_compaction` |
| `turn_start` | `turn_end` | `prompt_submit` |
| `prompt_response` | `tool_call` | `tool_result` |
| `file_edit` | `subagent_start` | `subagent_end` |

One verb can produce several: a pre-tool-use hook naming an editing tool maps to
a `tool_call` *and* a `file_edit`. Each event carries an envelope the adapter
has no authority to set — session, turn ordinal, event ordinal, time and the
hash of the redacted payload are stamped by the ingest layer. Adapters map; they
never mint identity, and have no redaction or chain authority at all.

### A read is a tool call *or* a command line

A `tool_result` naming a read tool records that the agent read a file: the
path, the bytes' identity at that instant, and — later, from the worker — what
those bytes cost. One of the three alpha clients has no read tool at all.
Codex reads through the shell, and while nothing recognised `cat src/lib.rs`
as a read, every Codex read was invisible to the projection. That is not a
cosmetic gap: the savings share of
[§8.6](08-cost-and-savings.md#86-what-avoided-actually-measures) divides by the
read tokens this projection holds, so a family with no read facts divides by
its own numerator and is dropped from the honest figure by name.

So the ingest asks the same recogniser the read gate asks — the vendor's own
published list of commands that count as having read a file, matched exactly
([chapter 7](07-serving-and-injection.md)) — and records the fact where the
command line names a range it can price: `cat` (the whole file), `head -n N`,
`sed -n 'A,Bp'`. The count is **the file's own tokens over that range**, at the
identity the fact recorded, and not what a truncating terminal displayed. A
`tail`, a `grep`, a pipe, a second command, anything the grammar does not
spell: no fact, no count. The direction is the recogniser's — a missed read
makes the share look larger than it is, which is a shortfall a reader can find,
where an invented one is a claim nobody can check. Nothing new is stored for
this: the fact is the same relation a read tool's is, on a `tool_call` whose
own `tool_name` says which of the two it was.

### One `tool_call` row per tool call

A `tool_call` row records that a tool ran: one row per call, written by the
pre-tool-use hook, keyed by the agent's own tool-use id (`tool_use_id`, which
Claude Code and Codex both send on their tool hooks). A read has a second row
of its own beside it, written when the read completes and keyed by what was
read (the turn, the path, the bytes' hash), because that is the row the file
revision and the read's token count hang from. So a reader that counts tool
calls skips the rows `tool_reads_file` links, and `rift recap`'s tool mix does
exactly that: a `Read` counts once.

The key was once the hook's event slot — turn ordinal plus event ordinal. The
event ordinal restarts at zero in every hook, so every call of a turn derived
the same id and the first one won; projections written before the change hold
one tool-ran row per turn, and nothing rewrites them. A family whose payload
carries no tool-use id (Antigravity's names a step, not a call) still keys by
the slot, as does an id that is empty, longer than 256 bytes or holds a control
character.

### Trust is a field, not a vibe

Every event carries a trust class that travels with the data forever:
`code-fact` (a file edit), `git-fact` (a commit or branch fact), `test-output`
(a test runner's result), `tool-output-untrusted` (arbitrary tool output) and
`transcript-untrusted` (prompt and transcript text). The last two are the
reason serving is bounded and never raw — see
[07-serving-and-injection.md](07-serving-and-injection.md).

### Content addressing is what makes retries safe

Event and row identities derive from content, not from a counter or a
coordinating lock, so ingest is "insert, or do nothing if it is already there".
That single choice pays for a lot:

- a hook the client redelivers writes no second row;
- a crash mid-ingest is repaired by the next hook, not by a repair tool;
- the two event sources (§3.13) can both write without inventing a protocol
  between them — their union is a set union.

---

## 3.6 The session state machine

A session is the unit capture is organised around, and its lifecycle is a
**pure** function: given a state and a trigger it yields the next state and a
list of *requested* effects. Nothing happens inside the transition — the caller
performs the effects, or journals them.

The interesting arms are the ones that were argued about. **Idle timeout is
admitted from `Compacting`**, because a crash does not wait for a compaction to
finish and without this arm a session whose agent died mid-compaction could
never be finalized — it would read as live forever. **`end` is deliberately
*not* admitted from `Compacting`**: that trigger comes from a real hook, and a
compaction that never reports completion is a different defect which should
look like one. **Idle timeout from `Ending` stays illegal**, which is what
makes a repeated sweep a no-op instead of a second condense.

**No phase is a trap.** A session that sends events is working, whatever its
recorded phase said a moment before. `idle`, `ending` and `ended` are all left
by the next sign of life: a `start` or an `activity` (any captured hook that
names no lifecycle trigger — a prompt, a tool call, a turn end) returns the
session to `active`. Three things in the field made this the rule. An editor
that restarts itself for an update ends every agent tab at once and then
resumes each one under the same id, so an `ended` that refused the resume
recorded the busiest session on the machine as over while thousands of its
hooks arrived. A session launched before capture was set up, or whose client
skipped its start hook, sent tool events to an `idle` that accepted only a
start. And an agent that publishes no session end left its sessions `active`
for days. So `ended` is not terminal: a session id can outlive its first
checkpoint, and a re-opened session condenses again at its next end, carrying
the bytes written since. What still holds is the claim that matters: nothing
but a finished condense leads into `ended`.

The bridge asks the machine only for legal steps. A compaction that sees any
other event is done; a session not working is returned to work first, and then
the event's own trigger applies, so a lone `SessionEnd` to a session never
opened opens and ends it in one pass. A start for a session already working,
and an end redelivered to one already ending or ended, change nothing. A
session opened without its agent's start is recorded as a
**`synthesized-start`** transition, never as a `start`, so the audit says what
arrived. And the arm that remains for a trigger the machine refuses is not
silent: **a refusal is its own append-only row**, `refused-<trigger>`, whose
from and to are the phase that refused it. A refusal once hid a working
session for two days because it left no trace, and the hook's receipt already
read "recorded".

**The session cache is transient, and a torn one is moved aside.**
The phase a session is in is mirrored into a small file under the git dir, `rift-sessions/<id>.json`, rewritten whole by every hook; the transition rows are the truth and the file is only their mirror.
Every write goes to a temp file of the writer's own, named by its process and a sequence number, and is renamed over the cache in the same directory.
The temp name is per writer because a subagent shares its parent's session id and its hooks run beside the parent's: with one temp name per session, two hooks truncated and rewrote the same temp, and the rename published one object followed by the tail of the other.
A cache that still does not parse is never fatal.
The next hook of that session renames it to `<id>.json.torn-<ms>` beside where it was, reads the session as unseen, re-opens it as a `synthesized-start`, captures its event and writes the cache whole, and its log says `session cache unreadable, moved aside, session re-opened`.
Nothing is deleted: the moved-aside file is the trace, and `rift doctor` counts those files as Rift's own defect, while `rift session list` prints `cache unreadable` for a cache no hook has reached yet.
The same hook removes a temp file a dead writer of the session left, once it is older than ten minutes — the age at which the capture queue steals a dead worker's lock.

**The idle sweep** ends a session whose agent went away without saying so. A
live session untouched for longer than the family's floor —
`[capture] idle_sweep_minutes`, five minutes by default — is timed out,
condensed against `HEAD` and closed. It runs where work already runs, never on
a timer: at every session start (the hook journals one small request for the
worker it was dispatching anyway) and at the tail of every worker pass,
across every member of the family whose checkout still exists, each member
under its own queue lock. A member whose lock another worker holds is left to
that worker's own tail sweep: a sweep beside a worker would be a second
archiver of the commit the worker is condensing. Sweeping that early is safe precisely because no phase is a trap: a
developer who was only reading gets the session back on their next prompt,
and a pause with no new bytes re-condenses to nothing. `rift doctor` is the
alarm and the manual arm: it reports sessions past the sweep's floor as
*sweepable*, sessions past **24 hours** as **STUCK** (the sweep is not reaching
them), and a session recorded `ended` or `idle` while its own events kept
arriving as Rift's own defect, with nothing asked of you; `--fix` sweeps at the
sweep's floor, under the same per-member lock, and reports a session whose
member a worker is draining as *deferred* rather than waiting for it.
Every deferral carries one cause from a closed vocabulary — `worker-holds-lock`, `no-idle-edge`, `no-commit-to-anchor`, `archive-produced-nothing`, `done-mark-failed` — printed one line per cause and given per session under `condense_deferred_causes` in `--json` (`deferred_causes` from `rift session stop`).
Only `worker-holds-lock` is somebody else's to finish: that worker's own sweep condenses the session and nothing is lost, while every other cause waits for the next `rift doctor --fix`.

---

## 3.7 From session to checkpoint: the condense

A **checkpoint** is what a session's work condenses into: a tree of git objects
written to a ref of Rift's own, with a Merkle leaf and a chain entry behind it.
A shard holds, at minimum, its own facts (`metadata.json`: sessions, times,
the compact transcript's name and boundary, task ids), the opted-in
working-tree capture after the redaction gate (`content.redacted`), a
`redaction_report.json` — required before a shard counts as importable or
projectable — the transcript and its deterministic compaction (`full.jsonl`
and `transcript.compact.jsonl`), the session's recorded tool calls including
the files it read (`tool_calls.jsonl`), the content address that lets identical
bytes share blobs across checkpoints (`content_hash.txt`), four mechanical
digests (`digest.*.json`: failure signatures, the intent/outcome slot, log
shape, file shape) and the leaf this shard contributes to the chain
(`merkle_leaf.txt`). [05-ledger-and-verification.md](05-ledger-and-verification.md)
walks the anatomy.

Two properties are easy to assume and expensive to get wrong. **The compaction
is deterministic, not a summary**: tool calls reduce to bare identifiers, file
reads to handles, no model is involved anywhere in capture, and the "intent and
outcome" slot is filled by your own redacted prompt plus mechanical outcome
facts. And **re-condensing an unchanged session is a no-op**, with identical
transcript bytes sharing the same underlying git objects. Subagent work nests:
each subagent gets its own subtree with its own metadata and transcript, and
its tokens roll up to the parent even when it started before the boundary,
while the parent's metadata enumerates the task ids so hiding one is detected
rather than merely absent.

One step follows the checkpoint and is not part of it.
Once a condense run by the detached worker has landed its checkpoint and chain entry, it binds an outline of every code file the session read whose bytes in the work tree are still a revision it read, redacted first and refused for any path the serve policy denies or marks sensitive, at most 64 per condense, into the recall projection ([§6.15](06-recall.md#615-what-is-running-and-what-is-not)).
It opens that projection for writing and runs the tokenizer, so a hook never does it: an archive made inside a hook, including a condense drained inline under `RIFT_CAPTURE_SYNC`, binds nothing, and the next worker condense of the session does.
A failed binding is counted and dropped; it never fails or retries the condense.

### When does a condense happen?

That is the **strategy**, configured per repository
([02-configuration.md](02-configuration.md)), and another pure function of its
inputs:

| Strategy | Archives when |
|---|---|
| `manual-commit` (default) | a session is live and you make a commit |
| `on-pr` | the work leaves the machine — a push |
| `scheduled` | a configured interval has elapsed, driven by agent activity as well as commits, so a long session without commits still checkpoints; the agent hook that finds the interval due journals the archive, and the detached worker writes it one pass later (§3.10) |
| `policy-driven` | only on the sensitive-path overlay below |

Over all of them sits one overlay: **a touched sensitive path always archives
immediately**, whatever the base strategy, so sensitive work is captured and
redacted promptly rather than at the next convenient moment. A session that is
not live never archives — idle and ended sessions produce nothing. Separately
from strategy, the end of a session always requests a condense: the state
machine's own effect, performed off the hook for the reasons in §3.10.

---

## 3.8 The two latency budgets

Capture has two budgets — different numbers about different things, and quoting
one for the other is the mistake this section exists to prevent.

### The git-latency budget: 100 ms

**Definition:** the time a `git commit` spends *beyond a bare commit*, summed
over all three of the commit's hooks (`prepare-commit-msg`, `commit-msg`,
`post-commit`). It is a displacement, not a wall-clock reading of one process.

The bar is 100 ms, a hard ceiling set by the project's owner rather than
derived from measurements, so it does not move when the machine has a good day.
The most recent published run measures a bare commit at about 35 ms, the same
commit with hooks installed and nothing opted in at about 64 ms, and with a
session live at about 80 ms — roughly 45 ms of displacement against a 100 ms
ceiling. Opting large files into the working-tree capture spends the rest of it
(§3.12).

### The agent-hook budget: p50 under 100 ms, p95 under 500 ms

**Definition:** the wall time of one `rift hooks <agent> <verb>` process,
*including* the ingest it performs. This is a per-hook figure, not a
displacement.

From the most recent published run of the capture harness, every verb is
inside the budget: session start 52.7 ms p50 / 55.4 ms p95, prompt submit
40.9 / 45.5, read tool 17.9 / 21.0, tool result 15.9 / 22.7, turn end
48.3 / 52.5, session end 20.7 / 23.9, identity attribution 52.5 / 55.9. Three
of them were not, and how they got there is the subject of the next two
sections — they are why capture has an off-hook settle and a detached worker at
all. §3.11 covers a fourth measured path that stayed inside the budget and was
fixed anyway.

The decisions the read tool's hook now takes
([07-serving-and-injection.md](07-serving-and-injection.md)) carry their own
rows against the same budget: deciding a read measures 21.12 ms p50 / 27.48 ms
p95, and recording an edit 16.60 / 20.50. Both are dominated by hashing the
file, which is the whole point of the ceiling on what may be hashed at all: a
16 MiB document hashed on the hook took the whole budget on its own before the
ceiling existed, and about a second in an unoptimized build, against **1.2 ms**
after it — the file is refused on the hook path and handed to the worker
instead. A document read answered from an existing sidecar costs 5.3–7.8 ms in
the shipped binary for a megabyte of input.

Deciding a *search* is cheaper still, because the decision is a lookup rather
than a hash: answering a name search costs 3.8–4.7 ms on a sixty-file tree and
23–27 ms on this repository, of which about 19 ms is the staleness probe that
asks whether the stored index still describes this checkout. A cold or stale
index costs **0.35 ms** and nothing else: the index is not loaded, the search
runs exactly as written, and the walk is journaled for the worker. That is the
shape every lever on this path shares — the expensive case is the one that does
not act.

One path is honestly outside the budget and stays there: the **first** read of
a document nothing has converted yet, which waits out the 250 ms conversion
deadline and lands around 265 ms. It is a decision, not a regression — past the
deadline the read runs exactly as written and the conversion is journaled, so
the cost is bounded, it happens once per document, and the alternative is an
agent reading a zip container as text. It is published rather than gated, and
the number is quoted in this repository's own reports as an exception with its
reason attached.

> Tip: this machine's git hooks may point at a development build. Every figure
> above is from the shipped release binary; an unoptimized build of the same
> code runs the same paths several times slower, which is a fact about the
> binary rather than about the design.

---

## 3.9 Off-hook turn usage

A turn's token usage — what the provider reported — lives in your agent's
transcript file, and the hook signalling "this turn is over" fires while the
agent is *still writing* it. Reading there means reading mid-write, which
durably records a truncated turn.

So it does not. The turn-end hook never reads its own turn and never waits;
turn N's usage is settled by the next hook that fires for that session, by
which time the transcript is long flushed. The read starts from a persisted
byte offset in a hook-owned sidecar beside the session cache
(`<git dir>/rift-sessions/<id>.usage`) and **stops at the next real prompt**,
so successive windows tile the transcript exactly and a late settle can never
fold turn N+1 into turn N. The rows keep the identities the old synchronous
path minted, and because those identities are content-addressed, a redelivered
hook, a second settle and the idle sweep are all idempotent. The last turn is
settled by the session-end path, or by the idle sweep before the session is
condensed.

Two honest consequences. **Spend appears one hook later** — invisible in
practice, and at worst bounded by session end or the idle sweep. The same is
true of the checkpoint: the settled spend is frozen into the shard's
`usage.jsonl` (see [05-ledger-and-verification.md](05-ledger-and-verification.md)
§5.3), so a commit made before a turn settles carries that turn's spend only
from the next checkpoint on. And **a
session Rift joins midway counts from the join point**: first contact seeds the
offset at the file's current length, with no backfill; an offset past the
file's length (a restored or relocated transcript) resets to the length and
counts nothing, logged once, never re-counted.

What *does* still happen on the hook is a small bounded tail read at a mid-turn
tool boundary, for the freshest context-pressure figure the dashboard's gauge
shows. That is a context reading, not a spend reading, and the two are kept
apart deliberately — see [08-cost-and-savings.md](08-cost-and-savings.md).

---

## 3.10 The detached worker

Two more pieces of work measured past the agent-hook budget and could not be
made to fit. **The session-end condense** — redact, write the shard, append to
the chain — measured about 648 ms at the median with a real transcript. And
**counting the tokens of a file the agent read** measured about 102 ms on a
120-line file: the cost is not the counting but the tokenizer, which takes
roughly 234 ms to initialise before it sees its first byte. **No size cap keeps a hook inside
a 100 ms budget when the first count alone exceeds it.**

The `scheduled` strategy's interval archive is the third.
It redacts the session's whole transcript, which on a long session is megabytes, and performed on the hook it outlasted the agent client's own hook timeout: the hook was killed inside it, nothing after it ran, and the next hook began it again, so the interval checkpoint never landed.
The hook only decides, and journals one archive per session however many hooks find the same interval due.

The working-tree snapshot a turn boundary takes is the fourth.
A prompt opens a turn and closes the human's window; a turn's end closes the agent's.
At each, capture observes every path that differs from `HEAD`, diffs it against the previous boundary and writes the anchor bytes to the local object store, which costs a `git status` and a read and a diff per changed path, so it grows with what you have left uncommitted.
The hook records only the commit `HEAD` names, read from the ref files without running git, and the boundary's instant; the worker takes the snapshot as of that instant, before the next boundary's and before the commit whose attribution replays it.
A file whose modification time is after the instant is unknown to that boundary: no edit is recorded for it in the window the boundary closes, and its bytes as the worker finds them anchor the next window.
An edit that lands between the hook and the worker therefore costs that file's human/agent split for one window, where diffing the later bytes would credit the edit to the window that closed before it was made.

None of them fits, and there is no daemon to hand them to, so the answer reuses the
shape the commit path already had. The hook writes down what is owed in a job
journal under the git directory — identifier-only jobs, written atomically,
the file name being the job's identity, so a redelivered hook overwrites its
own job rather than queueing a second — then spawns one detached, one-shot
child and does not wait; the child's standard streams go nowhere, so it can
never write over the terminal git is using, and it drains everything under one
lock in order (token counts and boundary snapshots, then condenses and interval archives, then queued commits) until a pass finds
nothing new. **`rift doctor` is the backstop**: its sweep drains the journal
*first*, before any idle-window logic, because a session whose end journaled a
condense that no worker finished is exactly what the sweep exists to rescue and
an idle window would never reach it.

The same journal carries the tree walk that keeps this member's path index
current, asked for at session start, after a commit, and by any search that
found the index cold or stale. It is the same bargain as the token counts: a
walk of a repository cannot be promised inside a hook budget, an index nobody
rebuilt is simply absent, and absent means the agent's own tool runs — so the
work migrates to the worker without anything waiting on it.

A job carries **identifiers only**, never capture bytes: a condense job names
the session, a token job names the read's row and the content identity of the
bytes to count, so the worker re-reads *those* bytes or none.
An interval archive job names the session and why it was due.
A boundary snapshot job names the session, the turn and its edge, `HEAD` and the instant.
The worker asks the strategy again under its lock, on the job's clock, and archives only if the interval is still owed, so a commit that archived the session in the meantime is not followed by a second checkpoint for the same interval.

Three properties hold this together. The journal entry is **durable before the
spawn**, written unconditionally above every branch, so a worker that never
starts leaves the job for the next worker or doctor run — deferral loses
nothing, because both inputs are durable and both outputs are idempotent
appends. Failure is **bounded and loud once**: after a bounded number of
attempts it is reported on a marker channel that says a thing once and stops
the moment it stops being true, named by the *next* commit — the correction to
weeks of silent condense failures.
A job whose row the schema refuses is dropped on its first run with one such note, because no retry can change a refusal.
And **the session only claims to be ended
when a checkpoint exists**: a failed archive leaves it in `ending`, retryable.

If you need the work inline — an end-to-end test, or a CI run with nowhere to
put a background child — set `RIFT_CAPTURE_SYNC=1`. It changes **who** drains
and **when**, never **whether**: both arms run the same drain, so the condense,
the redaction gate and the archive are the same code either way.

---

## 3.11 The machine id

Every session carries binding evidence about where it was captured, part of
which is a **machine id**: a salted hash of a platform fingerprint, never the
raw hardware identifier, zeroized in memory, never logged and never stored.

Deriving it on macOS means spawning a child process, so it is derived once and
cached under the machine-global cache root — the **salted digest only**, plus a
one-way tag naming the install salt, so a cache made under a different install
is refused and a copied file cannot supply a wrong id. Anything unexpected
degrades to deriving once and rewriting, and an absent fingerprint is never
cached: absence is re-asked, not remembered.

---

## 3.12 What gets captured from your working tree

By default: **nothing**. Rift captures the transcript, the events, the tool
calls and the git facts, not a snapshot of your working tree. To opt in, put a
`.worktreeinclude` file at the root of your working tree naming the paths you
want captured — the inverse of older tools' default of snapshot-everything with
re-included exceptions. A repository without the file produces the
byte-identical empty capture, and the same checkpoint id, it produced before
the control existed.

The rules are deliberately narrow, and each closed a real hole. There are **no
glob patterns and no ignore-file evaluation** — the control is defined by what
it names, and naming a tracked file merely duplicates content git already
holds. **`.git` is refused at any depth, in any spelling**: any case, either
separator, any run of trailing dots or spaces. **Symlinks are preserved, never
followed**, including a symlinked *directory* met mid-walk and the control file
itself, since a repository can ship a `.worktreeinclude` that links into your
home directory and reading through it would turn that file's lines into include
entries. **An entry containing `\` or `:` is refused outright**, because a
portable file whose lines mean different files on different platforms has no
honest capture semantics — and the `:` rule is the only thing refusing a
Windows alternate-data-stream name. **Refusals are recorded, not dropped**,
though a spent global budget ends the walk rather than narrating the remainder,
since one line per file under an opted-in dependency directory is a
multi-megabyte document the redactor would re-scan every commit. And **files
that are oversized or not valid UTF-8 are refused whole**, never truncated.

The caps are small on purpose — **8 KiB total**, 4 KiB per file, 256 records,
16 levels deep — because this walk happens inside `prepare-commit-msg`, the one
hook that cannot detach and that owns the git-latency budget, and the redaction
it feeds is that hook's dominant cost. The published runs show what a larger
opt-in costs: a corpus at the 8 KiB line measures around 103 ms median and a
less favourable one reaches 137 ms, against a 100 ms ceiling.

Opted-in bytes are raw and local by type, and their only durable exit is the
ordinary redaction gate — no second path, no second sealing point (see
[04-redaction-and-secrets.md](04-redaction-and-secrets.md)).

---

## 3.13 Two event sources, one truth

Capture has two ways to produce canonical events, with distinct roles.

**Agent hooks are the low-latency source.** Events land as the session runs,
which is what makes the dashboard, `rift status` and in-session metrics work at
all.

**The artifact bridge is the authoritative reconciler.** It walks the redacted
checkpoint shards, refuses any shard without a valid redaction report,
re-derives canonical events from the shard's own transcript, projects them
through exactly the same writers and identity derivations the live path uses,
and advances an append-only ingest watermark. Because git is the record, the
bridge's view wins and the hook path is an optimisation over it. Run it with
`rift rebuild`, deleting `projection.db` first for a from-scratch rebuild; over
a live projection it is a set-union no-op. That property is the point:
**hooks alone cannot be authoritative** — they miss sessions run before hooks
were installed — and a destroyed database must be rebuildable from git alone.
The bridge never reads raw session bytes; the reader it is handed opens only
the checkpoint ref.

A separate, read-only importer reads checkpoint histories written by an older
capture tool, in both on-disk layouts it shipped, and lands them as read-only
checkpoints under their own chain era. It is untrusting of the source: every
shard is parsed leniently, its redaction report is required, and its claimed
root is re-derived from its own events. An imported checkpoint is never a chain
anchor.

---

## 3.14 What capture never does

**No network** — capture makes no outbound connection, ever. **No model** —
nothing in capture calls one; compaction and digests are deterministic. **No
daemon** — every process is short-lived and started by a hook or by you. **No
blocking** — no hook can fail your commit, no agent hook can fail your agent.
**No guessing** — a fact that cannot be established is recorded as absent
rather than approximated, so read-token counts are "measured or absent" and a
hook fired outside any repository records nothing and mints nothing rather than
creating a repository under whatever directory the agent's shell happened to be
in. **No mutation** — corrections are new rows superseding old ones and the
ledger is append-only. [17-invariants.md](17-invariants.md) has these as
standing rules.

---

## 3.15 Seeing and repairing capture

```bash
rift status                  # is capture wired, and is anything live?
rift session list            # live, ended and archived sessions together
rift session info <ID>       # one session: state, age, identity, tokens, checkpoints
rift session show <ID>       # its phase and captured events
rift checkpoint list         # recorded checkpoints, newest first
rift checkpoint explain      # HEAD's checkpoint, from its own shard
rift doctor                  # capture, chain, sessions, queue, soak — no network
rift doctor --fix            # finalize stuck sessions, drain queues, heal a chain tail gap
```

`rift session stop` finalizes stale sessions through the state machine, or one
session by id; `rift session resume` checks out a stopped session's branch and
restores its transcript from the checkpoint shard; `rift session attach` adopts
an out-of-band session by linking its existing transcript to the last commit,
where amending that commit to add the trailer needs an explicit yes and never
runs from a hook.

---

## Related chapters

[02-configuration.md](02-configuration.md) ·
[04-redaction-and-secrets.md](04-redaction-and-secrets.md) ·
[05-ledger-and-verification.md](05-ledger-and-verification.md) ·
[08-cost-and-savings.md](08-cost-and-savings.md) ·
[09-worktrees-and-families.md](09-worktrees-and-families.md) ·
[10-agent-integrations.md](10-agent-integrations.md) ·
[11-canopy-tour.md](11-canopy-tour.md) ·
[15-benchmarks.md](15-benchmarks.md) for the protocol behind every number in
§3.8.
