# 7. Serving and injection

Chapter [06](06-recall.md) explained what Rift *knows*: the delta between the
state an agent holds and the state the repository is in now. This chapter is how
that knowledge reaches an agent — and what stops it reaching one.

Two channels pull in opposite directions. **Push**: at a turn boundary Rift
injects text into the agent's context without being asked, in three tiers — a
one-time pointer that teaches the surface, a per-turn delta manifest naming
what moved, and, at the tool call itself, a decision about a read the context
already holds, and a decision about a search it is about to run. **Pull**: the
agent shells out to `rift recall <verb> --json`, ten verbs in a closed set, and
reads the answer as data.

---

## 7.1 Two channels, one dispatcher

Both reach the one dispatcher, which consults the policy engine
(`.rift/policies/serve.toml`) and the per-turn budget, and both end in one audit
row — two serving paths would be two places for a rule to be forgotten. Push
enters through `rift hooks <agent> UserPromptSubmit` and passes the push-side
chokepoint (frame plus allowlist); pull enters through `rift recall <verb>
--json`, decoded argv → params.

The third tier enters through the same door as the second: `rift hooks <agent>
PreToolUse` reaches one composer of decisions, which knows per family what that
client's published schema allows, and answers neutrally for everything else
([7.4](#74-tier-three-the-decision-at-the-read)).

Two properties hold on every side. **Nothing here blocks your agent**: the push
side runs inside a hook, and a hook that fails injects nothing, never a stalled
or refused turn — the guard is structural, the injection attempt wrapped so a
panic is swallowed and the process exits successfully regardless. A declined
read is not an exception to that rule but an instance of it: the decline is a
response the client documents, the hook still exits successfully, and the
repeat runs. **Nothing here
invents content**: everything a manifest renders and everything a response
inlines passes through one chokepoint that knows the provenance of the bytes,
and untrusted bytes never arrive raw (see
[chapter 4](04-redaction-and-secrets.md) for the capture-side half). Everything
is also measured on the exact bytes shipped.

---

## 7.2 Tier one: the session pointer

The first time a context window is spoken to — a session's first prompt, or the
moment a subagent's window opens — Rift injects about a hundred tokens:

```
Rift is capturing this repository and knows what changed since the state you
already hold. Ask it before re-reading files:
  rift agent-help                 the map of the recall surface
  rift recall known-state --task <goal> --file <path> --json
  rift recall diff-since --known-hash <sha256> --json
  rift why <path>:<line> --json    who wrote a line, its prompt and cost
Every delta carries that provenance already: when, who, what it cost.
Answers are budgeted and audited, and say what they omitted.
```

That text is **124 tokens** by the bundled tokenizer, held under a cap of 150 by
a test so it cannot quietly grow into a manifest. The provenance line earns its
sixteen tokens by closing a loop the pointer otherwise opens: without it an
agent reads `rift why` as a separate errand rather than as the rest of what the
delta it already has is telling it. The verb spellings are read
from the dispatcher's own list, so a renamed verb renames itself here.

Why a pointer rather than guidance in a config file? A pointer is *spend* — it
costs the agent context every session, forever — so it says one thing, "ask
before you re-read", and hands off to `rift agent-help`, which costs nothing
until the agent runs it and is rendered live from this build's command tree, so
it cannot advertise a verb that does not exist.

**Once per context window, and only once.** The right to inject is claimed by an
exclusive create of a marker under `.rift/state/injection/`, named by the *hash*
of the session id and the agent id inside it — the same key, from the same
function, that the window journal is written under
([§6.12](06-recall.md#612-what-a-context-window-already-holds)); an
agent-supplied string never becomes a path component. One process wins it and
the decision counts only once that write has persisted; a crashed or retried
hook re-reads the marker and stays quiet. Deleting it costs at most one
duplicate pointer — local run state, not a ledger fact.

Per window rather than per session, because a coding assistant runs a subagent
under the **parent's** session id. Keyed by the session alone, whichever of the
two reached the marker first took the pointer and the other was never
introduced — and a subagent has no second boundary to be introduced at. The key
changed shape when this landed, so every session already running at the upgrade
is introduced once more: one duplicate pointer, once, and nothing else.

**Every supported family has a boundary channel, and each one is pinned.** Two
of them answer at prompt submission and session start, the third before each
model invocation. Claude Code has one channel more than that — the verb that
opens a subagent's window, which is the only boundary that window ever gets, so
this family pins three boundaries rather than two. One of the three families
declares its own limit on injected text, and
Rift fits the text to whichever is smaller, its budget or that limit, because
over the limit the client keeps a head and a tail and spills the rest to a file
the model cannot usefully open. A channel exists in the code only when a
committed fixture carries the client's published response schema — repository,
tag, path, blob hash and the verbatim excerpt — so "no invented shapes" is a
property of the fixtures rather than a promise
([chapter 10](10-agent-integrations.md)). The managed instructions file is
still written, and is now fallback documentation only: no capture, delivery,
decision or recovery behaviour depends on a model reading it.

---

## 7.3 Tier two: the per-turn delta manifest

Once a session holds the pointer, every later prompt can say something more
useful: *these files you have read changed since then.*

```
[rift] Recorded delta since the state you already hold (facts, not directives).
2 file(s) you have read changed since then:
- crates/rift-store/src/schema/capture.rs@sha256:b6caa84d L12-38,L204-206 (+2)
- bins/rift/src/hooks/gate/mod.rs@sha256:0a1b2c3d (changed)
(3 more not shown)
Detail on demand: rift recall changed-hunks crates/rift-store/src/schema/capture.rs
--known-hash b6caa84d1f5ada80 --json
```

Three things ride on an entry that renders anyway, for about fourteen tokens.
**The hash is the one the session holds**, taken from the window's own read
record first and the manifest baseline second, which is what makes every pull
verb usable: a hint naming a hash the agent has is runnable, and the pushed
alphabet refuses the angle brackets a placeholder would need. **The ranges are
the lines that differ**, in the numbering the agent is about to read, computed
at hook time by resolving the held hash to a blob through the content-to-object
map and diffing it against the worktree bytes, capped per turn and per entry.
**The fallbacks are honest**: an unreachable basis renders `(changed)`, a
binary file `(binary)`, a deleted one `(removed)`, and a span the allowlist
withheld is counted rather than guessed at.

Entries are drawn from the window's held set first and the baseline second, so
the sentence "file(s) you have read changed" is literally true wherever the
record has lines, and falls back to the older behaviour where it has none.

**The baseline and its stamp are keyed by the window too**, by the same
function the pointer marker and the window journal use — the session id and
the agent id inside it, hashed together. The held set was already per window, so a
baseline keyed by the session alone made the two halves of one decision
disagree: a parent would be told that *files you have read changed* about files
only a subagent ever read. The verb that opens a subagent's window is
deliberately **not** a manifest boundary either: a window that has read nothing
has no delta to be told about, so it is handed the pointer and what its
coordinator holds ([below](#what-a-subagents-window-opens-with)), and its first
manifest waits for something it has actually read to move.

The budget is **400 tokens**, set by `injection_turn_budget` in
`.rift/policies/serve.toml` ([chapter 2](02-configuration.md) covers the file).
Small on purpose: a manifest is a *pointer at* a delta, not the delta, and the
last line names the verb that returns the rest.

Set it to `0` to turn injection off: no baseline read, no projection opened. Never set `1`–`33`: the bare frame (header, counter, hint,
naming no file) costs 34 tokens, so a smaller budget injects nothing and still
pays the cost.

### What one turn decides

Each turn resolves to exactly one of four outcomes. **Bootstrap**, on the first
observation of this session: record where the session starts, inject nothing.
**Silent**, when nothing changed: inject nothing, and record that this state is
accounted for. **Unfittable**, when something changed but not even the frame
fits the budget: inject nothing, record nothing — the delta is still owed.
**Inject**, when something changed and a manifest fits: inject it, and fold what
it accounted for into the session's baseline.

*Silent* and *unfittable* were worth code to keep apart. Silence is honest only
when there is nothing to say; a delta that did not fit is one the agent has not
been told about, so nothing is absorbed and no "accounted for" stamp is written,
and the next turn whose budget can carry it announces it — collapsing the two
would turn a one-turn squeeze into permanent silence. Bootstrap exists because a
manifest asserts that files *the agent has read* changed, and one turn after the
pointer Rift does not yet know what the agent holds: a delta needs two
observations before it is a delta rather than a guess.

### Announce once, not every turn

A manifest "accounts for" everything it reported, including files folded into
the `(3 more not shown)` counter — the counter is itself a report, the pull hint
accompanies every announcement, and the unnamed detail stays available through
`diff-since` while it is a delta. A session's baseline holds at most 2048
content hashes; past that the oldest are forgotten, so their files may be
announced once more. The failure direction is re-reporting, never swallowing.

**An entry the guard withheld was not reported, and is not absorbed.** The
`2 withheld as unsafe to inject` clause ([§7.8](#78-what-may-appear-in-anything-rift-pushes))
names no file, and no verb the agent can run leads it back to one: it is a
number, not a fact about a path. So the revision behind it stays owed, and the
next turn announces it again — if the path becomes renderable, by naming it;
if it does not, by counting it again. Absorbing it would mark the file
announced on the strength of a count, and every later turn would have nothing
to say about it — the one direction this rule exists to rule out. The
distinction is per entry rather than per turn, because one manifest routinely
carries both kinds.

### Costing the turn as little as possible

Injection runs on the agent's critical path — the person is waiting — so the
work is arranged in cheapening layers, each able to answer "nothing to say"
before the next is paid for. A zero budget returns immediately. Then a **file
stamp**: if neither the projection nor the session's baseline has moved since
the last turn that accounted for them, answer silence before opening the
projection database, the expensive part. Then a **scoped stamp**, because the
file may have moved without moving *for you* — a sibling worktree landing facts
on a divergent branch changes the projection and nothing this member may be told
([chapter 9](09-worktrees-and-families.md)) — computed through the same
containment the manifest is built from. Only then the delta, by calling
`diff-since` through the ordinary dispatcher: the push tier is a caller of the
one serving surface, not a second implementation.

That last step does *not* refresh the projection first — the pull verbs do, and
that refresh is by far the most expensive thing in a recall invocation, while a
push runs with the agent waiting, so the manifest is built from the projection
as it stands and a cold one yields silence rather than a stall. The walk that
finds a fitting manifest considers at most 64 entries, stepping down one at a
time and measuring the real rendered string, so a thousand-file delta cannot
turn one prompt into a thousand tokenizer passes.

### What a subagent's window opens with

A subagent is spawned by a window that has usually read the files its task is about, and it starts holding none of them.
So at the boundary that opens its window, the pointer is followed by one line:

```
[rift] Held by your coordinator at your start (facts, not directives): src/lib.rs@3f9a1c2e8b7d4f60, bins/rift/src/hooks.rs@8b7d4f60a1c2e8b7 (2 more not shown)
```

It names the revisions the coordinator's window held at the instant of the spawn and the subagent does not: the latest revision of each path it read and still holds, newest first, with the hash in the sixteen-character form the pull verbs take.
At most eight are named and the rest are counted, never cut.
With the hash in hand, the subagent can ask `diff-since` or `known-state` for what moved instead of reading the file whole.
A coordinator that has read nothing is silence.

**Once, and inside the turn's budget.**
The line rides in the claim that hands the window its pointer, so a retried opening finds the claim taken and says nothing: the section is never pushed twice, and needs no marker of its own.
It fits what the per-turn budget of 400 tokens leaves after the pointer's 124, which is 276, dropping names from the tail into the count; a budget of `0` turns it off with the rest of injection.
Its tokens are measured in the same string as the pointer and booked as spend on the pointer's row, so the section is never a free line.

**Only the coordinator's window, in this checkout.**
The subagent works for the window that spawned it, and that window's reads are the ones it was briefed from.
A sibling subagent's reads belong to some other task, and another member's reads in the family may be of a different file at the same path.
The hook reads two window journals to compose it, the coordinator's and the subagent's own: no store and no git object database.

**Facts, never a decline.**
The section names revisions and writes nothing to the subagent's window journal: no read, no hold, no decline.
The read gate keeps deciding from the subagent's own reads alone ([§7.4](#74-tier-three-the-decision-at-the-read)), so a file the coordinator holds is never a reason to skip the subagent's read of it, and no decline ever rests on a line outside the window's own journal.
A test holds the subagent's journal untouched across the push.

**The coordinator's journal is not trusted to be well formed.**
It is a file an agent can write to, so a row is rendered only if its path is a checkout path in the journal's own spelling (relative, `/`-separated, no `..`, no control character) that the push guard passes unaltered, and only if its hash is hex.
Any other row is counted as withheld, as every listing counts its own, and the red-team corpus carries rows of each kind ([§7.17](#717-the-red-team-program)).

---

## 7.4 Tier three: the decision at the read

The two tiers above *ask*. The store says asking is not enough: over its life
this repository's projection holds 190 injected manifests, 463 read calls with
facts, and **8** pulls. A manifest naming what moved is worth its 400 tokens,
but the token is spent at the read, so that is where the third tier decides.
Both frames now end with the one pull that is worth a round trip on its own —
`rift why <path>:<line> --json` — and the deltas the pull verbs return carry
that provenance inline ([§7.9](#79-the-pull-surface-ten-verbs)), so an agent
learns *when, who and what it cost* without spending a call to ask.

At the pre-tool boundary, a read-shaped call resolves to one of three
outcomes. **Allow**, and record the range the tool is about to deliver.
**Observe** — the decision was reached and recorded, and the call runs
untouched; this is how a pool is priced before anything acts on it.
**Decline**, once, with a reason the model reads.

The predicate is narrow on purpose, and every clause of it closes something:

- the lever is in `enforce` for this family ([chapter 2](02-configuration.md));
- the call is read-shaped **in the fields this family's pinned surface names**,
  never in fields inferred from a shape that looks similar;
- the file is text and small enough to hash inline (1 MiB), so no decision
  waits on I/O it cannot afford;
- the slice is within the ceiling (96 KiB — the measured point at which a read
  result truncates in place rather than being kept whole) and at or above the
  floor (28 lines). The ceiling is about the *decline*: it bounds what one
  decision may cover, and it is unmoved by anything below;
- the requested range is **wholly inside** coverage at the file's *current*
  hash ([§6.12](06-recall.md#612-what-a-context-window-already-holds));
- and this window has not already been declined for that exact key since its
  last eviction.

A request that is **partly** held is decided on the same footing, as a
narrowing.
Where the window's own reads at the current hash hold one run at one end of
the request, and the rest of it is one contiguous run, the decision names both.
The held run must be lines the window read — never its edits, never another
window's, never lines from before its last eviction — and the floor is counted
on the held lines against the narrowing frame's own price (17 lines).
Held lines in the middle of a request, or in two runs, leave two windows to
name, and the read runs.
Narrowing ships in `observe` whatever the family's mode, so the decision is
recorded and the call runs; the observed call is recorded as a read too,
because the lines it delivered are now coverage.

Anything else allows. An unknown span allows, an unpinned family allows, an
unreadable policy turns the lever off, a panic anywhere in the gate yields the
family's neutral response, and the hook exits successfully in every one of
those cases.

**What the window holds is decided at the result, not here.** A read the gate
lets through is credited with what the tool *can* have delivered, and past the
size at which a measured density stops vouching for a slice it is credited with
nothing. The client's own result then says what actually arrived —
the run it handed over, the file's length, whether a token cap cut it short —
and that statement is believed over the arithmetic in both directions
([§6.12](06-recall.md#612-what-a-context-window-already-holds)). A whole-file
read of a seventeen-thousand-line source file becomes coverage that way and no
other. Two consequences follow and both are deliberate: the pre-call boundary
above is unchanged, because a decline may never rest on a delivery that has
not happened yet; and the credit lands one hook later, so two identical reads
issued in the same turn reach that boundary before either result does and both
run. A saving not taken, which is the direction every rule on this path leans.

**Read-shaped** includes the shell, because one supported client reads files
that way — and, measured on this repository's own transcripts, reads them that
way far more often than through its read tool. The grammar has two levels. The
**vocabulary** is a client's own read-before-edit list, unchanged: `cat`, `nl`,
`bat`, `batcat`, `head`, `tail`, `sed -n 'A,Bp'`, the `grep` family and `rg` on
one named path, plus a listing grammar of the same shape (`ls`, `find … -name`,
`rg --files`). A search on that one path may carry the flags that only choose
and number the file's own lines or count them (`-n`, `-i`, `-w`, `-x`, `-c`,
`-F`, `-E` on `grep`, a context count `-A`, `-B` or `-C N`, a pattern given
with `-e`), and it stays
what a bare one is: recognised, with no coverage, because which lines it
printed is the file's to decide. Those options may stand anywhere before `--`,
after the pattern or the path too, because the `grep` an agent's shell runs
permutes its arguments (Claude Code runs `grep` as `ugrep`, which permutes, as
GNU `grep` and `rg` do). A `grep` that does not would misread such a line,
which only moves a measurement, since a search can never decline a read; any
later form that grants coverage from a late option must first establish which
`grep` runs.
A search may name several files (`grep -n x a.rs b.rs`): it opens those and no
others, so it is one search of each, recognised with no coverage; narrowed by a
pipe it is left alone, because the cut falls on the files' matches joined.
A `sed -n` script may also print one line (`'Ap'`), credited like a range, or
several runs joined by `;` (`'1,40p;90,120p'`), which is one read per run and
is credited with nothing: the runs reach the window as one output that can
pass the terminal threshold while each run's own slice stays under it.
One program outside the client's list is read too, because it is a line pick
in disguise: `awk 'NR>=A && NR<=B' a.rs` prints exactly what
`sed -n 'A,Bp' a.rs` prints, so it is that range with that credit; any other
`awk` program, a number with a leading zero (which `gawk` reads as octal), an
option, a second file or an operand holding `=` is left alone.
A quoted word is read as the word the shell passes: single quotes as their
text, and double quotes as their text when they hold no `$`, backtick or `!`,
with every backslash before an ordinary character kept as written
(`grep -n "a\|b"`) and an escaped `"` or `\\` taken as the one character it
escapes (`grep -n "\"decision\""`); an escaped quote never closes the quote,
so a separator after it stays in the pattern. Any other escape (`\$`,
`` \` ``, a line break), an expansion, and a quoted path that spells a glob, a
`~` or holds any backslash are left alone. A quoted path is credited exactly
as a bare one is.
A read or listing command may be spelled from `/bin` or `/usr/bin`
(`/usr/bin/sed -n '1,40p' a.rs`), the system's own copies, and reads exactly
as the bare word; any other directory, and a `cd`, a label or a narrowing
command spelled that way, is left alone.
Above it sit exactly three **compositions**, each admitted
because the lines it delivers still follow from the line itself:

- a single leading `cd <dir> &&`, the directory bare or quoted, after which
  every relative path in the line is read from there (a rooted path is
  untouched). A `cd <dir>;` is the same `cd`: the reads after a `;` run even
  when the `cd` fails, but it fails only where the directory cannot be
  entered, so every path read from it then names a file nobody can open and
  nothing is recorded;
- one trailing `| head -N`, `| head -n N`, `| tail -N` or `| tail -n N` on a
  member that is one read, which narrows the run that member produced to a
  prefix or a suffix of it. A suffix is a *count from the end*, so it travels
  as a count and the step that already holds the bytes for its hash turns it
  into line numbers — which is what also brought a plain counted `tail -n N`,
  long recognised and never creditable, into the ledger. Every count is plain
  digits: a signed one (`tail -n +5`, from line five to the end) means
  something else to the command, so the line is left alone. The one composition
  with no interval is the first `N` of the last `M`: its start is a number the
  file's length would have to supply, and no span says that, so it is unknown
  and records nothing.
  A trailing `| sed -n 'A,Bp'` (or `'Ap'`) narrows the same way: lines `A` to
  `B` of the run, cut at its end, so `sed -n '100,200p' a.rs | sed -n '1,10p'`
  is lines 100 to 109.
  A pick of a run counted from the end, or one that starts past the run and
  prints nothing, is unknown; a `sed` without `-n`, with a second element or
  with a file after the script is left alone;
- members joined by `&&` or `;` in which **every** member is itself a
  recognised read, so one call yields several requests (a `cat a b` does too).
  A member that is an `echo` or a `printf` over plain operands is a **label**:
  it opens no file, so it contributes no request and disqualifies nothing, and
  a chain of reads and labels is decided on its reads alone — which is what
  admits the captioned chain (`cd … && echo "=== a ===" && sed -n '1,80p' a.rs
  && echo && sed -n '100,140p' b.rs`) that was the largest shape still going
  unseen. A line that is labels and nothing else reads nothing and is not a
  read.

Everything else is what it always was. A pipe into anything but those
narrowing commands, a member that is neither a read nor a label, a label whose
operand a shell would act on, a second `cd` or one that is not at the front, a
listing standing among reads, a redirect, a glob, a variable, a substitution, a
background `&` — each takes the **whole** command
line out of the grammar rather than costing only the part that stepped out.
Nothing is guessed at; the recogniser is total and has a fuzz target for
exactly that reason, and a chain is capped so that neither the classification
nor the sentence a decline would render can grow without bound. A call carrying
a field the pinned surface does not name — a page range, a notebook cell — is
not read-shaped either.

**A call that performs several reads is one act.** Every member's range is
recorded; the call is declined only when *every* member is a held range that
would be declined on its own, and one member the window does not hold — or one
this checkout cannot resolve at all — lets the whole line run.
A member held at one end counts as held where its narrowing would be declined
on its own, so a chain is declined only when every member is held or
narrowable, and that member's line is the narrowing frame. The reason frame
is then each member's own sentence, from the same sealed renderer, one to a
line. The alternative, declining the parts and running the rest, would mean
amending a command line Rift did not write.

**A decline is a decision on a channel the client documents; it is
never a block** — the guard of [§7.1](#71-two-channels-one-dispatcher) is
unchanged, and a family whose reason channel no recorded run has confirmed is
never declined to at all.

### One decline per key, which is the loop breaker

The identical repeat of a declined request is **allowed** and recorded. That
single rule bounds a decline to one extra round trip and makes starving an
agent structurally impossible: whatever the model does with the reason, the
bytes are one call away.

It is not a hypothetical. On the client whose reason channel is confirmed by
the docs, eleven scripted runs declined the second read of a file and **none**
re-issued it — every run answered correctly from context, so the decline cost
one round trip and no re-read. On the other two families, ten runs each
re-issued the identical call immediately and got it, on both the plain and the
delta frame, with the same sealed text. The difference is the family, not the
wording. That is why the retry rate is published as telemetry beside the
savings rather than used to gate anything: the escape working is not a failure,
and a rate cannot be allowed to demote a family whose channel is otherwise
sound.

### The reason frame

One sealed renderer beside the manifest renderer produces every frame. Its
fixed phrases are authored in code, its variable spans — path, twelve hex of
the hash, line numbers, the turn that established the coverage — each pass the
same allowlist [§7.8](#78-what-may-appear-in-anything-rift-pushes) applies to a
manifest entry, and it carries no file content, no placeholder brackets and no
apology. It states the fact, the escape hatch, one cheaper route and one handle
to the story behind the change:

```
[rift] Read skipped: docs/guide/06-recall.md@sha256:8099723c1f5a lines 1-40
are unchanged since your read in turn 1 and are already in your context.
Repeat this exact call if you still need it (it is allowed once). To see only
what changed since then: rift recall changed-hunks docs/guide/06-recall.md
--known-hash 8099723c1f5ada80 --json Provenance: rift why
docs/guide/06-recall.md:1 --json
```

The last line is fixed text plus a path and an integer, which is the whole of
what a frame may add on this path: it names where *when, who and what it cost*
are kept without opening anything to find out. The plain frame names the first
line it declined; the delta frame names the first line that moved, because a
window opens on context and `rift why` asked about a context line answers for a
line that did not move. The path span passes the same allowlist every other
span does, and the whole clause is dropped rather than rendered broken when it
would not be runnable.

Three things it deliberately does not say. It does not offer a summary — the
rung the store shows nobody pulls. It does not claim an earlier tool result
echoed the content, because no family was measured returning one Rift could
rely on. And it never uses an error word: the frame is feedback about a
decision, not a failure report.

**The narrowing frame** names the held run, the turn the window last read any
of it in, and the run still to read:

```
[rift] Read narrowed: src/lib.rs@3f9a1c2e8b7d lines 1-40 are unchanged since
your read in turn 3 and already in your context. Read lines 41-120 for the
rest, or repeat this exact call for the whole range (it is allowed once).
```

It carries neither the pull hint nor the provenance handle: both are routes to
a change, and this frame is about lines that did not change.
What a narrowing is booked at is its held run, net of the frame; the rest of
the request is the read the agent still makes, counted when it happens.
The call itself is never amended.

**The delta frame** is the same shape for a file the window holds at an *older*
hash. When the difference is known exactly — from the agent's own edit
([§6.13](06-recall.md#613-the-agent-holds-what-it-wrote)), or from a basis blob
the content-to-object map still reaches, diffed against the current bytes — the
frame names the changed sites in current numbering, the net shift, and a window
to read instead of the whole file. When no site survives the clamp against the
new line count, there is **no** delta frame and the read runs: a delta frame
without exact sites is the plain frame with a false claim attached. The
frame's numbers are computed from the bytes on disk, so they are exact or
absent.

A basis is not always there, and reaching one is a **ladder of two rungs,
ordered by exactness rather than by cost**.

The first rung is the git blob, through the content-to-object map. Within the
map's window a committed revision resolves in every measured case; past the
window it does not, and a revision that was never committed has no blob at
all — which is exactly the revision a window is most likely to be holding,
since an agent reads a file and then edits it.

So the second rung is a **read-time snapshot**: the bytes of every revision a
read recorded, kept once per repository family beside the document sidecars
and keyed by the read's own content address — the same address the hold lines,
the manifest entries and `--known-hash` all travel under. Losing one costs a
delta frame and never an answer: the blob route is untouched and the file is
still in the checkout.

**No raw read-time bytes are persisted**, and that has not changed. A snapshot
is written through the redaction gate, which is the only path to durable
storage ([chapter 4](04-redaction-and-secrets.md)) and the only mint of the
type the store's writer accepts. What that costs is stated rather than hidden.
A mask is a difference *Rift* wrote, not one the repository made, so a diff
computed against a masked span would name a changed place that never changed —
and, on the verb that serves lines, would return the **live** bytes of exactly
the span the gate took out. So the snapshot records the runs of lines the mask
touched, one hunk that rests on one takes the whole answer down to `(changed)`
with no lines named, and `changed-hunks` refuses the lines outright and
answers with the neutral digest under `source_deleted_or_redacted`. A mask
that *moved* a line number — a multi-line secret collapses to a single
placeholder — is not a snapshot at all and is never written: a basis whose
numbering is not the revision's can only produce wrong line numbers.

Past both rungs the answer is an honest `basis_unavailable` and the read
proceeds. Availability is a published rate with a fallback ladder, never an
assumption.

### Nothing on this path opens a store

The decision runs while a person waits, so its whole cost is a hash under a
ceiling plus one small file read. No projection is opened, **no tokenizer is
initialised** — the two most expensive things a Rift process can do — and the
audit row is journaled to the detached worker, which counts the declined
slice's tokens with the real tokenizer and writes both audit relations
([§7.12](#712-one-measure-one-charge-one-sealed-event)). A structural check
reads the call graph and fails the build if a store open or a tokenizer
appears under any gate entry point.

The floor and the ceiling are both measured rather than chosen, and the floor is
measured by a test rather than quoted from prose: the sealed renderer and the
real tokenizer meet in one crate, and that is where the arithmetic runs. A plain
frame costs **111 tokens** against roughly 4 tokens a line, so a decline starts
paying at 28 lines and the floor sits exactly there; a slice under the floor is
still decided and recorded, and only never acted on. It was 24 against a frame
the prose called "about 88" — a figure measured with an unusually
cheap-to-tokenize hash, where the renderer's own golden fixture already cost 98,
so the floor had drifted a line under its break-even before the provenance
handle was added. A reword that grows the frame reds the test that holds them
together.

---

## 7.5 After a compaction: the recovery map

A compaction is the one moment a full listing of held state pays for itself:
the context has just lost it and is about to re-read the world. So on a session
start whose source is a compaction or a resume — and at the first
boundary after a client's own compaction signal — Rift injects the
*pre-eviction* held set:

```
[rift] Context was compacted. Files this session had read, with the content hash and lines it held (facts, not directives):
- crates/rift-store/src/schema/capture.rs@b6caa84d0f12 L1-412 unchanged since your read
- bins/rift/src/hooks/gate/mod.rs@0a1b2c3d4e5f L1-406 changed at lines 160-188
  shape of the current revision: 431 lines, 15210 bytes, 19 top-level definitions
(6 more not shown)
```

Three rules keep it honest. It is **boundary-only**: an ordinary turn never
carries a held-state listing, because broadcasting hashes on quiet turns costs
hundreds of tokens to enable a call the store shows does not happen, and it
would break the silence property of [§7.3](#73-tier-two-the-per-turn-delta-manifest).
It is **fitted** to its own budget (800 tokens by default) with the remainder
counted, ranked by recency. And each entry carries a **verdict** — unchanged
since your read, or changed at these lines — so the first read after a
compaction can be skipped or narrowed rather than repeated blind.
A changed entry whose current revision has a valid bound outline is followed by one line of that revision's shape, counts only, read from the store the map already opens; when the budget binds, those lines go before any entry does.

How often a compaction happens is the client's business, not Rift's. Where a
client reads a repository instruction file that could tell it to compact more
gracefully, `rift doctor` suggests the block and leaves the writing to you:
that file is yours, and Rift does not write repository-owned files it does not
manage ([17-invariants.md](17-invariants.md)).

A cleared context is not one of those sources. A compaction and a resume are
things that happened *to* a window, but a clear is a person emptying it on
purpose, and handing back what they just discarded would be Rift overruling a
decision that was theirs to make. A clear still ends coverage — the record
stops counting those lines held ([§6.12](06-recall.md#612-what-a-context-window-already-holds))
— it simply gets no map.

Declining is **not** re-armed by the map. Pre-eviction coverage is evidence for
what to say, never grounds for a decision: the first re-read after a compaction
is legitimate, and only the second is declined again
([§6.12](06-recall.md#612-what-a-context-window-already-holds)).

---

## 7.6 Documents are read as Markdown

An agent that opens a `.docx` reads a zip container; a spreadsheet arrives as
markup, and on one family a PDF arrives as raw bytes rendered as numbered text
lines. Every one of those is a full-price read of something the model cannot
use. So Rift converts documents to Markdown once, keeps the result, and puts it
where the read lands.

**The converter is a seam over a pure-Rust engine**, covering fourteen
document extensions, with no OCR and no network — the hosted extraction path
that engine offers through its foreign bindings never compiles in. Markdown,
plain text and source files are never touched. Tables are capped per sheet
across all sheets, a document over the input ceiling is never converted, and a
PDF records which pages carried no text layer.

**The sidecar is content-addressed**, keyed by the document's bytes together
with the converter's version and the caps that shape its output, so a changed
document, a changed converter or a changed policy all key somewhere else and
nothing has to remember to invalidate anything. It is written once per
repository *family* and viewed per member, which is how one conversion serves
every worktree ([chapter 9](09-worktrees-and-families.md)).

**Where the client documents an input amendment**, a document read's path is
amended to the member's view of the sidecar, and the agent reads Markdown
without being told to. Rift amends a call only in a **closed set of pinned
additive forms** — each one a field the client's published schema documents,
each verified against the executed call afterwards — and never on a decline,
never any other field. Two guards sit on the amendment: a read that names a
page range is left alone, and so is a PDF whose text layer averages under the
per-page floor, because a scanned page has nothing to convert and a sidecar of
nothing is worse than the original read. Conversion on the gate path gets 250
ms, in a child process — `rift` re-spawned on a hidden route, so the deadline
is enforced by killing it and a hostile document dies in another address
space; past it the read runs untouched and the work is journaled for the
detached worker, so the second read is a cached hit.
The 250 ms is the hook's whole wait, measured from before the spawn: the spawn
and the reap run on a thread of their own, because a first exec the system has
to assess can take seconds under load and cannot be interrupted, and the hook
leaves when the budget does. A child that has spawned is dead when it leaves —
the hook kills it on the way out, since the hook's process exits straight after
and takes that thread with it — and the thread reaps it behind. A child whose
spawn is still in flight is killed when the spawn returns; if the hook is gone
by then, the child finds an empty document on a pipe whose writer died with
the hook, refuses it and exits.

**An amendment that does not take effect is caught, not assumed.** The executed
tool input is compared afterwards with what Rift substituted; a mismatch — a
sibling hook's response winning, which one client's own issue tracker
documents — records a dropped amendment and flips that window to **redirect**:
the next document read is declined once, naming the sidecar path in the reason.
A repository that would rather be told than amended can choose redirect
outright, and `rift doctor` warns when another hook is registered on a verb
that can answer a read.

**The families with no amendment channel get the same value by another route.**
The detached worker converts the member's documents proactively at session
start and after commits, in bounded passes, so the sidecars exist before
anything asks; the boundary text names the directory; and a strict-grammar
shell read or a document view of a converted document is redirected once, on a
confirmed reason channel only. The pull spelling is the same everywhere:
`rift recall doc-read <path>` (alias `rift doc read`), which serves the whole
converted document through the chokepoint under the ordinary path policy.

On a generated corpus of one document per format, the substitution removes
**90.94 %** of what the same reads would have cost natively — 107,784 tokens of
pool against 9,761 received — and the pull verb, which pays for its own answer
envelope, removes 89.72 % of the same pool. Both figures are quoted on the
formats that have a price: a PDF's native cost is a count of rendered pages
against a vision rate nobody has pinned, so its baseline is **zero** and the
tokens it saves are excluded rather than estimated
([chapter 8](08-cost-and-savings.md)).

---

## 7.7 Searches are bounded and sometimes answered

Reading is not the only way an agent spends context on your repository. It also
*searches*, and the two kinds of search behave so differently that they get
opposite treatments.

**A content search that named no bound is given one.** A search inside file
contents has an answer no index of Rift's can stand in for, so it is not
answered, not substituted and not declined — it is bounded. Where the client's
schema documents a result count, Rift adds the default 40 to a call that
carries none, and nothing else about the call changes. The tool states its true
total even where that bound truncates the listing, so the model learns what it
did not see, and the follow-up that asks for the rest runs with **no bound at
all**: the key that makes the amendment once-only is the search itself with the
paging parameters removed, so the same search asked again is unbounded by
construction. Rift writes nothing into the result. A call that named its own
bound — an explicit zero included — chose one and is left alone, and where a
sibling hook's amendment displaces Rift's, the executed call says so
afterwards: that window stops amending and the fact is recorded, never
escalated into a decline, because a content search is never substituted.

**A name search is answered from the index Rift already holds.** A glob, or a
recursive listing written in the grammar a client documents for its shell, asks
where files are — and [§6.14](06-recall.md#614-knowing-where-the-files-are) is
exactly that question. So the call is declined once and the answer takes its
place:

```
[rift] Search answered: 8 of 143 paths matching the pattern you searched for,
ranked by relevance to this session (facts, not directives):
- crates/rift-store/src/schema/capture.rs
- bins/rift/src/hooks/gate/mod.rs
…
Repeat this exact call for the full list.
```

The frame states the total it is a head of, says what the order means, and
names the one call that returns everything — the identical repeat, which the
one-decline-per-key rule allows. It usually declines to quote the pattern
back, as above, because a glob is spelled with characters the pushed alphabet
does not admit; the agent wrote the pattern, so naming it by reference costs
nothing ([§7.8](#78-what-may-appear-in-anything-rift-pushes)).

Four rules hold the answer to what it can defend. **From the index or not at
all**: warm, it is matched in this process inside the hook's budget; cold or
stale, the agent's own call runs and a rebuild is asked of the worker — a hook
never walks a tree. **Once per pattern and directory per context window**, and
only on a family whose reason channel a recorded run confirmed. The identical
repeat runs in full — that is what makes the frame's own escape true — and a
repeat inside the turn that was answered is counted, because a lever that sends
the model straight back for the whole list has saved nothing. **Names and a count,
never contents**, and never a name the walk excluded. And **the answer stands
down when its frame would cost more than the listing it replaces**, which is
why the weakest case for this lever is no lever rather than a loss — on a
listing of a handful of paths the answer is simply not made, and on a short one
it is made and removes little, which is published rather than averaged away.

The asymmetry is the point, and it is [§7.9](#79-the-pull-surface-ten-verbs)'s
boundary in another form. Answering the question Rift can answer completely is
a saving; standing in for the question it cannot would send a model looking for
an answer that is not there. The same reasoning decides what the bounded case
publishes: the share of bounded searches that are asked again in the same turn
is the honesty figure this lever is judged by, defined in exactly one place —
the window's own record — so no surface can recompute it into something
flattering ([chapter 8](08-cost-and-savings.md)).

The pull spelling of all this is `rift recall search <query>`, which answers
from the same index without declining anything
([§7.9](#79-the-pull-surface-ten-verbs)).

### What a search cost is measured, whatever Rift did about it

Bounding a search and answering one are the two things Rift *does* to a
search. Neither of them is what a search **costs**, and until the capture
projection had a relation that could hold one, that cost was invisible: a
search resolves to no file, so it has no read fact and no token count, and the
denominator every published share is taken over could only ever contain
read-shaped calls.

It is measured now, and off the hook path like everything else here. The
post-tool hook journals what it would take to reproduce the call — the glob or
the command line and the tree state it ran against, identifiers only — and the
detached worker runs the search again, read-only, and counts the output with
the real tokenizer. A glob and a literal content search are matched in process
against the index and the content matcher Rift already has, so nothing is
executed; what is executed is what cannot be matched here, through an
allow-list of four read-only verbs spawned as argv with no shell. A
composition that needs a shell — a pipe, a redirect, a substitution — is
recorded as a call nobody measured rather than dropped, and the count of those
is published beside the denominator.

Nothing about this is an avoided figure: the numerator is exactly what
[§8.6](08-cost-and-savings.md#86-what-avoided-actually-measures) says it is,
and a content search still declares no baseline at all. What changes is the
**denominator**, which grows — so every share this repository publishes gets
smaller, which is the direction that tells you it is not the one being chosen.

---

## 7.8 What may appear in anything Rift pushes

Injection is the dangerous direction: a pulled answer is JSON the agent asked
for and can treat as data, while pushed text lands beside the person's own
prompt, read with the same weight. So everything Rift pushes — a manifest
entry, a reason frame's variable spans, a recovery map's lines — passes a
stricter guard than anything else it emits. The frames are authored in code,
never interpolated from data; only the entries, spans and counts come from the
repository, and each of those must survive all of this:

- **An alphabet allowlist.** A rendered span may contain letters, digits, space
  and `. _ / @ : + -`, nothing else. An allowlist on purpose: enumerating escape
  syntaxes is unwinnable, while naming the ~70 characters a path or handle needs
  is not, and it closes non-ASCII, the escape spellings that re-introduce it,
  chat template brackets, code fences and backticks in one move.
- **Turn-marker vocabulary.** Spellings that mean *a new turn starts here* —
  `system:`, `developer:`, `user:`, `human:`, `assistant:`, `model:`, `tool:`,
  `function:` and the template-style tags — are refused anywhere in a span, not
  just at its start, because the span sits on one line of Rift's frame. The
  membership rule: a spelling belongs there if and only if a model can read it
  as a turn boundary in the message schema or prompt format of a client family
  Rift installs hooks for. Everything else is *content*, which a manifest exists
  to name, and a test fails the build if a family is added without deciding its
  dialect.
- **Imperative shapes**, such as a line opening by telling the reader to ignore,
  disregard or forget something.
- **Frame mimicry.** A span that *starts like* one of Rift's own frame lines is
  refused outright. Four of the five frame lines carry punctuation outside the
  pushed alphabet and cannot be reproduced byte for byte anyway, so this rule
  closes the paraphrase; it skips a leading digit run first, so
  `2 file(s) you have read changed since then:` cannot walk past a rule written
  for the phrase without its count.
- **A length cap.** A span renders at most 120 characters: a path is a name, not
  a document, and one hostile entry must not eat the turn's budget. Truncation
  happens *last*, on the whole span's verdict, so a directive buried past the
  cap is withheld rather than trimmed into innocence.

Anything refused is **counted, never quoted**: the manifest says `2 withheld as
unsafe to inject` and names nothing. One whose entries were *all* withheld still
renders — frame plus counter — because silence would tell the agent its state is
current when it is not, and on a repository whose paths are simply not ASCII
nothing hostile is involved. A counted entry is also one the turn does **not**
fold into the session's baseline: it stays owed
([§7.3](#73-tier-two-the-per-turn-delta-manifest)).

---

## 7.9 The pull surface: ten verbs

The pull channel is a closed set of ten verbs. One of them searches this
checkout's *files*; there is still no transcript search and no open-ended
session search — a deliberate boundary, because an unbounded search surface
over remembered content is an exfiltration channel with a friendly name, and
the file search is bounded by the same policy every other verb answers under.

| Verb | Answers | Requires |
|---|---|---|
| `known-state` | manifest first: what changed, what you hold, what to retrieve — and, per changed file, its provenance | `--task`, `--file` |
| `diff-since` | changed vs unchanged, given the hashes you hold — with provenance on each changed entry | — |
| `file-if-changed` | is this file still what I last saw? | a path |
| `changed-hunks` | the changed lines against a hash you hold — their content, their provenance, and a checkpoint per hunk where one recorded hunk explains it — or the ranked form | a path + `--known-hash`, else `--task`, `--file` |
| `read-summary` | a hash-bound summary, or `stale` | a path |
| `retrieve-exact` | exact bytes by handle — policy-gated | a handle, `--reason` |
| `cost-hotspots` | where the context tokens go (awareness only) | — |
| `context-pack` | one ordered, budgeted evidence pack | `--task`, `--file` |
| `doc-read` | a document as Markdown, from the sidecar Rift holds | a path |
| `search` | ranked paths matching a query, or with `--content` the matching lines | a query |

`search` is the pull spelling of [§7.7](#77-searches-are-bounded-and-sometimes-answered)
and is also reachable as `rift search` — one verb with two spellings, parsed
into the same call. It answers from the member's path index, says `pending` and
claims nothing when that index is cold rather than walking the tree while you
wait, and never names a path the serving policy denies or the redaction policy
calls sensitive — the total it reports counts those, so the count stays honest
about what it will not show you. Its `--content` form opens only tracked,
indexed files and returns matching lines as code facts through the same trust
chokepoint everything else passes; it declares **no baseline at all**, so a
content search books zero avoided tokens by construction, because what a native
content search would have cost is a number nobody has pinned
([chapter 8](08-cost-and-savings.md)).

Every verb is one short-lived process, takes `--json`, and answers in one shot.
The ranking verbs (`known-state`, `context-pack`, and `changed-hunks` in its
ranked form) refuse to run without `--task` and at least one `--file` — ranking
without a stated goal and focus set is guesswork, and guesswork dressed as a
ranked answer is worse than no answer — a usage error, exit code 2.

`changed-hunks` is one verb with two shapes because it answers one question
with different amounts of the caller's own state: given a path and the revision
the caller holds it returns that file's changed lines, and given neither it
returns the ranked form. Every verb that takes `--known-hash` accepts a
**prefix** of at least sixteen hex characters, resolved against the family's
recorded content hashes — exactly one match resolves, and zero or several read
as *unknown*, which is answered as "changed" rather than guessed at. Twelve hex
is the display form, sixteen the argument form, and nothing shorter is
accepted. Every verb also accepts
`--cross-branch <REASON>`
(see [7.15](#715-grants-widening-what-a-serve-may-see)), and the ranked and
delta verbs accept `--token-budget`.

### Provenance rides with the delta

Three of those verbs carry a `provenance` object on every file they report as
changed, so an agent learns where a change came from at the moment it learns
that there was one, rather than after it has already decided where to look:

```json
"provenance": {
  "checkpoints": { "from": "3f1a…", "to": "9c40…" },
  "recorded_at_ms": 1757894400000,
  "session": "01J8ZQ…",
  "agent": "claude-code",
  "token_usage": { "uncached_input": 1200, "cached_input": 30400,
                   "cache_creation": 900, "output": 640 },
  "cost_micros": 41230,
  "commit": { "oid": "9f3c1a2b", "subject": "fix(recall): one /-spelling chokepoint" },
  "why": "rift why crates/rift-store/src/schema/capture.rs:412 --json"
}
```

**Digests and identifiers, never prose.** Nothing in it was written by a model:
no prompt text, no summary, no reply. `checkpoints` brackets the change —
`from` is the checkpoint that recorded the revision you hold, `to` the one that
recorded the revision this answer is about — and **both ends are looked up by
revision**, so a file edited in the work tree and never read again simply has no
`to`. A missing key is *not known*; it is never a zero. `changed-hunks`
additionally names a `checkpoint` on a hunk when exactly one recorded hunk
covers the window it is showing, and none when two do, because a window
overlapping two recorded changes was established by neither alone.

**Every figure names its denominator.** `token_usage` and `cost_micros` are the model calls the bracket's later end froze — `to`, or `from` when only that end exists — and nothing else.
That is the checkpoint's first session's calls started after that session's previous checkpoint and no later than this one, so the window is whole turns of one session: a checkpoint is cut at a condense.
`rift why` prints the same figures for a line that checkpoint answers, read through the same store-side window, with the money a current catalog quote of exactly the calls the tokens count.
A session's total is never reported under a checkpoint's name, because the object brackets a change and a reader takes the figure for what that change cost.
A checkpoint with no recorded time has no window, so its object carries neither key: unknown, never a zero.

**Two sources, and no join between them.** The checkpoints and the anchor commit
are recall-projection facts; *when*, *who* and *what it cost* come from the
capture projection and from git, through a seam the pull process fills, because
`recall.db` never joins a capture table ([chapter 13](13-storage-and-schema.md)).
The push tier fills that seam with nothing at all, so no hook decision ever
waits on a second projection.

**Trust classes are unchanged.** The agent family is an identifier filtered to
the label charset; the commit subject is a git fact, one of the three classes
[§7.13](#713-trust-policy-and-the-shape-of-a-refusal) permits inline, bounded
and stripped of everything a terminal or a chat template would read as
structure.

**It is a rider, and it is sacrificed first.** The object is serialized into the
payload the single accounting seam measures, so it is spend like everything
else. When a turn's budget will not hold it, the whole answer downgrades to

```json
"provenance": "available_by_handle",
"why": "rift why crates/rift-store/src/schema/capture.rs:412 --json"
```

before a single delta is dropped — a delta the caller cannot see is a re-read it
has to do, while a delta whose story it cannot see is one command away. That is
the sensitive-path rule of [§7.13](#713-trust-policy-and-the-shape-of-a-refusal)
applied to the rider instead of the body: serve the handle rather than half the
thing.

A verb invocation goes argv → params → the shared decoder → the dispatch the
push tier uses. The decoder is *total* and fuzzed: arbitrary parameters
never panic it, ill-typed fields fall back to defaults, and only an unknown verb
is an error, because the surface is closed. The sibling surface for people is
`rift memory diff-since | file-if-changed | cost-hotspots | reread-report`,
asking the same dispatcher the same questions but hashing your worktree to
supply the known state, where the agent surface makes the caller state it.

If Recall is not enabled for the repository, every verb answers with exit code 4
rather than silently creating a projection.

---

## 7.10 The per-turn budget

Over-retrieval is the failure mode this surface is most exposed to: an agent
that calls until something useful comes back has re-read the world by a longer
road. A turn carries **4 tool calls** and **2000 returned tokens**.

A `known-state` call **anchors** the turn, zeroing the draw-down while keeping
the ceilings ([6.3](06-recall.md) has the reasoning), which is why the pointer
and the agent-facing help both say *start a turn with `known-state`*. Hitting
the call ceiling is a refusal saying exactly that: `known-state: refused
(budget_calls_exhausted)` on the human line, with a document hint to start the
turn with `rift recall known-state`. The token ceiling uses saturating
arithmetic, so a hostile or enormous figure cannot wrap the counter back under
it, and a charge that would breach it accounts nothing.

> Note the honest limit: the budget is held by the *caller*, and today each
> `rift recall` invocation is its own process with a fresh one, so the ceilings
> bind *within* one call — the next section's caps compose against the 2000
> tokens, a larger charge being clamped rather than allowed to breach — rather
> than accumulating across calls ([Chapter 18](18-known-gaps.md)).

---

## 7.11 Caps and truncation

Two verbs return lists that can grow without bound (`known-state` and
`diff-since`), and an unbounded list is how the first real serve overflowed the
calling agent's own tool-result limit. So list responses are capped: with no
`--token-budget`, **1200 tokens**; with one, the request's value clamped to a
hard maximum of **4000**; either way composed with `min(…, tokens left in the
turn)`. The default/max split mirrors the pack compiler's exactly, because a
flat default of 4000 against a 2000-token turn ceiling would let one anchoring
manifest eat the whole turn and fail every follow-up call in it.

**Truncation is deterministic and says so.** The response keeps the largest
fitting prefix, found by binary search plus a defensive walk-down (token counts
are not formally monotone under concatenation, so "it fits" is guaranteed rather
than assumed), and reports `truncated: true` with an `omitted` count. Drop order
is fixed per verb: `known-state` keeps the highest-ranked entries, since rank is
the point of the manifest, and `diff-since` concatenates
`[changed…, unchanged…]` and drops from the tail, so cheap unchanged paths go
first — which also lets the push tier read a truncated answer correctly, since
if any unchanged entry survived, every changed one did.

Two edges: if entries exist but none fit, the empty skeleton still ships with
`truncated: true`, because `known-state` is the turn's anchor and failing loud
would strand the agent (unlike `context-pack`, which refuses with a documented
fallback — call `changed-hunks`); and if there was nothing to drop, the skeleton
ships *untruncated*, since `truncated` must never claim an omission that did not
happen. Two smaller caps: `changed-hunks` returns at most 8 hunks, and
`context-pack` considers at most 8 ranked candidates — part of the citation
protocol rather than a tuning knob, see [7.14](#714-exact-retrieval).

---

## 7.12 One measure, one charge, one sealed event

Every response, refusals included, leaves through one function, which:

1. serializes the payload once, into the bytes the caller receives;
2. counts them with the real tokenizer;
3. performs the **only** budget charge in the crate, clamped so the counter
   cannot breach its ceiling;
4. computes `avoided = baseline − returned`, saturating, where a pull's
   baseline counts only files the caller stated, that the checkout still holds
   or, on the answers that carry changed lines, whose older revision the caller
   holds — each only where this family recorded reading the stated revision
   ([§8.6](08-cost-and-savings.md#86-what-avoided-actually-measures));
5. seals the audit event from those same two figures.

Handlers never touch the budget: every shaping decision upstream — dropping a
hunk, compiling a pack, downgrading a sensitive file, truncating a manifest — is
a *non-charging trial* against the remaining allowance. A structural test pins
exactly one charge site, and a property test over arbitrary call sequences
asserts that each response's emitted figure equals the measured count of its own
payload. Why so much machinery for a number? Because before it existed, every
audit row said `tokens_returned = 0` while claiming millions avoided — a value
proposition false on every row. Honesty is now structural rather than a matter
of each handler's discipline.

The decisions of [7.4](#74-tier-three-the-decision-at-the-read) and
[7.6](#76-documents-are-read-as-markdown) reach the same arithmetic by a longer
road, because nothing on a hook's gate path may open a store or start a
tokenizer. The gate journals what it decided; the detached worker counts the
declined slice with the real tokenizer, counts the frame it returned, and
writes both audit relations. Three properties survive the detour: a decision
that cannot be priced books **zero** avoided with a tag saying why rather than
an estimate, a substitution books what the sidecar actually cost as its
returned figure, and an injection still books spend with no credit.

Two consequences you can see from outside. **Provenance rides along**: each
answer reports its `tokenizer` — `cl100k_base` when the bundled encoder is live,
a conservative characters-over-four estimate when it is not — so a figure is
never presented as more exact than its basis. And **injection is spend with no
credit**: an injected manifest is audited under its own verb and the type has no
field for an avoided figure, so a caller cannot claim one, which is a query
rather than a guess because the pull and push sides once shared a verb whose rows
held nothing that told them apart. (Rows written before the split stay as they
are — the audit relations are append-only.)

[Chapter 8](08-cost-and-savings.md) turns these figures into a savings number,
under its own honesty ladder, including what `avoided`'s baseline is per verb.

---

## 7.13 Trust, policy, and the shape of a refusal

Content carries a trust class, and the class comes from **provenance** — where
the bytes came from in the ledger — never from the caller. Three classes may be
inlined raw: code facts (a file revision's content), git facts (a commit, a
diff) and test output (a runner's result). Two may not: tool output (whatever
some command printed) and transcripts (prompt and reply text).

An exact-retrieval target is classified by parsing its handle: `file:` or
`summary:` is a code fact, `log:` is untrusted tool output, and anything that
does not parse **fails closed to untrusted**. Untrusted content can leave only
as a neutral digest — counts and a handle, never the bytes — through the same
one-way serializer of [chapter 4](04-redaction-and-secrets.md).

`serve.toml` carries the repository's serving policy: denied path globs, the
raw-transcript allowlist, the injection budget. A decision about exact bytes
runs most-restrictive-first:

1. **fail-closed** — a policy file that exists but will not parse denies *all*
   exact retrieval and disables injection: an unreadable policy is precisely the
   state in which Rift does not know what you permitted;
2. **denied path** — never served, not even by handle;
3. **untrusted tool output** — never served as raw text;
4. **transcript** — off unless the path is explicitly allowlisted;
5. **reason required** — a missing reason is a refusal, and on a sensitive path
   one that says so specifically;
6. otherwise **allow**, returning the audit record the caller must emit.

Paths are lexically normalized before any glob is matched, so
`foo/../secrets/key` matches as `secrets/key`, and a path escaping the
repository root is treated as denied — and sensitive — rather than resolved.
Sensitive paths get one more rule, safety beating cost: if a sensitive file's
bytes will not fit the remaining budget the response *downgrades to a handle*
rather than truncating, decided on the inner body's size, so a secret is served
whole or not at all.

A refusal is a first-class response — measured, charged and audited like any
other — with a neutral tag from a closed vocabulary: `policy_denied_exact`,
`sensitive_path_requires_reason`, `budget_exhausted`,
`summary_invalid_source_changed`, `source_deleted_or_redacted`, `not_requested`.
It names *why*, never what was withheld.

---

## 7.14 Exact retrieval

Exact retrieval is where remembered bytes leave: two entrances, both narrowing
to one function.

**By handle.** The caller presents `file:<path>@sha256:<hash>` and a reason, and
before policy is consulted the address is checked against the record — **both
halves, or nothing**: a row must exist whose path *and* content hash match,
within the scope this caller may see. Not theoretical: a test measured the
handler serving a captured secret's bytes out of a `secrets/**` file under a
fabricated `src/a.rs` path, because it gated policy on the caller's path and
fetched the caller's hash. An unrecorded address gets the ordinary denial tag,
never a distinct "not in the record" one, which would answer the attacker's real
question — *which half was wrong* — and turn the refusal into an oracle.

**By citation.** An evidence pack hands the agent ordinals,
`pack:<key>/evidence:<n>`, not paths, and there is no stored table to look a key
up in: a server-side map keyed by a caller-supplied string is one an attacker
can drive by guessing. The table is *rebuilt* from Rift's own facts — the ranked
set as the store holds it now, the policy version in force, the budget the
request carried — the key recomputed, and the ordinal resolves only if the
recomputed key equals the presented one. A key nobody issued and a key whose
world has moved on get the same answer, both safe and honest: an ordinal means
something only relative to the pack that published it. If the caller also
restates a content address, the table is the authority; a disagreement is
refused, not reconciled.

The policy gate then runs on the path the ordinal *resolved to*, before any
bytes are read — the ordinal took the path from the agent without taking it from
the gate — and the payload names the ordinal, never the path. Refusal arms use
the same tag for the same reason, while the audit row records the key, the
ordinal, the resolved address and the verdict. Both routes converge on one
function that inlines bytes, routing the body through the trusted-payload type,
so a second inline cannot grow beside it and a class that may not be inlined
cannot be constructed there.

**What comes back today.** A permitted `retrieve-exact` answers `state: exact`
with an **empty body**: the blob reader wired into that channel is an in-memory
substrate with nothing in it, and `read-summary` answers `unavailable /
not_bound` until a binding exists, then the stored outline of the revision the
checkout holds (no shipped path writes one; see
[§6.15](06-recall.md#615-what-is-running-and-what-is-not)). Two answers do carry content. `changed-hunks` with a hash you hold
reads the basis blob out of the object store and returns the **changed lines
themselves**, each hunk windowed by three lines of context or grown to the
nearest blank or column-zero boundary within a cap — a deterministic,
parser-free window rule, never a claim about syntax — with unchanged runs
elided as a count, the window rule named in the payload, and code-fact
provenance only for tracked blobs — and, beside the lines, the `provenance`
object of [§7.9](#79-the-pull-surface-ten-verbs): the checkpoints that bracket
the change, who made it, what the calls its checkpoint froze cost, and `rift why`
for the rest.
`doc-read` returns the Markdown sidecar,
which is the position the read it replaces would have occupied. The rest of the
gates are real and exercised with the room behind them still empty
([chapter 18](18-known-gaps.md)).

Note what that object is **not**. It is not a second route to exact bytes: it
carries identifiers, four counts and a commit subject, and the one thing in it
that reaches further is a command the caller runs itself, under its own policy
gate. An agent that wants a prompt still has to ask for one, through
`retrieve-exact`, with a reason, and be allowed.

---

## 7.15 Grants: widening what a serve may see

By default a serve answers with what is visible on this checkout's line, and
says how much it held back; `--cross-branch <REASON>` is the only way it widens:

```
$ rift recall diff-since --cross-branch "reviewing the release branch" --json
```

The rules are small and firm. **Branches only**: widening the developer or
repository axis is a different act with a different blast radius, and this flag
is not it. **A reason is required**, at most 200 bytes, enforced at the command
line and again at the writer where the append-only row is made; a blank one is a
usage error rather than a quiet no-op. **Audited in two places**, because the
two values have two contracts: the serve's audit row carries a fixed neutral tag,
`cross-branch-grant`, in a column whose contract says it holds neutral tags, so "which serves
were widened" is a query, while your words go to a separate relation joined by the
audit id, **stored and never re-served**, so an auditor can read why without a
later serve quoting it back at an agent. And **the push tier can never grant** —
the injector builds its scope through the same shared function and never reaches
the line that applies one, guaranteed by the call graph rather than a check.

---

## 7.16 The audit trail

One serve produces exactly one terminal event — granted or refused — and that
bijection makes the trail readable. The kinds are `manifest_served`,
`manifest_injected`, `summary_served`, `exact_retrieved`, `pack_built`,
`invalidated`, `refused`. Two append-only relations hold them: the older
neutral-tag audit relation takes every event and is what the dashboard's savings
panel reads, while a successor adds a **channel** axis — pushed or pulled — plus
the verb name and the call's own id. A pulled serve writes both; an injection
writes the first only, under its `manifest_injected` verb. The decisions taken
at a read write both, through the detached worker, on a hook channel of their
own, so a declined read, a substituted document and a recovery map are each
queryable as what they are rather than inferred from a verb name. Old rows stay
readable; nothing is rewritten.

Two details that look like fussiness and are not. **Keys are per invocation**:
two `known-state` serves over unchanged state return byte-identical figures and
are still two serves, and keying on the payload would collapse them, understating
spend and inflating net savings — so an injected pointer, byte-identical every
session, is keyed by the session that paid for it, a per-turn manifest by a fresh
id. **Writing is best-effort**: a serve already made is never retracted because
its audit row could not be written, so an unavailable projection costs a row,
not a turn.

---

## 7.17 The red-team program

Serving remembered content without proof is an injection channel with a friendly
interface, so the guards above are held by an adversarial suite that runs in CI
and blocks a merge when it goes red. Two append-only corpora feed it: **21
hostile documents** — the shapes untrusted tool output and transcripts take when
trying to become instructions — and **50 hostile manifest entries**, short spans
in the position a remembered path or handle occupies on one line of the injected
frame (mid-span turn markers, homoglyphs, bidirectional overrides, a directive
buried past the render cap). Each row names the spans it must never leak. Every
document goes through the untrusted serializer and every row through the
push-side renderer, including the shape that beats a naive first-line check — an
innocent first line with the payload behind a newline — and each rendered
artifact is searched for those spans by literal containment.
A third corpus holds **11 hostile journal rows** in the position a coordinator's
read occupies in the section a subagent's window opens with: an absolute path,
a `..`, a control character, an instruction, a backslash spelling, and hashes
that are not hex. Each is rendered beside an honest row; none may be named, and
the honest row must still render.

Documents join the same corpora rather than getting a lane of their own: a
converted document is bytes a repository can contain, so word-processor,
spreadsheet and PDF fixtures carrying directive text assert that nothing
instruction-shaped reaches any pushed channel, and that the sidecar itself
arrives only in the tool-result position of the read it stands in for.

The suite is explicit about **which assertion is load-bearing**: asserting "no
manifest line looks like an instruction" over output the same predicate filtered
is circular, so that is only a tripwire, while the working oracle is independent
— the corpus refuses a row that does not contain what it forbids, so nothing
passes vacuously. Generative tests discover the pushed alphabet *from outside
the crate*, so a character added to the allowlist joins the tests at once, and
every hostile case asserts the honest half too: the innocent entry beside it
shipped, and the withholding was counted.

---

## 7.18 Command summary

Beyond §7.9's table: `rift recall search <query> [--content]` is also spelled
`rift search`; `rift agent-help [--json]` renders the surface from this
build; `known-state`, `diff-since` and `file-if-changed` accept repeated
`--known-hash <h>`; `cost-hotspots` accepts `--top-n <n>`; every pull verb takes
`--json`. A pull answer carries the payload, the two token figures and the
tokenizer that produced them, with an exit code separating *you asked wrong* (2)
from *the surface said no* (1).

`rift savings` prints what the decisions came to: the share of read tokens
avoided since the window boundary, overall and per family, with the honesty
figure beside it ([chapter 8](08-cost-and-savings.md)), and `rift recall
status` prints each lever's effective mode per family.

Turn per-turn injection off (or back on) by editing `injection_turn_budget` in
`.rift/policies/serve.toml`, or from the Project Settings surface in the
dashboard, which patches the same key with a conflict check
([chapter 11](11-canopy-tour.md)). The levers of
[7.4](#74-tier-three-the-decision-at-the-read),
[7.6](#76-documents-are-read-as-markdown) and
[7.7](#77-searches-are-bounded-and-sometimes-answered) have their own tables in
the same file, each with an `off` (nothing is consulted), an `observe`
(everything is decided and recorded, nothing acts) and an `enforce`
([chapter 2](02-configuration.md)).

---

## Related chapters

[Chapter 6 — Recall](06-recall.md) ·
[Chapter 8 — Cost and savings](08-cost-and-savings.md) ·
[Chapter 4 — Redaction and secrets](04-redaction-and-secrets.md) ·
[Chapter 10 — Agent integrations](10-agent-integrations.md) ·
[Chapter 17 — Invariants](17-invariants.md) ·
[Chapter 18 — Known gaps](18-known-gaps.md)
