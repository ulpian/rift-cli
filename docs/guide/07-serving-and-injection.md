# 7. Serving and injection

Chapter [06](06-recall.md) explained what Rift *knows*. This chapter is how that
knowledge reaches an agent, and what stops it reaching one. **Push**: Rift
injects text without being asked, in three tiers — a one-time pointer, a
per-turn manifest of what moved, and, at the tool call, a decision about a read
the window already holds or a search it is about to run. **Pull**: the agent
runs `rift recall <verb> --json`, ten verbs in a closed set, and reads the
answer as data.

---

## 7.1 Two channels, one dispatcher

Both channels reach one dispatcher, which consults `.rift/policies/serve.toml`
and the per-turn budget and ends in one audit row: two serving paths would be two
places for a rule to be forgotten. The third tier enters through `PreToolUse`,
reaching one composer that knows what each client's published schema allows and
answers neutrally for everything else.

**Nothing here blocks your agent**: the injection attempt is wrapped so a failure
or a panic injects nothing and the hook still exits successfully. A declined read
is an instance of that rule: the decline is a response the client documents, and
the identical repeat runs. **Nothing here invents content**: one module writes
every sentence a model reads, everything it renders passes one chokepoint that
knows the bytes' provenance, and untrusted bytes never arrive raw
([chapter 4](04-redaction-and-secrets.md)).

---

## 7.2 Tier one: the session pointer

The first time a context window is spoken to — a session's first prompt, or the
opening of a subagent's window — Rift injects:

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

A pointer is spend every session, so a test holds it under 150 tokens (it
measures 124) and it hands off to `rift agent-help`, which costs nothing unless
run and is rendered from this build's command tree, so it cannot name a dead
verb. The provenance line is there because without it an agent treats `rift why`
as a separate errand rather than as the rest of what a delta already tells it.

