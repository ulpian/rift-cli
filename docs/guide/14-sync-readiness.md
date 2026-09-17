# 14. Sync readiness

Rift is local-first: nothing it captures leaves your machine unless you name a
destination and ask for it. This chapter is the plumbing under that, because the
formats were designed assuming a captured ledger may *one day* travel — to
another checkout, to a colleague across an air gap, or to a future,
separately-secured cloud surface that does not exist yet.

The decision behind everything below: **build the local halves now, so hosting
later is a transport problem and not a format problem.** No network code, no
server, no authentication, no wire protocol — but a stream you can resume from
one token, a queue that knows what has not shipped, and a self-contained
transfer file that verifies before it is trusted.

---

## 14.1 Try it first

Two read-only looks that contact nothing. `rift bundle --help` lists **export**,
which writes a self-contained bundle of this repository's capture refs, and
**import**, which verifies and applies one (all-or-nothing, set-union).
`rift sync --help` lists **status**, what is queued and where it would ship, and
**flush**, ship it to the configured checkpoint remote. Of the four, only
`rift sync flush` can put bytes on a network, and only to a remote you
configured.

> **Tip:** `rift sync status` also tells you *why* nothing has shipped — usually
> "you never told Rift where these references belong". A queue that silently
> grows is indistinguishable from a broken one, so the verb says it out loud
> rather than reporting a cheerful zero.

---


## 14.2 The stream cursor

Every captured checkpoint lands in an append-only sequence — the chain of
[Chapter 5](05-ledger-and-verification.md). A **stream cursor** is a consumer's
position in it: "everything up to here, I have seen." It is one text token,
`rift-cursor/<schema_version>:<chain_seq>:<checkpoint_id>`, with exactly three
colon-separated fields after the prefix; a fourth is a refusal, not data to
skip. `rift-cursor/1:0:` is the origin, `rift-cursor/1:12:4f2c9ab…` position 12
bound to that checkpoint.

| Field | Meaning |
|---|---|
| `schema_version` | The contract version, carried *inside* the token because a single word has nowhere else to put it. Readers accept 1 through the current version and refuse anything else **whole**. |
| `chain_seq` | The checkpoint's **1-based position** in the chain's own order. `0` is reserved for the origin. Not a timestamp, not a row count. |
| `checkpoint_id` | The identifier of the checkpoint at that position. ASCII letters and digits, at most 64 bytes; empty **only** at the origin. |

The origin's two halves are one fact: position `0` with a non-empty identifier
is a refusal, and so is a non-zero position with an empty one; one fallible
constructor means an ill-formed cursor cannot be built. **Round-trip is a
bijection** — parsing accepts exactly the canonical spelling, no leading zeros,
no sign, no padding — because a cursor goes into a transfer file's manifest and
is then digest-bound, and a token with two spellings cannot be byte-compared
with itself.

### Why position, and not time

A clock-derived identifier would order the stream by *when the writing machine
thought it was*, so two skewed writers would reorder it — and once the order
changes, "everything after cursor C" stops being the set a consumer has not
seen. The chain's linkage cannot be reordered without breaking the chain, so
position in it orders the stream. The identifier still **binds the cursor to one
chain**: one that does not match the checkpoint now at that position means you
are reading a different sequence, and the resume fails closed. If you meet a
design note pairing identifier first and position second, read the code:
position decides, identity binds.

### Not the change detector

The capture projection also carries a **watermark** and a rebuild epoch that the
dashboard's poller reads ([Chapter 12](12-canopy-architecture.md)); conflating
the two causes real bugs.

| | Stream cursor | Projection watermark |
|---|---|---|
| Answers | *Which* events have I seen | *Has anything landed* since I looked |
| Direction | Never moves backward for a given chain | Legally resets backward on a rebuild |
| Bound to | A specific chain, by identifier | Nothing; it is advisory |
| Used by | Bundle export, recall's index warm | The dashboard poller |

The watermark is a change detector: a resume built on it would let a rebuild
rewind what a consumer thinks it has processed.

---


## 14.3 One resume path, two consumers

The rule the cursor enforces is not "two consumers agree about what a cursor
leaves unseen" but "two consumers call the same function", because agreement
between implementations is a coincidence that holds until someone edits one. So
the suffix-after-a-cursor computation lives in one place, generic over how a
stream names its items: **bundle export** calls it over the projection's stored
chain, **recall's index warm** over the substrate walk
([Chapter 6](06-recall.md)). It fails closed on two arms — a cursor **ahead** of
the stream, and one whose position now holds a **different** checkpoint.

