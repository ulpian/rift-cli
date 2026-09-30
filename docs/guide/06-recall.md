# 6. Recall — the delta model

[Chapter 5](05-ledger-and-verification.md) left you with a ledger you can prove.
This chapter is what that ledger was built to make possible: an agent that asks
**"what changed since the state I already hold?"** instead of re-reading the
world.

Answering that *honestly* is the whole design. A memory system that says "this
file is unchanged" when the agent has never seen it, or hands back a summary
written against bytes that have since moved, is worse than no memory at all: it
makes the agent confidently wrong. One rule governs everything below:

> **"Unchanged" never means "you know it."** It means "the bytes you named are
> still the bytes on disk." Whether the agent *holds* those bytes is a separate
> fact, and the agent is the only one who can state it.

---

## 6.1 Try it first

Pick a file you have not touched:

```console
$ rift memory file-if-changed AGENTS.md
AGENTS.md: unchanged since last captured read (re-read avoidable)
```

Now ask as an *agent* must — `rift recall file-if-changed AGENTS.md --json` —
and it answers `changed`, with the new hash, `estimated_tokens_avoided` 1010
against 59 returned.

Same file, same second, two answers, both correct. The difference is who
supplies the "state you already hold". `rift memory` answers for **a human**:
you are in the checkout, so it hashes the working tree on your behalf. `rift recall` answers for **an agent**, whose context is not on disk —
Rift cannot see it and refuses to guess. An agent that names no hash holds
nothing, so everything reads as changed: the honest answer, not a bug. Pass
`--known-hash ee2f8418…c05600` and the same call answers `unchanged`.

Both spellings go through one dispatcher, one set of policy gates and one
budget — one surface, not two implementations.

---

## 6.2 Why deltas at all

An agent that re-reads a file it read twenty minutes ago pays full price for
bytes already in its context; over a working day most of a session's token spend
can be re-reads of unchanged material. That is the waste the ledger exposes and
the delta model recovers — but recovery has to be *safe*, and a naive cache gets
it wrong in three ways:

1. **False memory.** It reports "unchanged" and the agent infers "so I still
   know it" — but the agent never had those bytes, or its context was compacted
   away, so it reasons about a file it cannot see.
2. **Stale digests.** It hands back a summary written against an older revision,
   which reads as current and quietly describes code that no longer exists.
3. **Silent truncation.** It drops content to fit a budget and says nothing, so
   the agent believes it received a complete answer.

The manifest answers the first, the validity binding the second, the four-state
envelope the first and third together, and the sealed constructors make all three
unrepresentable rather than merely tested.

---

## 6.3 Manifest first: the anchor call

The first call of a turn is `known-state`. It retrieves nothing; it returns a
**manifest**: for each candidate artifact in scope, its current content hash and
the epistemic state it is in relative to what you said you hold.

```console
$ rift recall known-state --task "fix the failing budget test" \
    --file crates/rift-recall-policy/src/lib.rs --known-hash ee2f8418… --json
```

Two flags carry the weight. `--task` is **required** on every ranking-sensitive
verb: retrieval that does not know what you are trying to do returns cheap,
plausible, wrong context, so the surface refuses without a task string and at
least one file in focus (`--error` adds an error signature when one is in play).
`--known-hash` is repeatable and the *only* way an artifact becomes "known".

The envelope, elided:

```json
{ "verb": "known-state",
  "estimated_tokens_returned": 1185, "estimated_tokens_avoided": 1313056,
  "result": { "tool": "known_state",
    "envelopes": [ { "path": "AGENTS.md", "basis_sha256": "ee2f8418…",
                     "state": "known_unchanged" },
                   { "path": "crates/rift-store/src/schema/capture.rs",
                     "basis_sha256": "9c40b1d2…",
                     "state": "available_by_handle",
                     "provenance": { "checkpoints": { "to": "9c40…" },
                       "recorded_at_ms": 1757894400000,
                       "session": "01J8ZQ…", "agent": "claude-code",
                       "token_usage": { "uncached_input": 1200,
                         "cached_input": 30400, "cache_creation": 900,
                         "output": 640 },
                       "cost_micros": 41230,
                       "why": "rift why crates/rift-store/src/schema/capture.rs --json" } } ],
    "truncated": true, "omitted": 206,
    "containment": { "cross_branch_grant": false, "out_of_branch": 135,
                     "unknown_head": false } } }
```

