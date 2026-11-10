# 3. Capture

Capture turns two streams of small, untrusted signals — the ones your git client
emits and the ones your coding agent emits — into one immutable record: canonical
events, sessions and checkpoints on a ledger you can verify. The constraint
behind almost every choice here: **capture may not be slow, and may not fail
your work**. This chapter describes what [`rift init`](01-quickstart.md) wired up.

---

## 3.1 The shape of capture

**The ledger is the record; the database is a projection.** Checkpoints are git
objects on a ref of Rift's own; `projection.db` is a queryable view that can be
deleted and rebuilt.

**Every hook is a short-lived process.** No background service: a hook reads at
most one JSON object from stdin (or git's arguments), writes what it can, and
exits. Work that cannot fit is *journaled* — written down durably — and performed
by a detached one-shot child (§3.10), a deliberate refusal to ship a daemon.

`RIFT_HOOK_TIMING=1 git commit` prints one line per instrumented hook naming where its
milliseconds went; unset, the timer reads no clock.

---

## 3.2 Two kinds of hook, one rule

Git hooks are installed into the repository's hooks directory and run as
`rift hooks git <name>`; agent hooks are installed into your agent's project config and
run as `rift hooks <agent> <verb>`, reading one JSON object on stdin.

**A hook never blocks the work that triggered it.** Commit-path git hooks run
their body inside a guard that catches even a panic and exits zero; agent hooks
always exit zero, reporting a capture error as deferred. The one exception is
`pre-push`, which refuses pushing Rift's own internal refs, never your branches.
A side channel that can break the main channel gets uninstalled the first time it costs a commit.

---

## 3.3 The five git hooks

`rift init` (or `rift hook install`) writes five entries; the list *is* the
dispatch table, so name and behaviour cannot drift apart.

| Hook | What Rift does |
|---|---|
| `prepare-commit-msg` | Adds the `Rift-Checkpoint` trailer when a session is live and preserves an existing attribution trailer; never rejects. |
| `commit-msg` | Normalizes those trailers idempotently; never rejects. |
| `post-commit` | The main capture beat: records the commit and session, hands heavy work to a detached worker. |
| `pre-push` | Refuses Rift's internal refs, signals "work is leaving" for `on-pr`, drains the push queue. |
| `post-checkout` | The one moment git tells Rift a worktree was born, from inside the new tree; adopts a family member ([09](09-worktrees-and-families.md)). |

There is no entry for an amend, rebase or merge: an entry with nothing behind it
would read as coverage Rift does not have. A
rebase that moves recorded commits leaves the ledger holding the pre-rewrite shapes.

The `Rift-Checkpoint` trailer is a **claim, not a record**: the prediction must finish
inside git's hook budget, so it omits the transcript the archive binds into the id, and a
transcript or settled turn arriving in between lands the checkpoint under another id.
Readers therefore resolve a commit through the union of the checkpoints its trailer
claims and those recorded against it (`rift checkpoint explain <commit>`,
[05](05-ledger-and-verification.md) §5.3).

### How they are installed

Entries name the binary by **absolute path** (a desktop git client can run hooks with a
sparse `PATH`). An unoccupied slot becomes a **direct symlink**, no shell per commit;
**your hooks are never clobbered** — an occupied slot gets a shim running *your* hook
first, your original moved aside. The installer asks git **where hooks actually
resolve**, and removes only entries Rift wrote under a name it no longer installs.

### Keep-if-healthy, and the binary of record

Git resolves hooks through the *common* directory, so one install covers every
worktree, and git skips a dangling hook **silently**: capture could stop for days
without an error line. So an install that is a side effect of something else keeps
healthy hooks, repairing only a dangling link or vanished binary, out loud. **The
binary of record** (`binary.json`) names the hooks' binary, when and by what act. It
moves only with the hooks, to a lasting build: `rift init`, `rift enable` and `rift
doctor --fix` in the main work tree follow the running binary, a debug or transient
build never ([09](09-worktrees-and-families.md) §9.7 has the rules), and `rift hook
install --repoint` moves both on purpose.

---

## 3.4 Agent hooks: what your client sends

Rift ships adapters for nine assistants and **captures from three**: Claude Code, Codex
and Antigravity; enabling the other six is refused ([10](10-agent-integrations.md)).

Claude Code registers nine verbs in `.claude/settings.local.json` (session start, prompt
submit, pre/post tool use, `Stop`, subagent start/stop, `PreCompact`, session end); Codex
registers those plus `PostCompact` in `.codex/hooks.json`; Antigravity declares five
(pre/post tool use, pre/post invocation, `Stop`) and Rift writes **no** hook file for it.
Each verb becomes `<absolute rift path> hooks <agent> <verb>`, read from the adapter's
own declared list so there is no second verb table.

Custody: **Rift owns only what it wrote** (a manifest under `.rift/state/`
fingerprints each fragment; an unrecognized bundle is a reported conflict; unrelated
groups are preserved), **symlinks are refused**, and **a tracked file is never
written** — Antigravity's `.agents/hooks.json` is commonly committed, so enabling it
owns only `.rift/agents/<agent>.json` and adoption verifies the tracked file.

Antigravity names its transcript on every hook as `transcriptPath` — the client's
`transcript_full.jsonl`; the bridge copies only that address into the session
cache, nothing on the hook path opens the file, and the archive seals it like
every other agent's. Neither its records nor Codex's rollout are shapes the
compactor reduces, so for both the full copy sealed beside the near-empty
compact form is the record — nothing is lost.

### The wire

A hook reads **one JSON object from stdin — never from arguments**, which leak into
process listings. The envelope is bounded before parsing: at most 1 MiB, at most 64
levels of nesting (so nesting cannot exhaust the stack), and a session id usable as
a file name (non-empty, at most 200 bytes of ASCII letters, digits, `-`, `_`, `.`,
not starting with `.`). A bad payload records nothing and exits zero.
`rift hooks --schema` prints the contract: the event field list, the accepted
session-id spellings and the fallback for a payload with no envelope keys.

### Receipts

Every agent hook writes a receipt row with one of four outcomes: `recorded`;
`recorded-no-event` (a known verb with nothing to capture, kept apart from an unknown
verb so an adapter gap is not confused with a quiet verb); `skipped-unknown-verb`
(not an error, since a new agent version may add verbs first, but counted); or
`deferred-no-session` (no usable session id, so the loss is countable). The ingest
never writes a fifth, `deferred`: that word is the hook process's own report of a
swallowed capture error. A receipt is addressed to the session's **owner** agent,
the label every other row of that session carries; the firing agent feeds the
idempotency key, so a redelivery still deduplicates.

---

## 3.5 Canonical events

An adapter translates one client verb into zero or more **canonical events**, so
verification, cost, recall and the dashboard read one shape: `session_start`,
`session_end`, `session_compaction`, `turn_start`, `turn_end`, `prompt_submit`,
`prompt_response`, `tool_call`, `tool_result`, `file_edit`, `subagent_start`,
`subagent_end`. One verb can produce several: a pre-tool-use hook naming an
editing tool maps to a `tool_call` *and* a `file_edit`. Session, ordinals, time and
the redacted payload's hash are stamped by the ingest layer; adapters map, never
mint identity, and have no redaction or chain authority.

### A read is a tool call *or* a command line

A `tool_result` naming a read tool records the path, the bytes' identity at that
instant and — later, from the worker — what they cost. Codex reads through the
shell, so without a recogniser its reads would be invisible and the savings share
of [§8.6](08-cost-and-savings.md#86-what-avoided-actually-measures) would divide
by its own numerator for that family. The ingest therefore asks the recogniser the
read gate asks (the vendor's published list of commands that count as a read,
matched exactly — [07](07-serving-and-injection.md)) and records a fact only where
the command names a range it can price: `cat`, `head -n N`, `sed -n 'A,Bp'`. The
count is **the file's own tokens over that range** at the recorded identity, not
what a truncating terminal displayed. `tail`, `grep`, a pipe, a second command: no
fact. The direction is deliberate — a missed read makes the share look larger, a
shortfall a reader can find, where an invented read is a claim nobody can check.

### One `tool_call` row per tool call

The pre-tool-use hook writes one row per call, keyed by the agent's `tool_use_id`; a
completed read gets a second row (keyed by turn, path and bytes' hash) that the file
revision and token count hang from, so a tool-call counter skips the rows
`tool_reads_file` links and a `Read` counts once. A payload without an id (Antigravity
names a step) or with an unusable one keys by the hook's event slot, which cannot key a
call alone: the event ordinal restarts at zero in every hook, so every call of a turn
would derive the same id.

### Trust, and content addressing

Every event carries a trust class that travels with it: `code-fact`, `git-fact`,
`test-output`, `tool-output-untrusted`, `transcript-untrusted`. The last two are why
serving is bounded and never raw ([07](07-serving-and-injection.md)). Identities derive
from content, not a counter or lock, so ingest is "insert, or do nothing": a redelivered
hook writes no second row, a crash mid-ingest is repaired by the next hook, and the two
event sources (§3.13) union as a set.

---

## 3.6 The session state machine

A session's lifecycle is a **pure** function: a state and a trigger yield the next
state and a list of *requested* effects; the caller performs or journals them.

The contested arms: **idle timeout is admitted from `Compacting`**, since a crash does
not wait for a compaction and otherwise a session whose agent died mid-compaction would
read as live forever; **`end` is not**, because it comes from a real hook and a compaction
that never completes is a different defect that should look like one; **idle timeout
from `Ending` stays illegal**, so a repeated sweep is a no-op, not a second condense.

**No phase is a trap.** A session that sends events is working: `idle`, `ending` and
`ended` are all left by the next `start` or `activity`. An editor restart ends every
agent tab and resumes each under the same id, a session may predate capture or skip its
start hook, an agent may publish no end; a phase that refused those would record the
busiest session as over, or leave one `active` for days. So `ended` is not terminal — a
re-opened session condenses again at its next end — and what holds is that nothing but
a finished condense leads into `ended`.

The bridge asks the machine only for legal steps: a session not working is returned
to work first, so a lone `SessionEnd` opens and ends it in one pass. A session opened without its agent's start is a
**`synthesized-start`**, never a `start`, so the audit says what arrived. A refused
trigger is **its own append-only row**, `refused-<trigger>`, because otherwise a
refusal can hide a working session while the receipt reads "recorded".

**The session cache is transient.** The phase is mirrored into
the member's run state, `sessions/<id>.json` ([09](09-worktrees-and-families.md) §9.5); the transition rows are the truth. Writes use a
per-writer temp file renamed over the cache, because a subagent shares its parent's
session id and a shared temp would be renamed half written. A cache that still does not
parse is renamed to `<id>.json.torn-<ms>` and the session re-opened as a
`synthesized-start`; nothing is deleted, and `rift doctor` counts torn files as Rift's
own defect.

**The idle sweep** ends a session whose agent vanished: one untouched longer than
`[capture] idle_sweep_minutes` (five by default — long enough that a developer reading a
diff keeps the session, short enough that its work is still news) is condensed against
`HEAD` and closed. It runs where work already runs, never on a timer: at every session
start and at the tail of every worker pass, per family member under its own queue lock; a
member a worker holds is left to that worker, since a sweep beside it would be a second
archiver of the same commit. Sweeping early is safe because no phase is a trap.

`rift doctor` is the alarm and the manual arm (*sweepable* past the floor, **STUCK**
past 24 hours; `--fix` sweeps under the same lock — §3.15).
A deferral carries one cause from a closed vocabulary (`worker-holds-lock`,
`no-idle-edge`, `archive-produced-nothing`, `done-mark-failed`, and the retired
`no-commit-to-anchor` from an old report); all but `worker-holds-lock` wait for
the next `rift doctor --fix`.

---

## 3.7 From session to checkpoint: the condense

A **checkpoint** is a tree of git objects on a ref of Rift's own with a Merkle leaf
and a chain entry behind it. A shard holds its metadata, the opted-in working-tree
capture after the gate, `redaction_report.json` (required before a shard is
importable or projectable), the transcript and its compaction, the tool calls
including files read, and four mechanical digests; [05](05-ledger-and-verification.md)
walks the anatomy. The sealed transcript is **redacted-complete**: every prompt,
assistant text and thinking block, tool call input and tool result in full, with
secrets and the opaque thinking signatures masked
([§5.3](05-ledger-and-verification.md#53-anatomy-of-a-checkpoint) says why that,
and not byte-identity, is the standard).
A turn-end checkpoint can seal only the lines past the previous turn's cut, as one more part under `transcript.segments/`, and one resolver joins the parts back into the whole transcript every reader and every hash sees.

A checkpoint written by capture recovery (§3.10) also carries
`turn_boundary_gaps.jsonl`: the turn boundaries this member missed, asserted as
missing, never as authorship. Only its carrier session becomes a session; the
other sessions it names create no session, usage, lifecycle or attribution facts.
Its commitments describe source provenance, not destination paths, possession of
old bytes or permission to retire queued work, and it restores no observation.

**The compaction is deterministic, not a summary**: in the *compact* transcript
only, tool calls reduce to identifiers and tool results keep their shape and lose
their payload (the full transcript beside it keeps every input and result), no
model runs in capture, and the "intent and outcome" slot is your redacted prompt
plus mechanical outcome facts. **Re-condensing an unchanged session is a no-op.**
Each subagent gets its own subtree — its transcript found through the address its
stop hook claimed or the client's `subagents/` listing, sealed through the same
gate — its tokens roll up to the parent, and the parent's metadata enumerates the
task ids so hiding one is detected, not merely absent. A tool result the client
spilled to a file of its own is read at archive, redacted and sealed beside the
transcript that names it, so the record holds the result and not its preview.

The worker job that counts a read binds that file's outline (code or Markdown,
redacted, policy-gated) into the recall projection
([§6.15](06-recall.md#615-what-is-running-and-what-is-not)); after a worker condense
lands, it binds any the session read and left unbound, at most 64. That needs the projection
open for writing and the tokenizer, so a hook never does it: an archive made in a hook,
including one drained under `RIFT_CAPTURE_SYNC`, binds nothing and the next worker
condense does. A failed binding is counted and never fails the condense.

A rule that cannot reach the bounded redaction fixpoint costs the affected
records and their required context. Failed parsed records become fixed fallback
records; failed flat runs also replace lines whose protection depends on their
neighbours, preserving safe independent lines ([§4.6](04-redaction-and-secrets.md#46-the-json-aware-walker)).

### When does a condense happen?

The per-repository **strategy** ([02](02-configuration.md)) is another pure function of
its inputs:

| Strategy | Archives when |
|---|---|
| `manual-commit` (default) | a session is live and you commit |
| `on-pr` | the work leaves the machine — a push |
| `scheduled` | `schedule_interval_minutes` (30) has elapsed, driven by agent activity too, so a commitless session still checkpoints; the hook that finds it due journals the archive (§3.10) |
| `policy-driven` | only on the overlay below |

Over all of them, **a touched sensitive path always archives immediately**; a session
that is not live never archives; a session's end always requests a condense, off the hook.

### One checkpoint per turn

Beside the strategy, every turn end mints a checkpoint of its own, archive reason `turn-end`.
The agent's `Stop` hook only journals the job; the detached worker (§3.10) seals it, so the hook budget never pays for it.
A condense never needs a commit: the shard is anchored to `HEAD` only when `HEAD` is born and is not the commit the session's previous checkpoint already anchors, and is otherwise **anchorless**, recording the `HEAD` it saw as `observed_head`.
Recall serves an anchorless seal to the same worktree on the same branch while that observed head is in the caller's history, and a rewound branch drops it again ([§6](06-recall.md)).

Each turn passes only its new lines through the gate.
The earlier parts, full and compact, are read back from the previous shard and carried by their checked blob oids, so the shard holds `transcript.segments/NNNNNN` and its compact counterpart, joined by the one resolver of [§5.3](05-ledger-and-verification.md#53-anatomy-of-a-checkpoint).
A part that fails its read-back, or a changed pack set, reseals the whole transcript instead.
The cut is the last complete record the worker read, so a turn's final line, still being written when `Stop` fires, waits for the next turn or the session end.

The worker records what it sealed in a sidecar beside the session cache, `<id>.turn-seal`: the checkpoint, the transcript length it read, the lines the parts cover and any fallback.
The **lag** is the live transcript's length past that read.
`rift doctor`'s `turn_seals` leg names a session whose last seal fell back (`segment-chain-broken`, `packs-changed`, `segment-mismatch`), whose lag passed 1 MiB (`lagging`), or whose sidecar does not parse (`unreadable`); it reports only, and `--fix` changes nothing there.

At the session end the tail is sealed the same way, then compared once with the whole transcript sealed in one pass.
Equal bytes keep the parts and the shard records `reconciled: true`; any difference makes the whole seal the final body, recorded as `reconciled: false` with `supersedes_segments: true` and named `segment-mismatch`.

The sidecar also carries the turn end's scans of the main transcript — the spill addresses its results name and the lines recording spooled originals — up to a line boundary, with a digest of the last bytes before it, so the next turn end scans only the bytes past that frontier and joins the two.
The carry is trusted only when the file still ends that boundary with the same bytes, the seal extended an unbroken chain, and no original was spooled that the earlier scan did not look for; anything else scans the whole file.
Earlier parts are checked against the blob oids their tree records, never re-hashed against a leaf of themselves.
The cost is the worker's, not the hook's, and what still grows with the session is hashing the whole document, which every leaf and id is computed over.

---

## 3.8 The two latency budgets

Two budgets measure different things; never quote one for the other.

### The git-latency budget: 100 ms

The time a `git commit` spends *beyond a bare commit*, summed over its three hooks —
a displacement, not one process's wall time. It is a ceiling set by decision, not
derived from measurements, so a good day on the machine does not move it.
The latest published run: bare commit about 35 ms, hooks installed about 64 ms, session
live about 80 ms — roughly 45 ms of displacement. Opted-in capture spends the rest (§3.12).

### The agent-hook budget: p50 under 100 ms, p95 under 500 ms

The wall time of one `rift hooks <agent> <verb>` process *including* its ingest. The
latest capture-harness run, p50 / p95: session start 52.7 / 55.4 ms, prompt submit
40.9 / 45.5, turn end 48.3 / 52.5, session end 20.7 / 23.9, the tool hooks 16–18 / 21–23
([15](15-benchmarks.md) has the rest). Three pieces of work did not fit and are why
there is an off-hook settle and a detached worker (§3.9, §3.10).

The read gate's decisions ([07](07-serving-and-injection.md)) are dominated by hashing
the file, so a ceiling bounds what may be hashed on the hook: a 16 MiB document took the
whole budget against **1.2 ms** with it, the file going to the worker. The **first** read
of an unconverted document is outside the budget by design: it waits out the 250 ms
conversion deadline (about 265 ms), then runs as written with the conversion journaled,
once per document — the alternative is an agent reading a zip container as text.
Figures are the release binary.

---

## 3.9 Off-hook turn usage

A turn's token usage lives in the agent's transcript, and the "turn over" hook fires
while the agent is *still writing* it, so reading then would durably record a
truncated turn. The turn-end hook therefore never reads its own turn: it is settled by
the next hook for that session, from a persisted byte offset in a sidecar
(`sessions/<id>.usage`), **stopping at the next real prompt** so windows
tile the transcript and a late settle cannot fold turn N+1 into N. Content-addressed
identities make a redelivery, a second settle and the idle sweep idempotent; the last
turn is settled by session end or the sweep. **Spend appears one hook later**, so a
commit made before a turn settles carries it in the next checkpoint's `usage.jsonl`.
**A session joined midway counts from the join point**: first contact seeds the offset
at the file's length, and an offset past the length (a restored transcript) resets to
it and counts nothing. Token counting itself is [08](08-cost-and-savings.md)'s.

---

## 3.10 The detached worker

Four jobs do not fit a hook. **The session-end condense** measured about 648 ms at the
median. **Counting a read file's tokens** measured about 102 ms on 120 lines because the
tokenizer takes about 234 ms to initialise. **The `scheduled` archive** redacts a whole
multi-megabyte transcript; on the hook it outlasted the client's timeout and was
restarted by the next hook, so the checkpoint never landed. **Turn-boundary collection**
costs a `git status`, bounded reads and object writes for changed paths. The hook
records a boundary event's `HEAD` and time; the worker later collects current bytes
under a separate collection-attempt interval — not an atomic snapshot, and not the
bytes at the event time.

Turn-boundary capture preserves a symlink's target spelling without reading its
referent. Rift trusts the directories above the configured worktree root; the root
and every repository-relative component are opened without following links. Regular
reads are capped at 5 MiB; directories, special files, unstable replacements and
larger files produce unknown observations. The object writer receives only the
captured bytes, so a replaced path cannot redirect a second read.

Append-only companion facts distinguish regular files, links, confirmed absence and
unknown observations; a boundary without one supplies no type evidence. They
describe observations, not who wrote the changes: Rift assigns no closed-window
hunks to a writer, commit-diff attribution is unchanged, no preview percentage
derives from them, and a retry repairs no stored attribution. Like the boundary
rows they are projection-only; shards do not carry them.

With no daemon, the hook writes an identifier-only job into its checkout's family
run state, spawns one detached child with its streams going nowhere, and does not
wait. Each checkout's worker holds an
open-file lock at its canonical Git directory, and elapsed time never releases a
live owner. An older Rift's lock file, a timestamp with no pid, reads as a live
owner forever; `rift doctor` names it, and `--fix` moves it aside — renamed, never
deleted — only while holding the current lock and only when no Rift process
started before the file was last written is still running.

A pass that wrote a checkpoint queues one **recall ingest**, so recall serves from
the newest checkpoint without the dashboard open; the job names only the family's
queue, so every pass that asks while one is pending is answered by that one.

Every name in the agent queue belongs to one closed grammar that its writers spell
through and every reader classifies through: a job ticket `<stem>[--<context>].json`;
the markers `condensed-<key>` (a session's finished condense), `completed-<id>.done`
(one condense delivery's completion), `unavailable-<stem>` (a basis snapshot that
completed `unavailable`) and `<key>.error`; and publish stages. A marker is
counted, never run, and a name outside the grammar is unknown, so no reader falls
behind a new marker.

Queue context includes the full payload apart from its attempt counter. An
identical context stays queued without resetting retries; a changed session,
worktree or queue time remains separate. Migration removes an old ticket only
when its complete payload, including attempts, matches the installed copy.
Actual execution failures have three attempts, each followed by a backoff
recorded on the ticket (30 s, doubling, at most an hour); the third parks the
ticket — kept and named by `rift doctor`, skipped by the ordinary worker, and
retried by `rift doctor --fix` or `rift session retry`. Ownership barriers and
unproved input or completion facts stay owed without spending that budget, and
doctor reports the available reason.

An exact-context condense marker also names an existing checkpoint. Legacy
markers, ended caches and token counts without their original inputs cannot
certify a retry, so some work remains pending. A session claims to be ended only
when a checkpoint exists.

### Recovering a backlog: `rift doctor --fix`

A turn-boundary collection queued long ago cannot be performed truthfully now: the
worker collects *current* bytes, so collecting late would credit the present tree to an
old turn. Recovery (Unix) records such a backlog as a gap instead, in one job.

**Getting the queue.** Some worker is nearly always draining, so `--fix` leaves a
request marker beside the owner lock and waits at most two minutes: a worker checks it
(one `stat`) after every job and lets go, and a hook only enqueues while it is fresh. Its
writer removes it on every exit, so one older than three minutes is ignored.

**The hold.** Under the lock the job writes a hold in the member's git directory;
while it exists ordinary execution, migration and ticket removal or retry wait, hooks
still queue, and age never expires it. It is released on every exit; the plan
directory stays as the receipt.

**The stages**, in the only order they run, each skipped once its own durable record
proves it finished, so a crash resumes at the next: `selection` reads the queue whole
under caps — any entry the grammar cannot place, any cap crossed or read failed makes
it non-actionable, never a partial list — reserves the one current boundary of a live
session and marks every other boundary historical; `cut` pins each historical ticket
and publishes an immutable cut; `gap` derives one checked gap batch from it; `archive`
seals the batch on a carrier's checkpoint; `retire-intent` and `retirement` retire the
pinned tickets; `observation` collects the reserved boundary last, inside the hold, so
ordinary capture resumes from a truthful fresh baseline. A refusal stops the walk
there, with the gap owed and named.

**The carrier** is a queued session-end condense whose session was observed ending,
with one capture identity, one stored `session_end` and no prior completion. Using it
claims no ending and consumes nothing; a doubt refuses rather than choose by time. When no condense can carry, recovery's own baseline — the reserved
boundary collected now, with its actual time — carries the gap; two admissible
condenses are a doubt the baseline does not settle. With no live session holding a
current boundary the refusal says `baseline-unreserved`; carrier refusals print
their reason counts.

`RIFT_CAPTURE_SYNC=1` drains inline (tests, CI); it changes **who** drains, never
**whether**.

---

## 3.11 The machine id

Each session carries a **machine id**: a salted hash of a platform fingerprint, never the
raw identifier, zeroized and never logged. Deriving it on macOS spawns a child, so only
the **salted digest** is cached under the machine-global cache root, tagged with the
install salt so another install's cache is refused; an absent fingerprint is never cached.

---

## 3.12 What gets captured from your working tree

By default **nothing**: Rift captures the transcript, events, tool calls and git
facts, not your working tree. To opt in, put a `.worktreeinclude` at the root naming
paths to capture. A repository without it yields the byte-identical empty capture.

The rules are narrow, each closing a real hole. **No globs and no ignore-file
evaluation** — the file means what it names. **`.git` is refused at any depth, in any
spelling** (case, either separator, trailing dots or spaces). **Symlinks are preserved,
never followed**, including the control file itself, since a repository can ship a
`.worktreeinclude` linking into your home directory. **An entry containing `\` or `:` is
refused**, so lines cannot mean different files on different platforms (the `:` rule
alone refuses a Windows alternate-data-stream name). Refusals are recorded, except that a
spent global budget ends the walk. **Oversized or non-UTF-8 files are refused whole**,
never truncated.

The caps are small on purpose — **8 KiB total**, 4 KiB per file, 256 records, 16 levels —
because this walk runs inside `prepare-commit-msg`, the one hook that cannot detach, and
the redaction it feeds is that hook's dominant cost: a corpus at the 8 KiB line measures
about 103 ms median against the 100 ms ceiling. Opted-in bytes are raw and local by type;
their only durable exit is the redaction gate ([04](04-redaction-and-secrets.md)).

---

## 3.13 Two event sources, one truth

**Agent hooks are the low-latency source**: events land as the session runs. **The
artifact bridge is the authoritative reconciler**: it walks the redacted shards, refuses
any without a valid redaction report, re-derives events from the shard's own transcript
through the same writers and identity derivations as the live path, and advances an
append-only watermark. Git is the record, so the bridge's view wins and the hook path is
an optimisation over it; hooks alone cannot be authoritative, since they miss
sessions run before installation. `rift rebuild` runs it (delete `projection.db`
first for a from-scratch rebuild; existing checkpoints are skipped, except that a
present recovery audit is authenticated and its projection rechecked; refusal
keeps the checkpoint and adds no unauthenticated gap facts). The bridge never
reads raw session bytes.

A read-only importer lands another tool's checkpoints under their own chain era, never as
a chain anchor ([05](05-ledger-and-verification.md)).

---

## 3.14 What capture never does

No network, model, daemon, blocking, guessing (an unestablishable fact is
recorded as absent) or mutation: see [17](17-invariants.md) and
[18](18-what-rift-does-not-do.md).

---

## 3.15 Seeing and repairing capture

`rift status`, `rift session list|info|show`, `rift checkpoint list|explain` and
`rift doctor [--fix]` show and repair capture. Plain `rift doctor` only reports: it
opens no lock and writes nothing, naming a recovery hold and how long it has been
held, a stale recovery request, an old-format lock, parked tickets and turn-seal
findings. `session_gaps` names each hole in a session's record with one word —
`never-captured`, `boundary-missing` (unless a published gap batch already names the
turn), `usage-absent`, `pricing-incomplete` — and stays report-only under `--fix`,
since filling a hole would invent attribution. `--fix` retires a dead old lock first,
then runs recovery, then drains; tickets recovery classed historical are kept out of
that drain. `rift session stop|resume|attach` finalize, restore or adopt a session; amending a commit
to add the trailer needs an explicit yes and never runs from a hook.

---

## Related chapters

[02](02-configuration.md) · [04](04-redaction-and-secrets.md) ·
[05](05-ledger-and-verification.md) · [08](08-cost-and-savings.md) ·
[09](09-worktrees-and-families.md) · [10](10-agent-integrations.md) ·
[11](11-canopy-tour.md) · [15](15-benchmarks.md) for the protocol behind every
number in §3.8.