**One honest caveat**, because "shared path" is easy to overclaim: index warm
does not persist a position — it records the branch tip it reached and
reconstructs the position from the current walk — so the position-mismatch arm
cannot fire there. What catches a rewritten stream instead is **presence**: a
recorded tip the walk no longer holds resolves to no cursor, so the ingest falls
back to a full pass and says so, which is correct (the apply is a set-union over
content-addressed rows) and merely slower. Persisting the position would need an
additive column on a canonical relation, carried as an open item in
[Chapter 18](18-known-gaps.md). The test
`the_index_warm_resumes_through_the_same_suffix_function_as_export` asserts the
two consumers hand back the same tail for the same cursor.

---


## 14.4 The push queue

Capture writes references locally; getting them *somewhere else* is a separate,
deferrable, retryable job, so a checkpoint write **enqueues** and a shipper
drains later.

**Where it lives:** `<common_dir>/rift-push-queue/`, one queue per repository
family, drainable from any worktree —
[Chapter 9's](09-worktrees-and-families.md) one exception to per-checkout state,
because a queue entry names a *reference* and references live in the object
store the family shares. Per checkout, a push from a linked worktree shipped
nothing while the main checkout's queue accrued unreachably, and removing a
worktree deleted its queue.

**What an entry holds:** a schema version, the fully-qualified reference name,
the stream position that caused the enqueue, and the time it was queued — no
capture bytes, raw or redacted, because everything a shipper needs is already in
git and a queue file holding transcript text would be a second place to keep
secrets out of. Unknown fields are **ignored** rather than rejected, the
opposite of the configuration rule ([Chapter 2](02-configuration.md)): a newer
build's entry must still drain through an older shipper.

**When it is written:** at the chain append, not the shard write, because the
chain entry makes a checkpoint canonical — so the queue can never name a stream
position the chain does not hold. Two references go in per checkpoint,
the chain reference and the checkpoint branch. The enqueue is best-effort and
silent: the references are already durable in git and the next checkpoint or
flush enqueues them again, so a queue that could fail an archive is the worse
trade. An entry naming a reference-and-position pair still queued is not written
twice, including pairs in a batch some drain has claimed.

### Draining without losing work

A drain claims work by **renaming** the live queue file aside, to
`draining-<timestamp>-<process-id>.jsonl` (both halves needed because either
alone collides), then ships, then deletes what it claimed. That makes the
sequence crash-safe both ways: a crash before the ship leaves a claimed file the
next drain picks up alongside the live queue, and a crash after it leaves the
same file, so those references ship again — and shipping one already at the
destination is a no-op, so re-draining *converges*.

A small lock file guards claims only, held **only across the rename** and never
across a ship, so a hanging remote cannot block a checkpoint write; a lock older
than a minute is treated as dead and stolen. If the shipper fails,
the claim stays and the next drain retries the batch. A per-checkout queue an
older build left behind is **adopted, not stranded**: the first reading or
draining path re-enqueues it into the family queue, idempotent by reference and
position, and removes the emptied directory.

`rift doctor` reports the queue: entries, distinct references, the directory,
and any per-checkout queue still stranded. Its `--fix` **adopts, and never
ships** — adoption moves entries between two local directories, contacts
nothing, and is safe to automate. Shipping stays a verb you type.

---


## 14.5 There is no default destination

The shipper's target must be **named** in `.rift/config.toml`:

```toml
[checkpoint]
remote = "rift-origin"
```

written by `rift configure --checkpoint-remote <name-or-url>`. Absent, the queue
simply **accrues** — losing nothing, because an entry is a reference name and
the references are already durable in git — and `rift sync flush` refuses with
an exit code and a sentence saying what to configure.

There is deliberately no fallback to `origin`, and the reason is evidence rather
than caution: `origin` is whatever you happened to clone, not "this project's
home". On the machine this was designed on it resolved to an unrelated third
party's project — and a shipper falling back to it would silently publish a
capture ledger of prompts, file paths and cost facts into someone else's
repository as a side effect of a push you meant for your own code. Resolution
happens in one function, with no fallback arm to remove.

Two smaller decisions ride along. The value is **screened before it becomes a
command-line word**: a leading `-` would read as a flag, and whitespace, control
bytes and NUL cannot name a remote, so an unusable value is an error naming the
reason, never the value. And **the remote is not part of the policy hash**,
because a destination is not policy: two clones with byte-identical rules hash
identically even when only one has a remote.

> **Tip:** There is no `--remote` flag on `rift sync`: the destination is
> repository policy, not a per-invocation argument, so a mistyped command line
> cannot aim a capture ledger somewhere new.

---


## 14.6 The push screen, and the one carve-out

The `pre-push` hook screens the references about to leave the machine and
refuses Rift's own in all three spellings — the checkpoint branch under
`refs/heads/rift/…`, the chain reference under `refs/rift/…`, and the bare
`rift/…` shorthand a person might type — one of which rejects the whole push.
This is the only place a Rift git hook may exit non-zero; every other is bound
by the never-block rule in [Chapter 17](17-invariants.md). The third spelling
was missing for a long time, so a hand-typed push of the chain reference sent
the repository's capture *order* off the machine unremarked; the test
`a_developer_push_of_rift_refs_is_still_refused` keeps it closed. The screen
matches a namespace, not a substring: `riftless` and `rift-v1` still pass.

The shipper is the carve-out, a different decision rather than an exception: its
push is the sanctioned egress of exactly these references to exactly the remote
the repository named, so it passes `--no-verify` — otherwise the screen would
reject Rift's own shipping. Everything else stays on: no force, no mirror, no
tags. These references are append-only, so a push fast-forwards or fails.

When a push you started is allowed through, the hook drains the queue *only* if
the remote git is contacting is the configured one. Identity is both halves —
remote name and URL — compared byte-for-byte and deliberately **not**
normalized, so two spellings of one URL count as different remotes, erring
toward *not* shipping. A normalizer is where two genuinely different remotes get
to become one, and this comparison decides whether a capture ledger leaves the
machine.

Every other remote drains nothing and prints nothing: push to your own remote
and you should not discover afterwards that Rift sent a ledger somewhere. The
drain cannot change the hook's exit status — a down remote, a locked queue, an
undiscoverable repository all leave work queued.

---


## 14.7 Bundles: the offline transfer unit

One file, three parts:

| Part | What it is |
|---|---|
| **manifest** | The file's claims: version, exporting repository identifier, the two bounding cursors, the reference list with each one's object, and a digest of each other part. |
| **chain segment** | The tamper-evidence: the links between the two cursors, in a canonical text encoding. |
| **git bundle** | The objects and references themselves. |

wrapped in a frame naming each part and its byte length (`rift-bundle/1`, then
`manifest <len>`, `segment <len>`, `gitbundle <len>`). A git bundle carries
only git objects, so a manifest would have to ride *inside* it and reading it
would mean unbundling first — inverting the ordering the format exists to
guarantee: **verify before trust, and the cheap offline check comes first.** An
unknown header version, a non-canonical integer, an unknown part name, a missing
part, a wrong length or a part out of order is a **refusal**, since everything
after a mis-framed part is unaligned bytes.

### What the segment proves, and what it does not

Segment verification is about **order and membership**, a pure function of the
segment's own bytes with no repository, clock or network: every link's
predecessor is the previous link and the first attaches to the lower cursor (or
is the genesis link, at the origin); the upper cursor sits where the arithmetic
demands, lower position plus link count, so a dropped or inserted link is a
refusal; it names the last link; an unknown version is refused whole.

It does **not** prove a link's Merkle root is the root of anything: a root is
authenticated by re-deriving it from the checkpoint's contents in the git half
of the file, which is what `rift verify` does against git as the authority
([Chapter 5](05-ledger-and-verification.md)). Flipping a root inside a segment
is therefore not caught by the segment's rules — stated plainly, because a
reader assuming otherwise would trust the wrong half — though the manifest
digest catches it in transit.

### Export is scoped; the git side is whole

`--since <cursor>` scopes the **segment**, the range being attested. The git side
always carries the references' full history, because a bundle with prerequisites
cannot be applied to a repository lacking them and "import into a fresh clone"
is what this format exists for. A repository with no chain reference exports
only the checkpoint branch; one with neither refuses.

### Import is all-or-nothing

The order **is** the contract: 1. frame → 2. manifest → 3. part digests →
4. segment rules (all offline) → 5. segment agrees with the manifest → 6. the
segment attaches to *this* repository's chain → 7. git accepts the bundle →
8. the heads the bundle carries equal the manifest's reference list → 9. objects
are read in → 10. every reference is a fast-forward → 11. one reference
transaction. Only at step 11 does anything move, so a failure anywhere leaves
every reference where it was; applying is a set-union, so re-importing converges.

Step 6 uses the **same attach predicate** as the projection-side import rather
than a second one that agrees today, and its arms all fail closed: an
incremental range cannot bootstrap a repository holding no chain; the lower
cursor must name a position the local chain holds; and where the two overlap
they must be the same chain — the origin cursor carries no identity, so without
that arm a bundle exported from the origin would attach to *every* repository
and write a second genesis.

### Import moves references and writes no rows

`rift bundle import` is deliberately **references-only** and does not open the
projection: a fresh clone with no local state is what a bundle exists to reach,
and `rift rebuild` projects what it brought in. The reason is mechanical: a
chain link carries no capture *strategy*, so a bundle-side writer would invent
one label for a whole range, and since insertion is insert-or-ignore while
canonical tables have no `UPDATE` ([Chapter 13](13-storage-and-schema.md)), that
placeholder would permanently shadow what the rebuild derives from each
checkpoint's own metadata.

```console
$ rift bundle import /tmp/ledger.riftbundle
bundle import: 2 ref(s) applied, 0 already present (12 link(s) verified)
$ rift rebuild && rift verify
```

An integrity refusal exits with the integrity code (65), a malformed file 1, and
a flush with no configured remote the precondition code (4)
[Chapter 1](01-quickstart.md) tabulates.

---


## 14.8 Wire-format discipline

Every artifact that could cross a machine boundary carries a `schema_version`.
The field-by-field shapes live in a working note kept outside this guide and
outside the repository, which this chapter does not restate; where the two ever
disagree, the code decides. The rules:

- **Readers accept up to the current version and refuse the rest whole**, never
  partially reading a future version as though its fields meant what this reader
  thinks; **writers emit the current version**, with no negotiation.
- **One canonical spelling per value.** Integers are ASCII digits, no leading
  zero unless the value is zero, no sign — decided by one shared function called
  by the cursor, the segment and the transfer container rather than re-derived
  per format. Each pair drifted before it was shared, the segment header
  accepting `01` and `+1` where the cursor refused them.
- **Identifiers are lowercase hex where the encoder writes lowercase hex.** The
  shared parser is case-insensitive on purpose, since other formats carry
  uppercase, so the strict rule sits at the reader that needs it: an uppercase
  spelling accepted and re-encoded lowercase is one artifact with two names, and
  a digest-bound part then stops matching the bytes it was read from.
- **Refusals name reasons, never inputs**: a part name is attacker-chosen text
  and the reader's terminal obeys escapes.

**The second-implementation test.** The bar these formats were held to is:
*could a second implementation be written from the document alone?* An
independent reader of the offline verification steps — written from the document
only, using none of Rift's crates, and pinned to stay that way — runs against
real bundles the shipped binary built and deterministically mutated. Its verdict
and the shipped importer's exit code must agree on every case, and the registry
of known disagreements is **empty**. When the document changes: diff the
entries, re-derive the reader, run the harness.

---


## 14.9 What a future remote reader may depend on

These are not plans and none may be built here: they are properties the formats
**already have**, written down so a later change cannot quietly remove one.

1. **A checkpoint is independently fetchable.** Nothing in a checkpoint's own
   metadata refers to a later one, so a reader may fetch a single reference and
   read it whole; the chain is the only structure that orders them, and it is
   carried separately.
2. **Deciding is cheap.** Everything needed to decide *whether* to fetch a
   checkpoint's contents — identity, position, predecessor, root — is in the
   chain segment, a few dozen bytes each with no file contents.
3. **Verify before trust, and the verify is offline.** Segment verification is a
   pure function of its own bytes; root authenticity is a second stage needing
   contents, and that ordering must not be inverted.
4. **Resume is a single token**, so a future protocol needs no session, no
   server-side per-client state and no negotiation — and a stale or foreign
   cursor is a refusal, not a wrong one.
5. **Apply is idempotent.** Everything is content-addressed and applies as a
   set-union, so a duplicated delivery converges and a transport may be
   at-least-once.
6. **None of this is a wire protocol** (§14.10).
7. **A read path may not leave durable state in the reader's repository as a
   side effect of reading.** A *single* filtered fetch — the obvious way to
   spend (2)'s saving — writes promisor settings into the fetching repository's
   configuration, after which every fetch from that remote is silently filtered:
   a reference advances to a commit whose contents were never transferred, later
   reads dead-end, and un-setting the configuration does not recover, because
   the reference has already moved. That rung was deleted rather than repaired,
   and partial transfer must instead use a disposable object store, explicit
   objects, or the chain segment deciding which references to advance.

---


## 14.10 What is deliberately not built

There is **no network transport, authentication, server, framing, compression or
delta protocol** — those belong to a future, separately-secured cloud surface,
and nothing here may depend on their shape beyond the contracts above. **Only one
shipper exists**: the queue talks to a consumer interface whose sole
implementation pushes with git to the configured remote, so a hosted shipper
would slot in without changing the queue. **Bundles are files you move** — no
verb fetches one. **Import does not project**, so a rebuild after import is
required. And **the dashboard does not surface the queue**: that is a
`rift sync status` and `rift doctor` fact today
([Chapter 18](18-known-gaps.md) carries these with the other open items).

---


## Related chapters

[02-configuration.md](02-configuration.md) for the state-root table and the
`[checkpoint]` settings ·
[05-ledger-and-verification.md](05-ledger-and-verification.md) for the chain
these cursors index ·
[09-worktrees-and-families.md](09-worktrees-and-families.md) ·
[13-storage-and-schema.md](13-storage-and-schema.md) ·
[17-invariants.md](17-invariants.md) · [18-known-gaps.md](18-known-gaps.md).
The field-by-field wire-format specification is a working note kept outside
this guide and outside the repository.