Four properties of that shape matter more than its contents.

**It is manifest-first by construction.** Most entries carry no bytes: a
candidate's default state is "here is its address, ask if you want it", so the
agent decides what is worth context, having been told what moved.

**A changed entry says where it came from.** The `provenance` object is the
whole of what an agent needs to *rank* a change without fetching it: when it
happened, which session and agent family made it, what that turn cost, and the
exact command that tells the rest. It is digests and identifiers only, it is
absent rather than zeroed where nothing was recorded, and it downgrades to a
tag and that command before any entry is dropped —
[§7.9](07-serving-and-injection.md#79-the-pull-surface-ten-verbs) has the shape
and the reasoning. An entry you already hold has no provenance, because nothing
changed for it to be the story of.

**Truncation is announced.** `"truncated": true` with `"omitted": 206` answers
failure mode 3. Entries are in rank order, so the manifest keeps the
highest-ranked prefix and says how many it dropped; a budget that silently
shortened its own answer would be the same lie as a cache serving stale bytes.

**It anchors the turn.** Retrieval is bounded per turn — by default four calls
and 2,000 returned tokens — but a turn has no natural delimiter on a
request/response surface, so the manifest call is the delimiter: `known-state`
resets the draw-down while keeping the ceilings, and every later call in that
turn draws from the same allowance. A process-lifetime budget would be airtight
but starve a long session, and a sliding time window would be
non-deterministic; anchoring on the manifest matches how the surface is already
meant to be used, and fails closed:
a caller that never anchors runs under one un-reset budget rather than an
unbounded one. Refilling by calling the manifest again is deliberate: it costs a
round-trip and leaves an anchor event in the serve audit, so over-retrieval
becomes *visible*. Hard isolation is the policy gate's job.

> **Tip:** the budget lives in the caller, and each `rift recall …` invocation
> is its own short-lived process with a fresh one, so the call ceiling binds
> *within* a dispatch sequence, not across shell invocations; token accounting
> and the audit are per-call regardless.
> [Chapter 7](07-serving-and-injection.md) covers both channels' budgets and the
> 400-token injected-manifest ceiling.

---

## 6.4 The four states

Every served artifact is in exactly one of four states — not three, not a
free-form status string — each with different obligations.

| State | Bytes? | Charges budget? | Means |
|---|---|---|---|
| `known_unchanged` | no | no | the hash you named is still the current basis; bytes are **not** re-sent |
| `summary_included` | a digest | yes | a validity-checked digest inline, bound to `basis_sha256` |
| `exact_included` | the bytes | yes | exact content inline — policy-gated and audited |
| `available_by_handle` | no | no | nothing inline: the handle, and *why* it was omitted |

Every state carries a `handle` and, where one exists, a `basis_sha256`: what the
claim is about, plus the address to get it.

`available_by_handle` carries a `reason` from a closed vocabulary, most of what
the surface can refuse: `budget_exhausted` (it would have fit, but the allowance
ran out), `policy_denied_exact`, `sensitive_path_requires_reason` (a sensitive
path needs an explicit, audited reason), `summary_invalid_source_changed` (there
*was* a summary; its source moved), `source_deleted_or_redacted` (the named
bytes are gone), `not_requested` (the manifest-first default) — the difference
between "I will not", "it is gone" and "not this time", which an agent acts on
differently.

### What actually makes something `known_unchanged`

This is the false-memory guard, and the rule is narrower than it sounds. An
artifact is `known_unchanged` when **the agent named this exact hash** and **the
bound source is not known to have changed**; hash equality is itself the proof.
A bound summary strengthens the claim but is not required — a plain file the
agent holds is `known_unchanged` with no summary in the picture — and what is
*never* `known_unchanged` is an artifact whose bound source moved.

The state carries zero tokens, and that is the whole saving: the agent learns
"still what you have" for the cost of a path and a hash.

On the pull surface the agent is still the only one who can say what it holds,
and it says so with `--known-hash`. What changed is that Rift now keeps its own
record of the ranges a context window was handed, so the push side and the read
gate can answer the same question without being told
([§6.12](#612-what-a-context-window-already-holds)).

---

## 6.5 Binding a summary to the bytes it describes

The naive fix for failure mode 2 is a summary beside a "last updated" timestamp
plus invalidation logic that must be correct everywhere. Rift has none, because
a summary's **key is derived from the bytes it describes**:

```
summary_key = SHA-256(
    "rift.summary.v1\0"          // domain separation
  ‖ length(source_bytes)         // length prefix
  ‖ source_bytes
  ‖ extractor_version
  ‖ policy_version
  ‖ redaction_version )
```

Change one byte of the source, or bump the extractor, the summary policy or the
redaction rules, and the key changes. A stale summary is therefore not "invalid"
but **unlookupable**: nothing has to remember to delete it and nobody can
accidentally find it. The three version numbers are in the key because a summary
can go wrong three independent ways — the source moves, the digest code changes
what it produces, or the redaction rules change what a digest may contain.

Checking a binding against the current tree has three outcomes.
**Valid**: the path is present, its current hash equals the bound hash and the
digest bytes were found, so the digest is returnable. **Invalid**: the path is
present but hashes differently; the verdict carries the key and the *current*
hash, and no bytes come back. **Degraded**: the path is gone, or the digest
bytes could not be fetched — handle-only. Note the asymmetry: `Invalid`
discloses the new hash, `Degraded` nothing but the key, because a deleted or
redacted source has no current basis to disclose.

### Digests are mechanical, never written by a model

Four extractor kinds ship: failure, log, diff and file shape, each a pure
function — same bytes in, same bytes out, no clock, no I/O, no model — that
counts, scrubs and outlines, never paraphrases. The log extractor keeps
severity-marked lines, deduplicates them and prefixes counts; file shape emits
`412 lines, 13840 bytes, 27 top-level defs`; diff emits per-file add/remove
counts under a total. Failure signatures are *stabilized* first: line and column
positions become `:_`, hex addresses `0x_`, durations `(_s)`, so the same
logical error yields the same signature across runs.

Two load-bearing consequences. **Injected directives are neutralized**: an
extractor that only counts and outlines cannot be talked into repeating an
instruction in a log file (the trust chokepoint of
[Chapter 7](07-serving-and-injection.md) is the general defence; determinism is
why the digest path is not a second door). **The digest store is rebuildable**,
since the same source recomputes byte-identically.

---

## 6.6 One constructor per artifact

A guarantee re-implemented at the call site is not a guarantee but a comment.
That was a real finding: the validity engine's sealed types were property-tested
and correct, and the serving handlers reimplemented the hash comparison *inline*
beside them, so the surface emitted two of the four states and the plane's
strongest guarantee was sidesteppable in production.

The fix is structural, in three layers:

1. **Sealed constructors are the sole producers.** The four-state envelope, the
   validity verdict and the pack sections each have exactly one function that
   makes them; serving calls those, nothing builds a state inline.
2. **The seal is type-enforced.** `ValidSummary` — the only returnable summary
   value — has a crate-private constructor, its digest bytes are unreachable
   without one, and the classification function is its only mint. Constructing a
   "valid" summary outside the validity crate is a **compile error**.
3. **A structural test proves no serve path bypasses them.** A new inline
   builder does not merely look wrong; it fails a test.

Where a property must hold for every byte reaching a durable or served surface,
it is made a type with one door — the redaction gate of
[Chapter 4](04-redaction-and-secrets.md) is the same pattern. Packs get it from
the other direction: a compiled pack can be *written* but has no deserializer,
since a decoded pack would be section text that never passed a constructor.

---

## 6.7 Handles: addressing bytes, not paths

A handle is how the agent says "give me that": it resolves to an immutable
object, never a floating path.

```
file:<repo-relative-path>@sha256:<64 lowercase hex>[:L<lo>-L<hi>]
log:<tool_use_id>
summary:<key>
```

Three schemes, each addressing something that cannot change underneath you: a
file *revision*, an immutable tool result, a hash-bound summary key.

**Both halves of a file handle are the address.** The path alone is a name an
attacker can create, the hash alone does not say which file it was, so a
resolver must match both. (An earlier reading treated the path as display-only,
which let a consumer resolve by path and then write a fabricated content address
into an append-only record — permanently, verifiably wrong about itself.)

Parsing is strict, each rejection for a reason: the hash must be exactly 64 hex
characters; the path must be non-empty, not absolute, and free of any `..`
segment; the path must contain **no ASCII control character**, since a newline or
carriage return inside a handle would let a path forge extra lines, or
role-shaped directives, when rendered into a line-oriented manifest or pack; and
a line range must be 1-based and non-inverted.

Resolution self-validates. `file:` handles resolve **by content**, and the
resolver re-hashes what it got against the hash in the handle; a mismatch is
`Unavailable`, never bytes, so a hostile or traversing handle cannot read
arbitrary content even if a lookup returned something. `summary:` handles route
through the validity check, so a stale one yields `Unavailable` with the
`summary_invalid_source_changed` reason, never the old digest. The invariant
across all three: **a handle never resolves to "the current file at that
path".** If the named revision is gone, you get an honest absence.

---

## 6.8 Two hashes of the same bytes, and the bridge between them

This plumbing is why exact retrieval works at all. Rift addresses content by
`content_sha256`, a plain SHA-256 of the file bytes and the same function
capture records at read time; git addresses blobs by object id, a SHA-1 over a
*framed* payload:

```
blob_oid = SHA-1( "blob " ‖ len(bytes) ‖ "\0" ‖ bytes )
```

Two hashes, same bytes, never coinciding. A durable resolver looking up git
objects by `content_sha256` asks git for an id it has never heard of, and misses
every time, on every real repository.

The bridge is an **append-only mapping relation** keyed by content hash, with
blob id, path and a first-seen timestamp. Three properties:

- **Computed in the same pass.** The worktree hasher emits both identities for
  each file it reads; the second hash is effectively free next to the I/O
  already paid, and deriving it later would mean re-reading every file.
- **Content-addressed and idempotent.** Rows are inserted-or-ignored on the
  content hash, so re-ingest is a set-union no-op and the map grows
  monotonically; a hash already mapped keeps its first-seen path.
- **Honest on a miss.** A handle whose hash predates the map has no row and
  resolves to `Unavailable`, healed by the next ingest that sees those bytes.
  The same holds under git's SHA-256 object format, where the framed SHA-1 is
  simply not that repository's object id — the lookup misses rather than serving
  something else.

---

## 6.9 A pack is identified by its inputs

The context pack — the ordered, budgeted bundle of
[Chapter 7](07-serving-and-injection.md) — needs an identity, so identical
inputs re-serve byte-identically and a pack cache is a pure function of
immutable inputs.

Hashing the rendered pack is circular: the key would depend on the artifact it
identifies and could not be computed until after rendering. So identity is keyed
by **inputs**:

```
pack_key = SHA-256(
    <domain string for this pack kind>
  ‖ length-prefixed query
  ‖ count and length-prefixed (artifact_id, content_sha256) pairs, in rank order
  ‖ policy_version
  ‖ token_budget )
```

Four properties follow: it is **computable before rendering**, so it can key a
cache; **rank order is part of the identity**, because placement is part of the
pack's contract; it is **domain-separated per pack kind**, so the same inputs
compiled as two kinds cannot collide; and it is **length-prefixed**, so no two
input sets collide by concatenation ambiguity.

The rendered output is never the key. Any pack cache would be content-addressed
by the input key, throwaway and never authoritative. There is no pack cache
today — every section is composed fresh — so the response carries the key and
nothing about caching: a disclosure block reporting an always-empty cached list
reads as a live optimisation, and the day a cache is added it has to disclose
per-section provenance beside the key rather than inherit a field that was
always telling the same answer.

---

## 6.10 Where the delta model's state lives

Everything above operates over a **projection**: a local database, per
repository family, holding the derived facts.

```
~/.cache/rift/<family-id>/recall.db
```

Every row is derivable from git, and the append-only discipline of
[Chapter 13](13-storage-and-schema.md) applies in full: no in-place updates, no
deletes, corrections as new superseding rows, a mutation guard on every
canonical table. There are 27 such tables today, in three families, and the
split explains what is safe to throw away:

- **Derived graph facts** — a generic node/edge graph with each node kind's
  payload in its own typed relation: checkpoints, sessions, tool calls, file
  revisions, file reads, token usage, changed hunks. Rebuildable from git.
- **Validity artifacts** — summary bindings and the content-addressed digest
  blobs they point at. Rebuildable, because the extractors are deterministic.
- **Governance records** — the serve audit, serve-call and session events,
  outcome labels, metric snapshots, evaluation runs, key metadata. *Not*
  derivable from git: they record what was served, to whom, and what was
  measured, which is why the store exists at all rather than being in-memory.
  The serves themselves are no longer only here: a serve made inside a
  session is written with a `memd_audit_session` row naming that session and
  its window, and every checkpoint of the session freezes the serves those
  rows name, canonicalised, into its `serve_audit.jsonl`
  ([Chapter 5](05-ledger-and-verification.md)), where a rebuild cannot erase
  them, and a rebuild reads them back from there, attribution included
  ([§13.12](13-storage-and-schema.md)).
  A skill-CLI serve made from inside an agent session is attributed when the
  agent's tool result comes back: the answer prints its two keys on a
  `serve:` line, and the hook that ingests the result queues the row for the
  worker, which writes it only for a key its relation holds.
  One a person makes in a terminal belongs to no session and is frozen into
  no shard. The caller's own words in an exact retrieval's reason are frozen as
  the placeholder `withheld`, so a rebuilt exact-retrieval row carries
  `withheld` as its reason and the only copy of them stays in this store.

Node and edge kinds are `CHECK`-constrained to a declared mechanical set that is
deliberately *narrow*: semantic kinds — "similar to", "touches" — are absent, so
the database refuses them until a migration widens the set. Semantic recall is
not disabled by a flag; it is unrepresentable.

`rift-memd status` prints its location and node counts by kind: here,
ChangedHunk 764, Checkpoint 182, FileRead 1633, FileRevision 1079, Session 67,
ToolCall 1657. Ingest is incremental and idempotent: a watermark
records the last checkpoint ingested per branch, so re-ingest is a set-union
no-op. Maintenance verbs live on the projection binary:

| Verb | What it does |
|---|---|
| `metrics` | ingests the repository's checkpoints, then reports scoped metrics |
| `rebuild` | destroys and rebuilds the projection from git — a cold, idempotent ingest |
| `status` | projection health: node counts by kind, and its location |
| `doctor` | walks the repository, reporting checkpoints, sessions, shadow refs, parse issues |
| `eval` | runs the deterministic evaluation conditions and records the results |

`status` and `doctor` are read-only; `metrics` and `rebuild` write.

`rebuild` is safe by design: if the projection disagrees with git, git wins and
the projection is discarded.

---

## 6.11 Scope: the model never widens on uncertainty

Every recall query is bounded — by repository, branch ancestry, worktree,
developer and a checkpoint window. Containment is a security property, enforced
at the query layer and property-tested: no query shape may widen what a caller
can see ([Chapter 9](09-worktrees-and-families.md) has the mechanics). Two
things are worth carrying here.

**Cross-scope access is not a free choice.** Widening past the caller's branch
requires an audited grant carrying a reason — on the command line,
`--cross-branch <REASON>`. The manifest's `containment` block reports whether a
grant was in force, and its `out_of_branch` count is candidates **excluded**:
the response says how much it declined to show you, rather than quietly showing
a smaller world.

**An undecidable position excludes and reports.** If the caller's `HEAD` cannot
be determined the branch axis answers *unknown*, and unknown excludes, never
widens. That literal rule is why serving and accounting paths use different
constructors: a report computing a whole-family total says so explicitly, since
a total that silently dropped every member but one would be unauditable.

Ranking orders *within* a scope and never widens it: a transparent linear
combination of three signals in `[0,1]` — changed-file overlap, error overlap,
recency — with weights that redistribute when a signal is absent, plus a signed
multiplier from the candidate's outcome label. No model, no embeddings, an
injected clock so results are reproducible.

---

## 6.12 What a context window already holds

Everything above answers *is the state you named still current?* It cannot
answer the question that arises one instant before a read is issued: **does
this context window already hold these lines of this revision?** Nothing
recorded it. The capture read fact lands after the bytes are in context, the
recall relation lands after a condense, and the per-session manifest baseline
holds the hashes a manifest *announced* — none of the three carries a range,
and none is reachable inside a hook's budget without opening a store.

The absence has a price, measured on this repository's own captured history: of
its read tokens, 14.8 % re-read a revision the session had already read (98
reads, 500,428 tokens) and 25.6 % re-read a file that had changed since (58
reads, 866,705 tokens). The first is waste by construction; the second is the
verification read of [§6.13](#613-the-agent-holds-what-it-wrote).

So the read is recorded where it happens, in a small local file:

```text
.rift/state/reads/<key>.jsonl              the live file, appended to
.rift/state/reads/<key>.<n>.rolled.jsonl   a live file that filled up
.rift/state/reads/<key>.<n>.snapshot.json  the merged state of everything below n
```

**It is run state, never a record.** Rows carry paths, hashes, line numbers and
counts — never content. Nothing here is a canonical table, joined to one, or
synced, and losing the directory costs a re-read that would otherwise have been
declined. That is the failure direction the whole design leans toward, and it
is why this state is a file rather than a row: a decision taken while an agent
waits can afford a file read and cannot afford a store open.

**The key is one context window, not one session.** A coding assistant runs a
subagent under the *parent's* session id, and the two hold different bytes, so
the key hashes the session id together with the agent id the hook carries. A
family whose payload cannot tell a child from its parent still records; it
never declines, because a window it cannot separate is a window it cannot make
a claim about. The same key, from the same function, names the once-per-window
pointer marker and the per-turn manifest baseline
([§7.2](07-serving-and-injection.md#72-tier-one-the-session-pointer)), so what
a window is decided against and what it is told are one window and not two.

**Rows are validated on the way in, and a bad row never happened.** The
directory sits in a work tree an agent can write to, so every row is checked
against its shape — a version, a kind, 64 lowercase hex where a hash belongs —
and one that fails is dropped rather than interpreted. Eight kinds exist: a
read, a decline Rift issued, an observation of a decline it would have issued,
the hash a write was asked to produce, an edit that landed, an eviction
boundary, an **announcement** Rift pushed — a manifest or recovery-map entry
saying this window holds these lines of this revision — and the single
**booking** an announcement is worth once the window acts on the file without
reading it again ([chapter 8](08-cost-and-savings.md#86-what-avoided-actually-measures)).
The last two are not coverage and never become coverage: what a window was
*told* it holds is a different fact from what it was handed, and only the
second may decline a read. An announcement stands until something in the same
window shows the bytes reached it anyway — a read, a re-read the gate decided
against and let through, a decline, or the one booking it is worth — or until
the window is emptied, which ends everything before it.

### Coverage, and why it is deliberately pessimistic

**Coverage** of `(path, content hash)` is the merged union of the read
intervals recorded for that pair since the last eviction, plus the
edit-derived coverage of §6.13. Three properties make it safe to decide on:

- **A different revision has no coverage at all.** Intervals are scoped to the
  hash, never carried across one, so a changed file starts empty and no
  interval is ever shifted or adjusted to fit new bytes.
- **A range is recorded conservatively**, from pinned tool semantics only:
  never more than the tool can have delivered, and where a tool's default
  window is not pinned, a single line. A span is *lines*, *verified bytes*, or
  **unknown** — and an unknown span can never satisfy a decline, only fail to.
  A slice too large for a measured density to vouch for is the same rule taken
  to its end: a client that truncates at a *token* cap leaves nobody able to say
  in bytes where it stopped, so the call is recorded as nothing at all.
- **Everything that goes wrong subtracts.** An unreadable file, an unparseable
  snapshot, a row longer than one atomic append, a directory that cannot be
  created: each ends as *less* coverage than really exists, which costs a
  saving and cannot produce a decline of lines the window does not hold. The
  one direction that would be unsafe — keeping coverage while losing the line
  that says the window was emptied — is why an unreadable snapshot is read as
  an emptying rather than as nothing at all.

**The client's own statement about a delivery outranks that arithmetic.** A
density is a guess about bytes; a result that names the run it handed over is
the client answering for itself. Where a family's result object is pinned to
say so — the first line delivered, how many followed it, the file's own length,
and whether a token cap cut the call short — that run is what the window is
credited with, **whatever the slice weighs**. It is one rule read in two
directions: smaller than the record and the coverage is taken back to it,
larger and the difference is simply added, since coverage is a union and a
union needs no subtraction to grow. The clamp against Rift's own line count
stays on both, so a client that counts the empty stretch after a final newline
as a line costs a line of saving and never a line of coverage; a run sharing no
line with the call it answers leaves no claim standing at all; and a result
that says *nothing* about itself is silence, which stays silence. This is why
the largest files in a repository — the ones a single read can never vouch for
by size — are reachable at all. It also has a price: the statement arrives one
hook after the call, so nothing is credited at the pre-call boundary and two
identical reads issued in the same turn both run.

### Eviction keeps what it stops counting

A context window is emptied on the client's own boundaries: a compaction, a
clear, a resumed session. Each writes an eviction line, after which earlier
coverage no longer supports a decision — the bytes are genuinely gone, and
declining a read then would starve the agent rather than save it.

The pre-eviction lines are **kept**, because they are the one thing worth
saying at that moment: they are the input to the recovery map
([chapter 7](07-serving-and-injection.md)). The first re-read of each file
after a compaction is therefore legitimate and allowed; the *second* is
declined again, because the first re-recorded coverage. One read per file per
compaction is the floor of any design that never lies about what a context
holds. Compaction is not rare — measured here at about 0.85 per session with
reads — and the two levers reinforce each other: every token kept out of
context postpones the next compaction.

### Two processes, one window

A client may run its hooks in parallel and a model may issue parallel tool
calls, so two processes appending to one window's file is ordinary. Each row is
a single append of at most a few hundred bytes, atomic per call on the
platforms Rift runs on, so rows never interleave; a reader arriving mid-append
sees a final row without its newline and drops it. A file that reaches its line
cap is renamed aside and a snapshot of the merged state is written beside it,
so a long session folds into a smaller equivalent history without a reader ever
seeing a gap, and nothing is ever truncated — an append from a process still
holding the rolled file open is still read. `rift clean` collects old windows
and `rift doctor` counts them.

### Where the halves live

The *decision* is pure: the row schema, the interval set, coverage and the
predicate that reads them live in the agent SDK with no filesystem and no
clock, and are property-tested against arbitrary row sequences. The *files*
live in the `rift` binary beside the manifest baseline code. Pulling the two
apart is what lets the decision be tested exhaustively and the storage be
tested for concurrency, and it is why a per-family payload difference — which
field carries the path, which carries the range — is an adapter method with
pinned field names rather than a branch in the middle of the decision
([chapter 10](10-agent-integrations.md)).

---

## 6.13 The agent holds what it wrote

The largest measured pool is not a re-read of an unchanged file. It is the read
an agent makes of a file **it just edited**, to confirm the edit landed: 58
reads and 866,705 tokens here, about 15,000 tokens each, to check a change the
agent itself specified. The new bytes are in the agent's own tool call. The
information the verification read is looking for is already in context; the
read is habit.

Rift can say so only if it *knows* the agent holds the file, and knowing is an
equality it checks rather than an assumption about editors:

1. **The hash a write was asked to produce is recorded before the write runs.**
2. **After the edit, the file is hashed.** For a whole-file write, the file
   either hashes to what the agent supplied — so the agent holds the file, and
   coverage at the new hash is the whole file — or it does not, which is
   **formatter drift**: a save hook or formatter changed the bytes, the agent
   does *not* hold what is now on disk, and nothing is recorded.
3. **For a replacement edit**, the new text is located in the new bytes.
   Exactly one occurrence — or exactly the count a replace-all asked for —
   gives the edit site in the **new** numbering and the shift it caused, and
   full coverage carries forward only if coverage at the old hash was full.
   Anything ambiguous records no coverage and the read runs.

Multi-edits, notebook edits and patch application record no coverage in this
version: each can move several regions at once, and a site Rift cannot state
exactly is one it will not state at all.

Two consequences are worth naming. **Line shift is a non-problem for
correctness**, because coverage is scoped to a hash and a new hash starts
empty; the shift is used exactly, in new numbering, only where Rift names the
changed sites in a reason frame ([chapter 7](07-serving-and-injection.md)).
And **the echo is never the evidence**: no family was measured returning a
snippet Rift could rely on, so coverage comes from bytes Rift hashed itself.

## 6.14 Knowing where the files are

Everything above answers *is what I hold still true*. One question sits beside
it and is asked constantly: *where is the file called something like this*. An
agent asks it by running a glob or a listing, which walks the tree and hands
back an alphabetical page of paths — and Rift already knows more about which of
those paths matter here than a fresh walk can, because it has the record of
which files this family's sessions actually read.

So each member keeps a **path index**: every path this checkout may answer
with, each with the modification time it had, in one file under the member's
own run state. Three rules make it safe to trust without checking:

- **It is keyed by what would invalidate it** — the commit HEAD points at, and
  the newest modification time the walk saw. A key that does not match the
  checkout is not a stale index to be patched up; it is not an index.
- **A damaged file is not an index either.** A truncated or unreadable one
  reads as *absent*, and absent has a defined meaning everywhere: the agent's
  own tool runs, exactly as written.
- **Nothing walks a tree while somebody waits.** A walk is started only where
  it has been measured to fit inside the hook's budget, is abandoned as soon as
  it is clear it will not, and hands back absence plus a request that the
  detached worker rebuild it ([chapter 3](03-capture.md)). A partial index is
  never written, because a partial index is a wrong answer that looks like a
  right one.

**The ranking is Rift's own, over facts it already records.** How warm a path
is comes from window journals and the family's recorded per-file read counts;
how recent it is comes from the modification times in the index; whether it is
dirty comes from git. There is no second database and no per-machine store to
keep in step with anything — the score is a query over the record this chapter
is about.

How much of that a given answer can afford differs, and the difference is
stated rather than averaged. Asked as a command, the ranking gathers the window
journals of *every member of the family*, which is why a file a sibling worktree
read ranks warm in this one — the compounding property of
[chapter 9](09-worktrees-and-families.md) applied to names. Answered inside a
hook, it uses the two arguments a hook can afford: the matched paths, and this
conversation's own window journal, one bounded file the page cache is already
holding. It runs no git status there, because a full status of a working tree
is not a hook's to run and a frame that tagged nothing is honest where a frame
that guessed would not be.

The matcher underneath is a seam over pure-Rust pieces — fuzzy scoring, a query
grammar, content matching — chosen so nothing here needs a file watcher, a
resident process or a foreign engine. The index is a file the worker writes and
a search reads; a walk order cannot change an answer, which a property test
fixes by feeding one path set to the ranker in different orders and requiring
one result.

Two boundaries hold this to what it is. The index carries **names and
modification times, never content**, and it never carries a path the redaction
policy calls sensitive or a path git ignores — those are excluded by the walk
itself, so they cannot be ranked, named or matched later. And a *content*
search is a different question with a different answer, which is the subject of
[§7.7](07-serving-and-injection.md#77-searches-are-bounded-and-sometimes-answered).

## 6.15 What is running, and what is not

**Summaries are not bound by any shipped path.** The validity engine, digest
store, extractors, sealed types and the serving of a binding are built and
tested; the ingest step that would *write* bindings in production is missing.
The summary-binding tables are empty here and the manifest builder passes no
summary, so `summary_included` does not occur today — an artifact is
`known_unchanged` or `available_by_handle`, and `rift recall read-summary <path>`
answers `unavailable` with the reason `not_bound`: honest absence rather than a
guess dressed as a digest.

**Serving from git through an in-session index was designed and never built.**
No such crate exists in the workspace; §6.10's projection is what runs, and the
graph tables are live and written, not superseded — treat any description of an
in-memory recall index as a plan, not a component
([Chapter 18](18-known-gaps.md)). §6.14's path index is not that component and
does not stand in for it: it holds names, not content, and it answers *where is
this file*, never *what does it say*.

Every response also carries `estimated_tokens_avoided`: an estimate, and
specifically an upper bound, counting what a full read of the candidates would
have cost against what was returned. [Chapter 8](08-cost-and-savings.md)
explains the accounting seam behind it and the honesty ladder governing how
strong a claim it licenses; [Chapter 18](18-known-gaps.md) has the open items.

---

## Related chapters

[Chapter 5 — The ledger and verification](05-ledger-and-verification.md) ·
[Chapter 7 — Serving and injection](07-serving-and-injection.md) ·
[Chapter 8 — Cost and savings](08-cost-and-savings.md) ·
[Chapter 9 — Worktrees and families](09-worktrees-and-families.md) ·
[Chapter 13 — Storage and schema](13-storage-and-schema.md) ·
[Chapter 17 — Invariants](17-invariants.md) ·
[Chapter 18 — Known gaps](18-known-gaps.md)
