# 6. Recall — the delta model

The ledger of [Chapter 5](05-ledger-and-verification.md) lets an agent ask
**"what changed since the state I already hold?"** instead of re-reading the
world. Answering that *honestly* is the whole design. A memory that says "unchanged"
about a file the agent never saw, or returns a summary written against bytes that
have since moved, is worse than none: it makes the agent confidently wrong.

> **"Unchanged" never means "you know it."** It means "the bytes you named are
> still the bytes on disk." Whether the agent *holds* them is a separate fact,
> and only the agent can state it.

---

## 6.1 Try it first

```console
$ rift memory file-if-changed AGENTS.md
AGENTS.md: unchanged since last captured read (re-read avoidable)
```

`rift recall file-if-changed AGENTS.md --json` answers `changed` with
`estimated_tokens_avoided` 0. Both are correct: `rift memory` answers for **a
human** and hashes the working tree; `rift recall` answers for **an agent**,
whose context is not on disk, so one naming no hash holds nothing. With `--known-hash ee2f8418…c05600` it
answers `unchanged`, credited because this family recorded reading those bytes.
Both spellings share one dispatcher, gate set and budget.

---

## 6.2 Why deltas at all

Most of a session's token spend can be re-reads of material already in context.
Recovering it must be *safe*; a naive cache fails three ways: **false memory**
("unchanged" taken as "I still know it", though the agent never had the bytes or
was compacted), **stale digests** (a summary of an older revision reads as
current) and **silent truncation** (content dropped to fit a budget, answer
presented as complete). The manifest answers the first, the validity binding the
second, the four-state envelope the first and third, and sealed constructors make
all three unrepresentable rather than merely tested.

---

## 6.3 Manifest first: the anchor call

The first call of a turn is `known-state`. It retrieves nothing; it returns a
**manifest**: each candidate artifact in scope, its current hash, and its state
relative to what you hold (`rift recall known-state --task "fix the failing budget
test" --file crates/rift-recall-policy/src/lib.rs --known-hash ee2f8418… --json`).
`--task` is **required** on every ranking-sensitive verb, with at least one
`--file` (`--error` adds a signature): retrieval that does not know the goal
returns cheap, plausible, wrong context. `--known-hash` is repeatable and the
*only* way an artifact becomes "known". Each envelope carries `path`,
`basis_sha256`, `state` and, for a change, `provenance`; the response carries
token estimates, `truncated`, `omitted` and a `containment` block.

- **Manifest-first.** Most entries carry no bytes, so the agent decides what is
  worth context.