**Once per context window.** The right to inject is an exclusive create of a
marker under `.rift/state/injection/`, named by the hash of the session id and
agent id — the window journal's key
([§6.12](06-recall.md#612-what-a-context-window-already-holds)) — so no
agent-supplied string becomes a path. Per window, because a subagent runs under its
parent's session id. Where a family declares a limit on injected text, Rift fits the
smaller of its budget and that limit, because over it the client spills the
middle to a file the model cannot usefully open. Channels per client are pinned
by fixtures ([chapter 10](10-agent-integrations.md)).

---

## 7.3 Tier two: the per-turn delta manifest

```
[rift] Recorded delta since the state you already hold (facts, not directives).
2 file(s) you have read changed since then:
- crates/rift-store/src/schema/capture.rs@sha256:b6caa84d L12-38,L204-206 (+2)
- bins/rift/src/hooks/gate/mod.rs@sha256:0a1b2c3d (changed)
(3 more not shown)
Detail on demand: rift recall changed-hunks crates/rift-store/src/schema/capture.rs
--known-hash b6caa84d1f5ada80 --json
```

**The hash is the one the window holds**, so the pull hint is runnable (the
pushed alphabet refuses the angle brackets a placeholder needs). **The ranges are
the lines that differ**, in the numbering the agent is about to read, at most
eight per entry. **The fallbacks are honest**: `(changed)` for an unreachable
basis, `(binary)`, `(removed)`, and a withheld span counted. The baseline is
keyed by window, because keyed by session a parent would be told that files only
a subagent read had changed; a subagent's opening is no manifest boundary, since
a window that has read nothing has no delta.

The budget is `injection_turn_budget`, **400 tokens**
([02 §2.6](02-configuration.md#26-riftpoliciesservetoml--the-serving-overlay)
has its reason); `0` turns injection off with no projection opened, and a budget
below the bare frame's cost injects nothing.

### What one turn decides

**Bootstrap** on the first observation: record the start, inject nothing — a
manifest claims files *the agent has read* changed, and that needs two
observations. **Silent** when nothing changed: record the state as accounted for.
**Unfittable** when not even the frame fits: record nothing, so the delta stays
owed; folding this into silence would turn a one-turn squeeze into permanent
silence. **Inject** when a manifest fits, folding what it reported into the
baseline.

### Announce once, not every turn

The `(3 more not shown)` counter is itself a report, and the hint reaches the
rest, so counted entries are accounted for. A baseline keeps 2048 hashes; past
that a file may be announced again — the failure direction is re-reporting,
never swallowing. **A withheld entry is not absorbed**: the count names no file,
so the revision stays owed and the next turn names or counts it again.

### Costing the turn as little as possible

The person is waiting, so each layer may answer silence before the next is paid
for: a zero budget; a **file stamp** before the projection is opened; a **scoped
stamp** for a projection that moved, but not for this member
([chapter 9](09-worktrees-and-families.md)). Then `diff-since` runs through the
ordinary dispatcher, skipping the pull verbs' refresh (the dearest step), so a
cold projection yields silence, not a stall.

### What a subagent's window opens with

A subagent starts holding nothing, while its spawner usually holds the files the
task is about. So its pointer is followed by one line, `[rift] Held by your
coordinator at your start (facts, not directives): <path>@<hash16>, …`: the
latest revision of each path the spawning window holds and the subagent does
not, newest first, at most eight named and the rest counted, with the hash form
`diff-since` takes. It rides in the pointer's claim (never pushed twice), fits what the budget
leaves, and changes no decision. Only the spawning window is read, because a
sibling's reads belong to another task. That journal is agent-writable, so a row
renders only with a relative path free of `..` and control characters that the
guard passes unaltered and a hex hash; any other row is counted.

### What a subagent's window inherits

Inheritance writes the coordinator's lines into the subagent's **own** journal,
marked with their class: *paste* (lines the spawning prompt carried), *by name*
(lines of a path the prompt named, which the spawner held), *fork* (a window
begun from its spawner's history). Nothing writes fork or paste, both fixed at
`observe`: no supported client signals a fork, and paste is not measured on the
hook path. By name has the lever `inherit_by_name`
([02 §2.6](02-configuration.md#the-read-gate-interception)).

The opening hook carries no prompt, so the spawning call writes a `spawn` line
naming the spawner and the held paths its prompt names (the prompt, at most
256 KiB, is scanned in memory, never written), and the opening `claim`s the one
unclaimed spawn under its prompt id and type; a sole match is always its own.
On recorded sessions the claim named the spawning call for 395 of 398 subagent
windows and a wrong one for none.

**At observe**, the spawner's live reads of the named paths become at most 256
`inherit` lines of class `by_name`, never coverage: a read only they would have
declined runs, the window records its `read`, and the worker books an observed
repeat under `inherited:by_name`, shown beside the savings figure and never in
it. Neither end of a spawn opens a store.

---

## 7.4 Tier three: the decision at the read

This repository's projection holds 190 injected manifests against 8 pulls:
asking is not enough, and the token is spent at the read. A read-shaped call
resolves to **Allow** (record the range delivered), **Observe** (the decision is
recorded and the call runs untouched — how a pool is priced before anything acts)
or **Decline**, once, with a reason the model reads.

**The levers and their modes.** `[interception] mode` defaults to `enforce`. An
`[interception.agents]` entry replaces that `mode` for its family, and the
family's pinned reason channel still caps the result (the per-client caps are in
[10 §10.7](10-agent-integrations.md#107-deterministic-context-delivery)).
`first_contact`, `decline_family_held` and `inherit_by_name` default to
`enforce`, and each never exceeds the family's resolved mode. The table is in
[02 §2.6](02-configuration.md#the-read-gate-interception); `rift recall status`
prints each lever's effective mode per family.

A decline needs every clause, and each closes something:

- the family resolves to `enforce` on a confirmed reason channel;
- the call is read-shaped **in the fields this family's pinned surface names**
  (a page range or a notebook cell is a field it does not name);
- the file is text and at most 1 MiB, so it is hashed inline and nothing waits on
  I/O;
- the slice is at most **96 KiB** and at least **28 lines**;
- the range is **wholly inside** coverage at the file's *current* hash
  ([§6.12](06-recall.md#612-what-a-context-window-already-holds));
- the window was not declined for that exact key since its last eviction.

Anything else allows — an unknown span, an unpinned family, an unreadable policy
(lever off), a panic in the gate — and the hook exits successfully.

**The 96 KiB ceiling** is the measured point at which a read result truncates in
place (25,000 tokens), so a decision never covers more than the context was going
to keep. **The 28-line floor** is the reason frame's break-even: the plain frame
costs **111 tokens** at about 4 tokens a line, so a decline pays from
111 / 4 + 1 = 28 lines. A test in the one crate holding both the sealed renderer
and the real tokenizer measures it, so a reword that grows the frame fails the
build. A slice under the floor is decided and recorded, never acted on.

**What the window holds is decided at the result.** A read is credited with what
the tool can have delivered; one too large to vouch for is credited with nothing
unless the client's result states what arrived, which is believed over the
arithmetic both ways ([§6.12](06-recall.md#612-what-a-context-window-already-holds)).
A decline never rests on a delivery that has not happened, so two identical reads
in one turn both run: a saving not taken, the direction every rule here leans.

### A read that is partly held

The narrowing applies when the window's own reads at the current hash — never
its edits, another window's lines, or lines from before an eviction — hold one
run at one end of the request and the rest is one run; held lines in the middle
or in two runs leave two windows to name, and the read is allowed. The build
holds it at `observe`: the call runs, the window gets a `read` line for what was
delivered (an `observe` line would leave it believing it holds less), and the
worker books an observed repeat under `already_held`, priced over the held run
only, since the rest is read either way. Its frame, `[rift] Read narrowed:`,
names the held run, its turn and the run still to read, and costs 67 tokens, so
its floor is 17 held lines. It carries no pull hint and no provenance handle,
because both are routes to a change and these lines did not change.

### Read-shaped includes the shell

One client reads through its shell far more than through its read tool, so the
shell counts, through a client's own published read-before-edit list (`cat`,
`nl`, `bat`, `head`, `tail`, `sed -n 'A,Bp'`, the `grep` family and `rg` on one
path, plus `ls`, `find … -name`, `rg --files`): matching a published list is not
parsing shell. A search is recognised with no coverage, since which lines it
printed is the file's to decide; a multi-run `sed -n` is credited with nothing,
since one output can pass the terminal's inline threshold while each run stays
under it. Three compositions are admitted because the delivered lines still
follow from the line: a leading `cd <dir> &&`, a trailing `| head -N`,
`| tail -N` or `| sed -n 'A,Bp'` on one read, and a `&&`/`;` chain of reads,
where an `echo` or `printf` is a label that opens nothing. Anything else takes
the **whole** line out. The recogniser is total, fuzzed, and capped at 4096
bytes and 16 reads.

**A call with several reads is one act**: declined only when every member would
be. One member not held lets the whole line run, because declining parts would
mean amending a command line Rift did not write.

### What a shell result showed the window

A command line the grammar cannot read is still a delivery, and its output says what it was.
At `PostToolUse` for a Claude Code `Bash` call whose result arrived inline, `[interception] recognise_by_output` cuts the line into members at `&&`, `||`, `;` and line breaks; a pipeline stays one member, because only what reaches the agent counts.
It finds the delivered lines in the files those members name, at the bytes on disk when the result arrived, after stripping the prefixes `grep -n`, `cat -n` and `nl` add.
A delivered line grounds coverage only where it cannot be a coincidence: a run of five distinct lines in order, a line found once among the files at hand, or a printed `path:N:` the file's own line confirms.
Each stretch of grounded lines is booked to one member, in order: the first that names its file, or the next one when that member has already shown those lines; a `cd` owns nothing, and output nobody owns books nothing.
What it books is a `matched` line: sparse runs of one revision and the member they came from, frozen into the checkpoint with the window's other rows.
At `observe`, the default, the lines only size the pool and no decision leans on them; at `enforce` a re-read of them is decided like a re-read of anything the window was handed.
The lever never sits above the read lever.
The step opens no store and no tokenizer: it hashes the named files as the read gate does, and reads their line hashes from the family's cache ([13](13-storage-and-schema.md)), which the detached worker writes, or computes them from the bytes in hand.

### One decline per key, which is the loop breaker

The identical repeat of a declined request is **allowed**, so a decline costs at
most one round trip and starving an agent is impossible. In scripted runs one
family never re-issued a declined read (eleven runs) and the other two always did
(ten each); the difference is the family, not the wording, so the retry rate is
published beside the savings and gates nothing.

### The reason frame

One sealed renderer writes every frame: fixed phrases in code, variable spans
through the allowlist of [§7.8](#78-what-may-appear-in-anything-rift-pushes):

```
[rift] Read skipped: docs/guide/06-recall.md@sha256:8099723c1f5a lines 1-40
are unchanged since your read in turn 1 and are already in your context.
Repeat this exact call if you still need it (it is allowed once). To see only
what changed since then: rift recall changed-hunks docs/guide/06-recall.md
--known-hash 8099723c1f5ada80 --json Provenance: rift why
docs/guide/06-recall.md:1 --json
```

It offers no summary (nobody pulls one), never claims an earlier result echoed
the content (no family was measured doing so reliably), and uses no error word.

The **delta frame** serves a file held at an *older* hash: where the difference
is known exactly — the agent's own edit
([§6.13](06-recall.md#613-the-agent-holds-what-it-wrote)) or a reachable basis —
it names the changed sites in current numbering and a window to read, and its
`rift why` names the first line that *moved*, because asked about an unmoved
context line it would answer for a different change. Without
exact sites the read runs: a delta frame without them is the plain frame with a
false claim attached. The basis is the git blob or, for a revision never
committed (the one a window most often holds, since agents read then edit), a
**read-time snapshot** written **through the redaction gate**
([chapter 4](04-redaction-and-secrets.md)). A mask is a difference Rift made:
diffed against, it would name a change that never happened and serve the live
bytes the gate removed. So a hunk touching a masked run is `(changed)`,
`changed-hunks` refuses its lines under `source_deleted_or_redacted`, and a mask
that moved line numbers is never snapshotted. With no basis the read proceeds:
losing a snapshot costs a delta frame, never an answer. A revision, to Rift, is
the SHA-256 of the bytes as the window read them, so an unchanged re-read is
declined whether git has committed those bytes, staged them or never seen them.

### A first read of a large file

The first sight of a file in a window is the read no other lever touches.
`[interception] first_contact` decides whether to answer it, once, with the
file's **bound outline**. A read qualifies when the window holds nothing of the
revision, it starts at line one (the whole file or a first page of at least 95
lines), the file is code or Markdown, and it has at least 400 lines or 16,384
bytes; a mid-file read names its lines, and no outline is cheaper. The outline
must be bound for these exact bytes, learned without a store: the worker that
counts a read writes its outline to a small file named by the bytes' hash under
the family's common directory ([13 §13.2](13-storage-and-schema.md#132-three-stores));
a miss records nothing. The frame, `[rift] Read outlined:`, names the costliest
twelve definitions and costs 379 tokens at that cap, so the 95-line page floor is
that cost at four tokens a line.

**At enforce**, the default, a qualifying read on a confirmed channel is
answered with the frame once, and the identical repeat runs. **At observe**,
where a repository writes it, a qualifying read with a bound outline
runs, the window records its `read`, and the worker writes a row under
`first_contact` with no frame and no claimed saving: the pool an outline would
answer.

### A read another window holds

A window holding nothing of a revision may ask for bytes another live window of
the family holds — a subagent re-reading its coordinator's file.
`[interception] decline_family_held` decides that read, only with a **cheaper
route**, never a bare refusal: a whole file or a first page of at least 95 lines,
nothing of the revision held, no family decision already standing on it. The
ground is what **live** windows of the family's checkouts hold at this revision
([9 §9.5](09-worktrees-and-families.md#95-what-is-shared-and-what-stays-put)),
never what they inherited: session working, journal written within the
idle-sweep floor, the newest sixteen journals and at most a mebibyte (about
19 ms).

The route: a bound outline gives the outline frame with the clause `is held at
this revision by another window of this repository`, one definition fewer so the
clause is paid for; else, if the others hold part of the request in at most
sixteen runs, `[rift] Read held by another window:` names them; if they hold all
of it, nothing is cheaper and the read runs. No frame names the holder.

**At enforce**, the default, the route is served once on a confirmed
channel. **At observe**, where a repository writes it, the read runs and
records its `read`; a `family`
line beside it records request, route, offered runs, frame size and
`acted: false`, and keeps the decision once per revision; the worker books a row
under `family_held:outline` or `family_held:held` with no claimed saving. An
acted decline admits every later read and books it under `family_held:followup`,
so its credit is the request less its frame less every read charged against it.

### Nothing on this path opens a store

The decision costs a hash under a ceiling and a few small run-state files —
journal, outline file, and for a family read the bounded sibling walk. No
projection is opened and **no tokenizer is initialised**, the two dearest things
a Rift process does; the worker counts slice and frame
([§7.12](#712-one-measure-one-charge-one-sealed-event)). A structural check fails
the build if either appears under a gate entry point.

---

## 7.5 After a compaction: the recovery map

A compaction is the one moment a full listing of held state pays for itself. On
a session start from a compaction or a resume, and after a client's own
compaction signal, Rift injects the *pre-eviction* held set under `[rift] Context
was compacted.`: each file's hash, the lines held, and a **verdict** (unchanged,
or changed at which lines), so the next read is skipped or narrowed rather than
repeated blind. It is **boundary-only** — on a quiet turn the listing is spend
for a call that does not happen — and **fitted** to `recovery_budget` (800
tokens), the rest counted. A cleared context gets no map: a clear is a person
emptying it on purpose, and handing it back would overrule them. The map does not
re-arm declining: pre-eviction coverage is evidence for what to say, never
grounds for a decision.

---

## 7.6 Documents are read as Markdown

A `.docx` read is a zip container, a spreadsheet arrives as markup, and on one
family a PDF arrives as raw bytes: full-price reads the model cannot use. So Rift
converts documents to Markdown once, through a pure-Rust engine (fourteen
extensions, no OCR, no network), into a sidecar **keyed by the document's
bytes**, the converter's version and the output caps, so any change keys
elsewhere and nothing needs invalidating; one per family
([chapter 9](09-worktrees-and-families.md)).

**Where a client documents an input amendment**, a document read's path is
amended to the sidecar, only in pinned additive forms verified against the
executed call. A page-range read is left alone, as is a PDF whose text layer is
under the per-page floor: a sidecar of nothing is worse than the original.
Conversion on the gate path gets 250 ms in a child process, so the deadline is
enforced by killing it and a hostile document dies in another address space;
past it the read runs and the worker converts for next time. **A dropped
amendment is caught**: when the executed input is not what Rift substituted (a
sibling hook won), the window flips to **redirect**, declining the next document
read once and naming the sidecar. A redirect books no avoided figure, because
nothing observes whether the model went there. Families with no amendment
channel get proactive conversion and a one-time redirect. The pull is
`rift recall doc-read <path>` (alias `rift doc read`).

On a generated corpus of one document per format, substitution removes
**90.94 %** of the native cost (107,784 tokens against 9,761). A PDF's native
cost is pages at a vision rate nobody has pinned, so its baseline is zero
([chapter 8](08-cost-and-savings.md)).

---

## 7.7 Searches are bounded and sometimes answered

**A content search that named no bound is given one.** No index of Rift's can
stand in for a search inside contents, so it is never answered or declined —
only bounded, where the client documents a result count. The tool states its true
total, and the follow-up runs **unbounded**, because the once-only key is the
search minus its paging parameters. A call that named its own bound, zero
included, is left alone; a displaced amendment stops that window amending,
never escalating to a decline.

**A name search is answered from the index Rift holds**
([§6.14](06-recall.md#614-knowing-where-the-files-are)): a glob or listing is
declined once and answered `[rift] Search answered: 8 of 143 paths matching the
pattern you searched for, ranked by relevance to this session`, ending `Repeat
this exact call for the full list.` It refers to the pattern rather than quote
it, since glob characters are outside the pushed alphabet. From the index or not
at all — cold, the call runs and the worker rebuilds; a hook never walks a tree.
Once per name search per window, on a confirmed reason channel; a repeat in the
answered turn is counted, since it saved nothing. Names and a count, never
contents; and it stands down when its frame would cost more than the listing.

Standing in for a question Rift cannot answer completely would send a model
hunting for an answer that is not there. The bounded
case is judged by the share asked again in the same turn, defined once in the
window's own record. Defaults and reasons:
[02 §2.6](02-configuration.md#search-search).

### What a search cost is measured, whatever Rift did about it

A search resolves to no file, so without a measure every share's denominator
would hold only reads. The post-tool hook journals what reproduces the call,
identifiers only, and the worker runs it again read-only and counts the output:
globs and literal searches in process, the rest through four allow-listed verbs
(`grep`, `rg`, `find`, `ls`) as argv with no shell. A composition needing a shell
is counted as unmeasured. The denominator only grows, so every published share
shrinks ([§8.6](08-cost-and-savings.md#86-what-avoided-actually-measures)).

---

## 7.8 What may appear in anything Rift pushes

Pushed text lands beside the person's own prompt, read with the same weight, so
every pushed entry, frame span and map line must survive:

- **An alphabet allowlist**: letters, digits, space and `. _ / @ : + -`.
  Enumerating escape syntaxes is unwinnable; naming what a path needs closes
  non-ASCII, template brackets, fences and backticks in one move.
- **Turn markers** (`system:`, `user:`, `assistant:` …) refused anywhere. A spelling is listed if and only if a model
  can read it as a turn boundary in a family Rift hooks, and a test fails when a
  family is added without deciding its dialect.
- **Imperative shapes** ("ignore …").
- **Frame mimicry**: a span starting like a Rift frame opener, after any leading
  digits, is refused.
- **120 characters**, applied *last*, so a directive past the cap is withheld
  rather than trimmed into innocence.

A refused span is **counted, never quoted** (`2 withheld as unsafe to inject`),
and a manifest of only withheld entries still renders, because silence would say
the state is current.

---

## 7.9 The pull surface: ten verbs

There is no transcript search: an unbounded search over remembered content is an
exfiltration channel with a friendly name.

| Verb | Answers | Requires |
|---|---|---|
| `known-state` | manifest first: what changed, what you hold, what to retrieve | `--task`, `--file` |
| `diff-since` | changed vs unchanged, given the hashes you hold | — |
| `file-if-changed` | is this file still what I last saw? | a path |
| `changed-hunks` | the changed lines against a held hash, or the ranked form | a path + `--known-hash`, else `--task`, `--file` |
| `read-summary` | a hash-bound summary, or `stale` | a path |
| `retrieve-exact` | exact bytes by handle, policy-gated | a handle, `--reason` |
| `cost-hotspots` | where the context tokens go | — |
| `context-pack` | one ordered, budgeted evidence pack | `--task`, `--file` |
| `doc-read` | a document as Markdown | a path |
| `search` | ranked paths for a query, or with `--content` matching lines | a query |

The ranking verbs refuse without `--task` and a `--file` (exit 2): ranking
without a stated goal is guesswork dressed as an answer. `--known-hash` takes a
prefix of at least sixteen hex characters; zero or several matches read as
*unknown*, answered as changed. Every verb accepts `--cross-branch <REASON>`
([§7.15](#715-grants-widening-what-a-serve-may-see)); the ranked and delta verbs
`--token-budget`. With Recall off every verb exits 4 rather than creating a
projection. `search` (also `rift search`) never names a path the policy denies
but counts it in its total, and `--content` declares **no baseline**, booking
zero avoided, since nobody has pinned what a native content search costs. The
decoder is total and fuzzed. `rift memory …` asks the same dispatcher for
people, hashing your worktree for the known state.

### Provenance rides with the delta

`known-state`, `diff-since` and `changed-hunks` attach a `provenance` object to
each changed file: `checkpoints` (`from`, `to`), `recorded_at_ms`, `session`,
`agent`, `token_usage`, `cost_micros`, `commit` (oid and subject) and `why` (the
`rift why` command). **Identifiers, never prose**: nothing in it was written by a
model, and a missing key means *not known*, never zero. **Every figure names its
denominator**: the tokens and cost are the calls the bracket's later checkpoint
froze, never a session total that a reader would take for the change's cost.
**No join**: the story comes from the capture projection through a seam only the
pull process fills, because `recall.db` never joins a capture table
([chapter 13](13-storage-and-schema.md)). **It is sacrificed first**: over budget
it becomes `"provenance": "available_by_handle"` plus `why` before any delta is
dropped — a hidden delta is a re-read, a hidden story one command away.

---

## 7.10 The per-turn budget

A turn carries **4 tool calls** and **2000 returned tokens**: an agent that
keeps calling for something useful re-reads the world by a longer road. A `known-state` call **anchors** the turn, zeroing the draw-down
([§6.3](06-recall.md#63-manifest-first-the-anchor-call)); exhausting the calls
refuses with `budget_calls_exhausted` and that hint. The counter saturates. The
budget is held by the caller and each `rift recall` is its own process, so the
ceilings bind within one call, not across calls
([18](18-what-rift-does-not-do.md)).

---

## 7.11 Caps and truncation

List answers are capped at **1200 tokens**, a `--token-budget` clamped to
**4000**, and never past what the turn has left: a flat 4000 against a
2000-token turn would let one anchoring manifest fail every follow-up.
Truncation keeps the largest fitting prefix, found by binary search plus a
walk-down because token counts are not monotone under concatenation, and reports `truncated: true` with an
`omitted` count; `known-state` keeps its best-ranked entries, `diff-since` drops
unchanged ones first. If nothing fits, the anchor still ships an empty skeleton
rather than strand the agent. `changed-hunks` returns at most 8 hunks and
`context-pack` considers at most 8 candidates.

---

## 7.12 One measure, one charge, one sealed event

Every response, refusals included, leaves through one function: serialize once,
count with the real tokenizer, make the **only** budget charge, compute
`avoided = baseline − returned` (a pull's baseline counts only files the caller
stated and this family recorded reading,
[§8.6](08-cost-and-savings.md#86-what-avoided-actually-measures)), and seal the
audit event from those figures. A structural test pins the one charge site and
a property test asserts each emitted figure equals its payload's count.

Gate decisions ([§7.4](#74-tier-three-the-decision-at-the-read),
[§7.6](#76-documents-are-read-as-markdown)) reach the same arithmetic through the
detached worker. An unpriceable decision books **zero** with a tag saying why; a
substitution books the sidecar's cost as returned; an injection books spend with
no avoided field at all. Each answer names its `tokenizer` (`cl100k_base`, or a
characters-over-four estimate), so no figure looks more exact than its basis.

---

## 7.13 Trust, policy, and the shape of a refusal

Trust comes from **provenance**, never the caller. Code facts, git facts and test
output may be inlined raw; tool output and transcripts leave only as a neutral
digest ([chapter 4](04-redaction-and-secrets.md)). A handle is classified by
parsing it, and anything unparseable **fails closed to untrusted**.

Exact bytes are decided most-restrictive-first: a `serve.toml` that does not
parse denies all exact retrieval and disables injection, because Rift then does
not know what you permitted; a denied path is never served, even by handle;
untrusted output never raw; a transcript only if allowlisted; a missing reason
refuses. Paths are normalized before matching, and one escaping the repository
is denied. A sensitive file over budget *downgrades to a handle*: a secret is
served whole or not at all. A refusal is audited like any answer, with a tag
from a closed set (`policy_denied_exact`, `budget_exhausted`,
`source_deleted_or_redacted`, …) naming *why*, never what.

---

## 7.14 Exact retrieval

**By handle**, `file:<path>@sha256:<hash>` with a reason, a recorded row must
match **both** path and hash in the caller's scope before policy runs: gating the
caller's path while fetching the caller's hash would let a fabricated `src/a.rs`
serve a `secrets/**` file. An unrecorded address gets the ordinary denial, so the
refusal is no oracle. **By citation**, `pack:<key>/evidence:<n>`, the key is
recomputed from Rift's own facts rather than looked up, since a map keyed by a
caller string can be driven by guessing; the payload names only the ordinal, and
policy runs on the resolved path.

A permitted `retrieve-exact` returns `state: exact` with what the git object store
holds under the handle's address; a `file:` address is a content SHA-256, which
names no git object, so that body is empty. `read-summary` answers the bound
outline, `stale` or `unavailable` ([§6.15](06-recall.md#615-what-is-running-and-what-is-not)).
`changed-hunks` returns **the changed lines**, each hunk with three lines of
context or grown to a blank or column-zero boundary within a cap.

---

## 7.15 Grants: widening what a serve may see

`--cross-branch <REASON>` is the only way a serve sees past this checkout's line. **Branches only**:
another axis is a different blast radius. **A reason is required**, at most 200
bytes, checked at the command line and at the writer. **Audited twice**: the serve
row carries the neutral tag `cross-branch-grant`, while your words go to a
separate relation, **stored and never re-served**. **The push tier can never
grant**: the injector never reaches the line that applies one, by call graph.

---

## 7.16 The audit trail

One serve, one terminal event: `manifest_served`, `manifest_injected`,
`summary_served`, `exact_retrieved`, `pack_built`, `invalidated`, `refused`. A
neutral-tag relation feeds the savings panel; a second adds a pushed/pulled
channel, the verb and the call id. **Keys are per invocation**, because two identical
serves are two serves and keying on the payload would understate spend; so an
injected pointer, byte-identical every session, is keyed by the session that
paid for it.
**Writing is best-effort**: no serve is retracted for a failed row.

---

## 7.17 The red-team program

A CI suite blocks a merge on any leak. Append-only corpora of hostile documents,
manifest entries, frame spans and coordinator rows each name the spans they must
never leak, and every rendered artifact is searched for them literally. A check
that "no line looks like an instruction" over output the same predicate filtered
is circular, a tripwire only; the literal search is the oracle, and a row that
does not contain what it forbids is refused, so nothing passes vacuously. Each
hostile case also asserts that the innocent entry shipped.

---

## 7.18 Command summary

Beyond §7.9's table: `rift agent-help [--json]` renders the surface. Exit 2 is
*you asked wrong*, 1 *the surface said no*. `rift savings` prints the avoided share overall and per
family with its honesty figure ([chapter 8](08-cost-and-savings.md)); `rift
recall status` prints each lever's effective mode. `injection_turn_budget` and
the lever tables of §7.4, §7.6 and §7.7 live in `.rift/policies/serve.toml`, each
lever `off` (not consulted), `observe` (decided and recorded, nothing acts) or
`enforce` ([02 §2.6](02-configuration.md#26-riftpoliciesservetoml--the-serving-overlay));
Project Settings patches the same keys ([chapter 11](11-canopy-tour.md)).

---

## Related chapters

[Chapter 6 — Recall](06-recall.md) ·
[Chapter 8 — Cost and savings](08-cost-and-savings.md) ·
[Chapter 4 — Redaction and secrets](04-redaction-and-secrets.md) ·
[Chapter 10 — Agent integrations](10-agent-integrations.md) ·
[Chapter 17 — Invariants](17-invariants.md) ·
[Chapter 18 — What Rift does not do](18-what-rift-does-not-do.md)
