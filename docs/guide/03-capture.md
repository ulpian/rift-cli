# 3. Capture

Capture turns two streams of small, untrusted signals — the ones your git client
emits and the ones your coding agent emits — into one immutable record: canonical
events, sessions and checkpoints on a ledger you can verify. The constraint
behind almost every choice here: **capture is not allowed to be slow, and it is
not allowed to fail your work**. Run [`rift init`](01-quickstart.md) first; this
chapter describes what it wired up.

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

There is no entry for an amend, rebase or merge: a name in the list *is* an installed,
routed hook, so one with nothing behind it would read as coverage Rift does not have. A
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
worktree — and any worktree's `rift init` could re-point them all at whatever binary
that process was, such as a deleted temporary build. Git skips a dangling hook
**silently**, so capture could stop for days without an error line. So
**keep-if-healthy is the default** for any install that is a side effect of something
else: only a dangling link or vanished binary re-points anything, and the repair is
reported. **The binary of record** is a small file in the family's directory naming the
binary, when and by what act, with superseded records kept. Changing it is explicit,
`rift hook install --repoint`; a binary under a temporary or test-build directory or a
linked worktree is refused without `--allow-transient`, because all vanish while the
repository lives on.

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
to work first, so a lone `SessionEnd` opens and ends it in one pass, and redelivered
starts and ends change nothing. A session opened without its agent's start is a
**`synthesized-start`**, never a `start`, so the audit says what arrived. A refused
trigger is **its own append-only row**, `refused-<trigger>`, because otherwise a
refusal can hide a working session while the receipt reads "recorded".

**The session cache is transient.** The phase is mirrored into
`<git dir>/rift-sessions/<id>.json`; the transition rows are the truth. Writes use a
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
past 24 hours; `--fix` sweeps under the same lock — [05](05-ledger-and-verification.md)).
A deferral carries one cause from a closed vocabulary (`worker-holds-lock`,
`no-idle-edge`, `no-commit-to-anchor`, `archive-produced-nothing`,
`done-mark-failed`); only `worker-holds-lock` is someone else's to finish, and every
other cause waits for the next `rift doctor --fix`.

---

## 3.7 From session to checkpoint: the condense

A **checkpoint** is a tree of git objects on a ref of Rift's own with a Merkle leaf
and a chain entry behind it. A shard holds its metadata, the opted-in working-tree
capture after the gate, `redaction_report.json` (required before a shard is
importable or projectable), the transcript and its compaction, the tool calls
including files read, and four mechanical digests; [05](05-ledger-and-verification.md)
walks the anatomy.

**The compaction is deterministic, not a summary**: tool calls reduce to identifiers,
file reads to handles, no model runs in capture, and the "intent and outcome" slot is
your redacted prompt plus mechanical outcome facts. **Re-condensing an unchanged session is a no-op.** Each subagent gets its own subtree, its tokens roll up to the
parent, and the parent's metadata enumerates the task ids so hiding one is detected,
not merely absent.

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

---

## 3.8 The two latency budgets

Two budgets measure different things; quoting one for the other is the mistake to avoid.

### The git-latency budget: 100 ms

The time a `git commit` spends *beyond a bare commit*, summed over its three hooks —
a displacement, not one process's wall time. It is a hard ceiling set by decision,
not derived from measurements, so it does not move when the machine has a good day.
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
(`<git dir>/rift-sessions/<id>.usage`), **stopping at the next real prompt** so windows
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
records a boundary event's `HEAD` and time. The worker later collects current bytes;
a separate collection-attempt interval records that later work. Collection is not
an atomic snapshot and does not reconstruct bytes at the event time.

Turn-boundary capture preserves a symlink's target spelling without reading its
referent. Rift trusts the directories above the configured worktree root; the root
and every repository-relative component are opened without following links. Regular
reads are capped at 5 MiB; directories, special files, unstable replacements and
larger files produce unknown observations. The object writer receives only the
captured bytes, so a replaced path cannot redirect a second read.

Append-only companion facts distinguish regular files, links, confirmed absence and
unknown observations. A boundary without a file-kind fact supplies no type evidence.
File-kind and collection facts describe observations. They do not establish who
wrote the changes. Rift does not record closed-window hunks that assign those
changes to a writer. Commit-diff attribution stays unchanged, and Rift generates
no new attribution preview percentages from these observations. Existing stored attributions remain unchanged and may be
incorrect; a retry does not repair them. Kind and collection facts share the existing
boundary rows' projection-only lifecycle; checkpoint shards do not carry them.

With no daemon, the hook writes an identifier-only job into its checkout's family
run state, spawns one detached child with its streams going nowhere, and does not
wait. Doctor can attempt the pending work too. Each checkout’s worker holds an
open-file lock at its canonical Git directory. Elapsed time never releases a live
owner. Legacy lock markers defer work until a controlled handoff; the new owner
file does not exclude older installed workers.

Queue context includes the full payload apart from its attempt counter. An
identical context stays queued without resetting retries; a changed session,
worktree or queue time remains separate. Migration removes an old ticket only
when its complete payload, including attempts, matches the installed copy.
Actual execution failures have three attempts. Ownership barriers and unproved
input or completion facts stay owed without spending that budget, and doctor
reports the available reason.

An exact-context condense marker also names an existing checkpoint. Legacy
markers, ended caches and token counts without their original inputs cannot
certify a retry, so some work remains pending. A session claims to be ended only
when a checkpoint exists.

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
an optimisation over it. `rift rebuild` runs it (delete `projection.db` first for a
from-scratch rebuild; over a live projection it is a set-union no-op). Hooks alone cannot
be authoritative: they miss sessions run before installation. The bridge never reads raw session bytes.

A read-only importer lands another tool's checkpoints under their own chain era, never as
a chain anchor ([05](05-ledger-and-verification.md)).

---

## 3.14 What capture never does

No network, no model, no daemon, no blocking, no guessing (an unestablishable fact is
recorded as absent) and no mutation: [17](17-invariants.md) states these as standing
rules and [18](18-what-rift-does-not-do.md) lists the rest.

---

## 3.15 Seeing and repairing capture

`rift status`, `rift session list|info|show`, `rift checkpoint list|explain` and
`rift doctor [--fix]` show and repair capture ([05](05-ledger-and-verification.md)).
`rift session stop|resume|attach` finalize, restore or adopt a session; amending a commit
to add the trailer needs an explicit yes and never runs from a hook.

---

## Related chapters

[02](02-configuration.md) · [04](04-redaction-and-secrets.md) ·
[05](05-ledger-and-verification.md) · [08](08-cost-and-savings.md) ·
[09](09-worktrees-and-families.md) · [10](10-agent-integrations.md) ·
[11](11-canopy-tour.md) · [15](15-benchmarks.md) for the protocol behind every
number in §3.8.