- **A changed entry says where it came from.** `provenance` (when, session, agent
  family, turn cost, the `rift why` command) lets an agent *rank* a change without
  fetching it; digests and identifiers only, absent rather than zeroed, dropped to
  a tag before any entry is
  ([§7.9](07-serving-and-injection.md#79-the-pull-surface-ten-verbs)).
- **Truncation is announced.** Entries are in rank order; the manifest keeps the
  highest-ranked prefix and says how many it dropped. A budget that silently
  shortened its answer would be the same lie as a stale cache.
- **It anchors the turn.** Retrieval is bounded per turn — by default four calls
  and 2,000 returned tokens — and a request/response surface has no turn
  delimiter, so the manifest call is the delimiter: it resets the draw-down and
  keeps the ceilings. A process-lifetime budget would starve a long session; a
  time window is non-deterministic. Refilling leaves an anchor event in the serve
  audit, so over-retrieval is *visible*; hard isolation is the policy gate's job.
  Each `rift recall …` process has a fresh budget, so the ceiling binds within
  one dispatch sequence, not across shell invocations
  ([Chapter 7](07-serving-and-injection.md)).

---

## 6.4 The four states

| State | Bytes? | Charges budget? | Means |
|---|---|---|---|
| `known_unchanged` | no | no | the hash you named is still the current basis |
| `summary_included` | a digest | yes | a validity-checked digest inline, bound to `basis_sha256` |
| `exact_included` | the bytes | yes | exact content — policy-gated and audited |
| `available_by_handle` | no | no | nothing inline: the handle, and *why* |

Every state carries a `handle` and, where one exists, a `basis_sha256`.
`available_by_handle` carries a `reason` from a closed vocabulary — `budget_exhausted`,
`policy_denied_exact`, `sensitive_path_requires_reason`,
`summary_invalid_source_changed`, `source_deleted_or_redacted`, `not_requested`
(the manifest-first default) — the difference between "I will not", "it is gone"
and "not this time", which an agent acts on differently.

### What makes something `known_unchanged`

This is the false-memory guard. An artifact is `known_unchanged` when **the agent
named this exact hash** and **the bound source is not known to have changed**;
hash equality is itself the proof, a bound summary strengthens but is not
required, and an artifact whose bound source moved is never `known_unchanged`.
The state carries zero tokens, and that is the saving.

"Not known to have changed" is a question about the file on disk, not the
projection: a projection's newest revision trails every edit since, and answering
from it would tell an agent holding the old bytes that they are current. So every
answer that says a file is current — `file-if-changed`, `known-state`,
`diff-since`, `context-pack`, `read-summary` — reads the checkout and compares
hashes.

- A file the checkout does not hold at the stated hash is `changed`, with the
  hash it has now.
- A file that cannot be read (no checkout, nothing at the path, a symbolic link, a
  path leaving the checkout, past the size ceiling) is `unavailable`; a path no
  read recorded is `invalid` (`path_not_found`). The agent reads it itself.
- An answer without the checkout's bytes is the projection's view and credits
  nothing; the push tier's delta list is one, and says `unverified`.
- A hash-bound `retrieve-exact` of a revision the checkout no longer holds is
  still served — history asked for by address — with `"historical": true`.

The figure beside an answer follows the same line
([§8.6](08-cost-and-savings.md#86-what-avoided-actually-measures)): list answers
credit only files the agent still holds; `changed-hunks` and `context-pack` carry
the changed lines, so they credit a changed file's cost less the lines returned.

A `known_unchanged` entry may carry `last_seen_turn`, the turn the calling window
last read those bytes ([§6.12](#612-what-a-context-window-already-holds)) — a
citation, never a condition, present only where the pull can tell which window
asks (a Claude Code subagent looks like its parent, so neither cites).

---

## 6.5 Binding a summary to the bytes it describes

Rather than a summary beside a timestamp plus invalidation logic that must be
right everywhere, a summary's **key is derived from the bytes it describes**:

```
summary_key = SHA-256(
    "rift.summary.v1\0" ‖ length(source_bytes) ‖ source_bytes
  ‖ extractor_version ‖ policy_version ‖ redaction_version )
```

Change a byte, or bump the extractor, the summary policy or the redaction rules —
the three independent ways a summary goes wrong — and the key changes. A stale
summary is not "invalid" but **unlookupable**: nothing must remember to delete it
and nobody can find it by accident.

Checking a binding has three outcomes. **Valid**: path present, current hash equals
the bound hash, digest found — returnable. **Invalid**: hashed differently; the
verdict carries the key and the *current* hash, no bytes. **Degraded**: path gone
or digest unfetchable — handle-only, only the key disclosed, since a deleted or
redacted source has no current basis to disclose.

### Digests are mechanical, never written by a model

Four extractors ship — failure, log, diff and file shape — each a pure function
(no clock, no I/O, no model) that counts, scrubs and outlines, never paraphrases.
File shape emits an outline by extension (code or Markdown); for code, a header and one line per
column-zero definition from a fixed keyword table (`fn summary_key L36-L52`), names
keeping identifier characters only so no comment or string text reaches them, with
each definition's raw token count stored beside it (`summary_token_count`) so a
surface can state its cost without a tokenizer. Failure signatures are
*stabilized* (`:_`, `0x_`, `(_s)`) so one logical error yields one signature.

So **injected directives are neutralized** (an extractor that only counts and
outlines cannot repeat an instruction found in a log; [Chapter 7](07-serving-and-injection.md)
has the general defence), and **the digest store is rebuildable**, since the same
source recomputes byte-identically.

---

## 6.6 One constructor per artifact

A guarantee re-implemented at the call site is a comment, not a guarantee: a
handler redoing the hash comparison inline sidesteps the sealed types beside it.
So it is structural:

1. **Sealed constructors are the sole producers.** The four-state envelope
   (`envelope_for`, the single builder), the validity verdict and the pack
   sections each have one function that makes them.
2. **The seal is type-enforced.** `ValidSummary`, the only returnable summary, has
   a crate-private constructor and one mint, the classification function.
   Constructing a "valid" summary outside `rift-recall-validity` is a **compile
   error**.
3. **A structural test proves no serve path bypasses them.**

A compiled pack can be *written* but has no deserializer: a decoded pack would be
section text that never passed a constructor. The redaction gate of
[Chapter 4](04-redaction-and-secrets.md) is the same pattern.

---

## 6.7 Handles: addressing bytes, not paths

```
file:<repo-relative-path>@sha256:<64 lowercase hex>[:L<lo>-L<hi>]
log:<tool_use_id>
summary:<key>
```

Each scheme addresses something that cannot change underneath you: a file
*revision*, an immutable tool result, a hash-bound summary.

**Both halves of a file handle are the address.** The path alone is a name an
attacker can create; the hash alone does not say which file it was. Treating the
path as display-only would let a consumer write a fabricated content address into
an append-only record — permanently wrong about itself.

Parsing is strict: the hash exactly 64 hex characters; the path non-empty, not
absolute, no `..`, and **no ASCII control character** (a newline could forge lines
or role-shaped directives in a line-oriented manifest); the range 1-based and
non-inverted.

Resolution self-validates. `file:` handles resolve **by content** and the
resolver re-hashes the result against the handle; a mismatch is `Unavailable`,
never bytes, so a hostile handle cannot read arbitrary content. `summary:`
handles route through the validity check, so a stale one yields `Unavailable`
with `summary_invalid_source_changed`. **A handle never resolves to "the current
file at that path"**: a gone revision is an honest absence.

---

## 6.8 Two hashes of the same bytes, and the bridge between them

Rift addresses content by `content_sha256`, plain SHA-256 of the bytes (the same
function capture records at read time); git addresses blobs by a SHA-1 over a
*framed* payload, `SHA-1("blob " ‖ len ‖ "\0" ‖ bytes)`. The two never coincide,
so a resolver asking git for a `content_sha256` misses every time. The bridge is
an **append-only mapping** from content hash to blob id, path and first-seen time:

- **Computed in the same pass** the worktree hasher reads each file; deriving the
  second later would mean re-reading everything.
- **Idempotent**: inserted-or-ignored on the content hash, so re-ingest is a
  set-union no-op.
- **Honest on a miss**: a hash predating the map resolves to `Unavailable`, healed
  by the next ingest that sees the bytes. Under git's SHA-256 object format the
  lookup misses rather than serving something else.

---

## 6.9 A pack is identified by its inputs

Hashing the rendered pack is circular — the key would depend on the artifact it
identifies. So the identity of a context pack ([Chapter 7](07-serving-and-injection.md))
is keyed by **inputs**:

```
pack_key = SHA-256( <domain string for the pack kind> ‖ length-prefixed query
  ‖ count and length-prefixed (artifact_id, content_sha256) pairs, in rank order
  ‖ policy_version ‖ token_budget )
```

It is computable before rendering; **rank order is part of the identity**, since
placement is part of the pack's contract; it is **domain-separated per kind**
(context, review, investigation) and **length-prefixed**, so inputs cannot collide
by concatenation. Identical inputs re-serve byte-identically; every section is
composed fresh, so the response carries the key and nothing about caching.

---

## 6.10 Where the delta model's state lives

A **projection** per repository family holds the derived facts:

```
~/.cache/rift/<family-id>/recall.db
```

The append-only discipline of [Chapter 13](13-storage-and-schema.md) applies in
full — no updates or deletes, corrections as superseding rows, a mutation guard on
each of its 34 canonical tables. Three families of table explain what is safe to
throw away:

- **Derived graph facts** — a node/edge graph with each node kind's payload in its
  own relation: checkpoints, sessions, tool calls, file revisions, file reads,
  token usage, changed hunks, and a checkpoint's `window_journal.jsonl` as
  `n_window_row`, one per line, keyed by the SHA-256 of the line so rows every
  later checkpoint carries again land once. Rebuildable from git.
- **Validity artifacts** — summary bindings, their digest blobs (keyed by hash)
  and the token counts beside each outline. Rebuildable: extractors are
  deterministic.
- **Governance records** — serve audit, serve-call and session events, outcome
  labels, metric snapshots, evaluation runs, key metadata. *Not* derivable from
  git: they record what was served, to whom, and what was measured. Every
  checkpoint freezes the serves its session's `memd_audit_session` rows name into
  its `serve_audit.jsonl` ([Chapter 5](05-ledger-and-verification.md)), where a
  rebuild cannot erase them ([§13.12](13-storage-and-schema.md)). A skill-CLI
  serve is attributed when the agent's tool result returns (the answer prints its
  keys on a `serve:` line); a terminal serve belongs to no session. The caller's
  words in an exact retrieval's reason are frozen as `withheld`; the only copy
  stays here.

Ingest is incremental and idempotent (a watermark per branch) and runs before
every pull verb, and after every worker pass that writes a checkpoint
([§3.10](03-capture.md#310-the-detached-worker)), so a family worked on with no
dashboard open never serves from an ever older checkpoint. A watermark stamps `ingested_at` from the injected clock (the
system clock in production, a fixed one in tests and rebuilds), and `0` reads as
*unknown*, never an instant. A history walk records a proof per revision git does not hold, so
a fully proven pass does not walk again, and a new read or moved `HEAD` has no
proof; one pass walks at most 64 paths, unattempted first, so bytes git never
holds cannot starve the rest.

The projection binary `rift-memd` has `metrics` (ingest, then report; `--full` for
the full pass), `rebuild` (destroy and rebuild from git — git wins on any
disagreement), `status` and `doctor` (read-only) and `eval` (the deterministic
evaluation conditions).

---

## 6.11 Scope: the model never widens on uncertainty

Every query is bounded by repository, branch ancestry, worktree, developer and a
checkpoint window, a property-tested guarantee ([Chapter 9](09-worktrees-and-families.md)).

**Cross-scope access is audited.** Widening past the caller's branch needs a grant
with a reason (`--cross-branch <REASON>`); `containment` reports whether one was in
force and `out_of_branch` counts candidates **excluded**, so the response says what
it declined to show.

**An undecidable position excludes and reports.** If `HEAD` cannot be determined
the branch axis answers *unknown*, and unknown excludes. A whole-family total uses
a different constructor and says so, since a total that silently dropped every
member but one would be unauditable.

Ranking orders *within* a scope: a linear combination of changed-file overlap,
error overlap and recency in `[0,1]` (weights redistribute when a signal is
absent) and a signed multiplier from the candidate's outcome label. No model, no
embeddings, an injected clock.

---

## 6.12 What a context window already holds

Everything above asks *is the state you named still current?* It cannot answer,
one instant before a read: **does this window already hold these lines of this
revision?** Nothing recorded it in a form a hook could reach without opening a
store. The cost, measured on this repository: 14.8 % of read tokens re-read a
revision the session had already read (98 reads, 500,428 tokens) and 25.6 %
re-read a file changed since (58 reads, 866,705 tokens;
[§6.13](#613-the-agent-holds-what-it-wrote)).

So the read is recorded where it happens:

```text
.rift/state/reads/<key>.jsonl              the live file, appended to
.rift/state/reads/<key>.<n>.rolled.jsonl   a live file that filled up
.rift/state/reads/<key>.<n>.snapshot.json  the merged state of everything below n
```

**It is run state, never a record.** Rows carry paths, hashes, line numbers and
counts, never content; nothing is canonical or synced. Losing the directory costs
a re-read that would have been declined — the safe direction. It is a file
because a decision taken while an agent waits can afford a file read, not a
store open. At condense the archive copies the rows into the
checkpoint's `window_journal.jsonl`
([§5.3](05-ledger-and-verification.md#53-anatomy-of-a-checkpoint)) and the ingest
reads them into `n_window_row`, so a rebuild restores every decline, edit and
eviction and no hook writes the relation.

**The key is one context window, not one session.** A subagent runs under the
*parent's* session id and holds different bytes, so the key hashes the session id
with the agent id the hook carries. A family whose payload cannot tell them apart
still records but never declines: a window it cannot separate is one it cannot
make a claim about. The same key names the pointer marker and the manifest
baseline ([§7.2](07-serving-and-injection.md#72-tier-one-the-session-pointer)).

**Rows are validated on the way in.** The directory sits in a tree an agent can
write to, so a row failing its shape is dropped, never interpreted. Fifteen kinds
exist, among them a read, a decline, an observed decline, an edit, an eviction,
the subagent links of [§7.3](07-serving-and-injection.md#73-tier-two-the-per-turn-delta-manifest),
an **announcement** (`hold`: a manifest or recovery-map entry saying this window
holds these lines) and the one **booking** it is worth once the window acts on the
file without re-reading it (`prevented`,
[chapter 8](08-cost-and-savings.md#86-what-avoided-actually-measures)). Those two
are never coverage: what a window was *told* is not what it was handed, and only
the latter may decline a read.

### Coverage, and why it is deliberately pessimistic

**Coverage** of `(path, content hash)` is the union of read intervals recorded for
that pair since the last eviction, plus the edit-derived coverage of §6.13.

- **A different revision has no coverage.** Intervals are scoped to the hash and
  never shifted to fit new bytes.
- **A range is recorded conservatively**, from pinned tool behaviour: never more
  than the tool can have delivered. A span is *lines*, *verified bytes*, or
  **unknown**, and unknown never satisfies a decline; a client truncating at a
  *token* cap leaves a call recorded as nothing.
- **Everything that goes wrong subtracts.** An unreadable file or snapshot, an
  over-long row or an uncreatable directory yields *less* coverage, never a
  decline of lines the window lacks; an unreadable snapshot is read as an
  emptying, the one direction that would otherwise be unsafe.

**The client's own statement about a delivery outranks that arithmetic.** Where a
family's result object names the run it handed over, that run is what the window
is credited with whatever the slice weighs, clamped to Rift's own line count. This
is why the largest files, which no density can vouch for, are reachable. The price:
the statement arrives one hook after the call, so two identical reads in one turn
both run.

### Eviction keeps what it stops counting

A window is emptied on the client's boundaries — compaction, clear, resume — and
an eviction line ends earlier coverage's authority: the bytes are gone, and
declining a read would starve the agent. The pre-eviction lines are **kept**, as
the input to the recovery map ([chapter 7](07-serving-and-injection.md)). The
first re-read of a file after a compaction is allowed, the second declined: one
read per file per compaction is the floor of any design that never lies about what
a context holds. Compaction is common (about 0.85 per session with reads here).

`rift memory reread-report` and `rift savings --json` (`compaction` key) publish
evictions per session and the post-eviction reading storm.

### Concurrency and the two halves

Each row is one atomic append of a few hundred bytes at most; a reader drops a
final row lacking its newline; a file at its line cap is renamed aside with a
merged snapshot beside it. The *decision* (schema, intervals, coverage,
predicate) is pure, in the agent SDK, and property-tested; the *files* live in the
`rift` binary; a per-family payload difference is an adapter method with pinned
field names ([chapter 10](10-agent-integrations.md)). `rift clean` collects old
windows and `rift doctor` counts them.

---

## 6.13 The agent holds what it wrote

The largest measured pool is the read of a file **the agent just edited**, to
confirm the edit landed (the 58 reads of §6.12, about 15,000 tokens each). The
new bytes are in the agent's own tool call; the read is habit. Rift knows the
agent holds the file only by an equality it checks, not by assuming editors:

1. **The hash a write was asked to produce is recorded before the write runs.**
2. **After the edit, the file is hashed.** A whole-file write either hashes to
   what the agent supplied — it holds the file, coverage is the whole file — or
   it does not: **formatter drift**, the agent does *not* hold what is on disk,
   nothing is recorded.
3. **A replacement edit** has its new text located in the new bytes. Exactly one
   occurrence (or the count a replace-all asked for) gives the edit site in the
   **new** numbering and the shift, and full coverage carries forward only if
   coverage at the old hash was full. Anything ambiguous records nothing.

Multi-edits, notebook edits and patches record no coverage: a site Rift cannot
state exactly is one it will not state. Line shift is no correctness problem, since
a new hash starts empty. **The echo is never the evidence**: no family was measured
returning a snippet Rift could rely on, so coverage comes from bytes Rift hashed.

---

## 6.14 Knowing where the files are

Beside *is what I hold still true* sits *where is the file called something like
this*. An agent asks with a glob and gets an alphabetical page, while Rift knows
which paths this family's sessions read. Each member keeps a **path index**: every
path the checkout may answer with and its modification time, in one run-state file.

- **Keyed by what would invalidate it** — the commit `HEAD` points at and the
  newest modification time seen. A mismatched key is not a stale index to patch;
  it is not an index.
- **A damaged file is not an index.** Truncated or unreadable reads as *absent*,
  meaning the agent's own tool runs as written.
- **Nothing walks a tree while somebody waits.** A walk starts only where measured
  to fit the hook's budget, is abandoned once it clearly will not, and returns
  absence plus a request that the detached worker rebuild it
  ([chapter 3](03-capture.md)). A partial index is never written: it is a wrong
  answer that looks right.

**The ranking is Rift's own, over facts it records**: warmth from window journals
and read counts, recency from the index, dirtiness from git; no second database.
As a command it gathers the journals of *every family member*, so a file a sibling
read ranks warm here ([chapter 9](09-worktrees-and-families.md)); inside a hook it
uses only the matched paths and this conversation's journal and runs no git
status — a frame that tagged nothing is honest where one that guessed is not. Walk
order cannot change an answer (a property test feeds one path set in different
orders).

The index holds **names and modification times, never content**, and never a path
the redaction policy calls sensitive or git ignores: the walk excludes them, so
they cannot be ranked, named or matched later. A *content* search is a different
question
([§7.7](07-serving-and-injection.md#77-searches-are-bounded-and-sometimes-answered)).

## 6.15 What is running, and what is not

**Summaries are bound at the read and served on pull.** The detached worker
that counts a read binds the outline of the code or Markdown file it read and
caches it for the hooks; a condense
([§3.7](03-capture.md#37-from-session-to-checkpoint-the-condense)) binds any file
the session read, has not changed since, and left unbound.
`rift recall read-summary <path>` answers `summary` with the outline, key, versions
and `file:` handle; `known-state` makes an entry `summary_included` when the
checkout hashes to the bound revision and the outline fits; the pack renders it
with names left out. Another revision answers `stale`; an older-version or absent
binding `unavailable` with `not_bound`; an unreadable digest `unavailable` with
`digest_unavailable` — honest absence, not a guess dressed as a digest.

**A read's four stages are kept apart**: *receipt* (the gate's journal line),
*capture* (the checkpoint that froze it), *queryable* (the first ingest covering
that checkpoint), *delivered* (an answer built on it reached the agent). Each
names its join, the client's `invocation_id` or else `path_hash` (window, path,
content hash), and none is matched by time: no match is absent, two reads the key
cannot tell apart are ambiguous. `rift recall status --json` summarises the newest
200 reads, three lags kept apart; `rift doctor` names reads still not queryable
after a whole five-minute sweep.

Recall keeps no in-session index of content; §6.14's path index answers
*where*, never *what does it say*. Every response carries `estimated_tokens_avoided`, an upper bound: a full read of
the candidates against what was returned ([Chapter 8](08-cost-and-savings.md)).

---

## Related chapters

[Chapter 5 — The ledger and verification](05-ledger-and-verification.md) ·
[Chapter 7 — Serving and injection](07-serving-and-injection.md) ·
[Chapter 8 — Cost and savings](08-cost-and-savings.md) ·
[Chapter 9 — Worktrees and families](09-worktrees-and-families.md) ·
[Chapter 13 — Storage and schema](13-storage-and-schema.md) ·
[Chapter 17 — Invariants](17-invariants.md) ·
[Chapter 18 — What Rift does not do](18-what-rift-does-not-do.md)
