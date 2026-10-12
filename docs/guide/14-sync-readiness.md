# 14. Sync readiness

Rift is local-first: nothing it captures leaves your machine unless you name a
destination and ask. The formats let a ledger travel — to another checkout, or
across an air gap — without a format change, so moving it is a transport problem,
not a format problem. There is no network code, server or wire protocol; there is
a stream you resume from one token, a queue that knows what has not shipped, and
a transfer file that verifies before it is trusted. Only `rift sync flush` can
put bytes on a network, and only to a remote you configured; `rift sync status`
says *why* nothing has shipped, because a silently growing queue looks like a
broken one.

---

## 14.1 The stream cursor

A **stream cursor** is a consumer's position in the append-only chain of
[Chapter 5](05-ledger-and-verification.md): one token,
`rift-cursor/<schema_version>:<chain_seq>:<checkpoint_id>`, with exactly three
fields (a fourth is a refusal). `rift-cursor/1:0:` is the origin.

- `schema_version` is carried *inside* the token; readers accept 1 through the
  current version and refuse the rest **whole**.
- `chain_seq` is the **1-based position** in the chain's own order (`0` =
  origin), not a timestamp or row count.
- `checkpoint_id` is at most 64 ASCII letters and digits, empty **only** at the
  origin; a mismatch of the two is a refusal.

Only the canonical spelling parses: a cursor is digest-bound in a manifest, and a
token with two spellings cannot be byte-compared.

**Why position, not time.** A clock orders the stream by when the writing machine
thought it was, so skewed writers reorder it; chain linkage cannot be reordered
without breaking the chain. Position orders the stream and the identifier
**binds the cursor to one chain**: a mismatch means a different sequence, and the
resume fails closed.

**Not the change detector.** The projection's **watermark** tells the dashboard
whether anything landed and legally resets backward on a rebuild; a cursor never
moves backward, so a resume built on the watermark would let a rebuild rewind a
consumer.

---

## 14.2 One resume path, two consumers

Agreement between two implementations breaks when one is edited, so **bundle export** (over the stored chain) and
**recall's index warm** (over the substrate walk, [Chapter 6](06-recall.md)) call
one suffix-after-a-cursor function. It fails closed on a cursor **ahead** of the
stream and on one whose position now holds a **different** checkpoint.

Index warm records the tip it reached and the position that tip held, which
catches a checkpoint sorted in *before* a surviving tip: the recorded position
now names another checkpoint, so the ingest takes a counted full pass — correct
(apply is a set-union), merely slower, so a rewrite is counted, not refused.

---

## 14.3 The push queue

A checkpoint write **enqueues** and a shipper drains later, so a slow remote
never touches capture. The queue is `<common_dir>/rift-push-queue/`, one per
family and drainable from any worktree: an entry names a *reference*, and
references live in the shared object store
([Chapter 9](09-worktrees-and-families.md)).

An entry holds a schema version, the reference name, the stream position and the
time — no capture bytes, since a queue of transcript text would be a second place
to keep secrets out of. Unknown fields are **ignored**, so a newer build's entry
drains through an older shipper. Two entries are written per checkpoint (chain
reference and checkpoint branch) at the chain append, so the queue never names a
position the chain lacks. The enqueue is best-effort and silent: the references
are durable in git and the next checkpoint or flush enqueues them again, and a
queue that could fail an archive is the worse trade.

**Draining.** A drain **renames** the live file to
`draining-<timestamp>-<process-id>.jsonl`, ships, then deletes it. A crash on
either side leaves the claim for the next drain, and shipping a reference already
at the destination is a no-op, so re-draining *converges*. A lock guards the
rename only, so a hanging remote cannot block a checkpoint write; one older than a
minute is stolen. `rift doctor --fix` **adopts a stranded per-checkout queue and
never ships**; shipping stays a verb you type.

---

## 14.4 There is no default destination

The target must be **named** (`rift configure --checkpoint-remote`,
[Chapter 2](02-configuration.md)). Absent, the queue **accrues** — losing
nothing, since the references are durable in git — and `rift sync flush` refuses
with a sentence saying what to configure.

There is no fallback to `origin`: it is whatever you happened to clone, and on
the machine this was designed on it resolved to an unrelated third party's
project, so falling back would publish prompts, file paths and cost facts into
someone else's repository as a side effect of a push meant for your own code.
There is no `--remote` flag either: the destination is repository policy, so a
mistyped command line cannot aim a ledger somewhere new. The value is screened
before it becomes a command-line word and is outside the policy hash.

---

## 14.5 The push screen, and the one carve-out

The `pre-push` hook refuses Rift's own references in all three spellings
(`refs/heads/rift/…`, `refs/rift/…`, the bare `rift/…`), rejecting the whole
push; it is the only Rift git hook that may exit non-zero
([Chapter 17](17-invariants.md)). The bare spelling is screened because a
hand-typed push of the chain reference would send the repository's capture
*order* off the machine unremarked. It matches a namespace, not a substring (`riftless` passes).

The shipper is the carve-out: its push is the sanctioned egress of exactly these
references to the named remote, so it passes `--no-verify` (no force, no mirror,
no tags; append-only references mean a push fast-forwards or fails). The hook
drains the queue after a push you started *only* if the remote is the configured
one, compared by name and URL byte-for-byte and **not** normalized: two spellings
of one URL count as different remotes, erring toward *not* shipping, because a
normalizer is where two different remotes become one. Other remotes drain
nothing and print nothing, and the drain cannot change the hook's exit status.

---

## 14.6 Bundles: the offline transfer unit

One file: a frame naming each part and its byte length (`rift-bundle/1`, then
`manifest <len>`, `segment <len>`, `gitbundle <len>`). The **manifest** holds the
version, exporting repository, the two bounding cursors, the reference list and a
digest of each other part; the **chain segment** is the tamper-evidence, the links
between the cursors; the **git bundle** carries the objects and references.

A git bundle carries only objects, so a manifest would have to ride inside it,
and reading it would mean unbundling first — inverting the ordering the format
exists for: **verify before trust, and the cheap offline check comes first.**
Any mis-framing is a **refusal**.

**What the segment proves** is **order and membership**, as a pure function of
its own bytes: each link's predecessor is the previous link, the first attaches
to the lower cursor (or is genesis), and the upper cursor sits at lower position
plus link count, so a dropped or inserted link is a refusal. It does **not**
prove a Merkle root is the root of anything; `rift verify` re-derives it from
the checkpoint's contents in the git half
([Chapter 5](05-ledger-and-verification.md)). A flipped root is caught only by the manifest digest in transit.

**Export is scoped; the git side is whole.** `--since` scopes only the
**segment**: a bundle with prerequisites cannot apply to a fresh clone, which is
the format's purpose.

**Import is all-or-nothing**, in this order:

1. frame, 2. manifest, 3. part digests, 4. segment rules (offline);
5. segment agrees with manifest; 6. segment attaches to *this* chain;
7. git accepts the bundle; 8. its heads equal the manifest's references;
9. objects read in; 10. every reference a fast-forward; 11. one reference
transaction.

Only step 11 moves anything, and apply is a set-union, so a failure leaves every
reference in place and re-importing converges. Step 6 uses the same predicate as
the projection-side import and fails closed: an incremental range cannot
bootstrap a repository with no chain, the lower cursor must name a position the
local chain holds, and overlapping chains must be the same chain — the origin
cursor carries no identity, so without that arm an origin bundle would attach to
*every* repository and write a second genesis.

**Import moves references and writes no rows**, and never opens the projection:
a fresh clone is what a bundle reaches, and `rift rebuild && rift verify` follow.
A chain link carries no capture *strategy*, so a bundle-side writer would invent
one label that, with insert-or-ignore and no `UPDATE`
([Chapter 13](13-storage-and-schema.md)), would permanently shadow what the
rebuild derives. Exit codes are in [Chapter 1](01-quickstart.md).

---

## 14.7 Wire-format discipline

Every artifact that can cross a machine boundary carries a `schema_version`;
where this chapter and the code differ, the code decides.

- **Readers accept up to the current version and refuse the rest whole**; writers
  emit the current version.
- **One canonical spelling per value**: integers are ASCII digits with no leading
  zero or sign, from one shared function, because separate copies drift (the
  segment header once accepted `01` and `+1`).
- **Lowercase hex where the encoder writes lowercase.** The shared parser is
  case-insensitive, so the strict rule sits at the reader that needs it: an
  uppercase spelling is one artifact with two names, and a digest-bound part
  stops matching its bytes.
- **Refusals name reasons, never inputs**: a part name is attacker-chosen text.

**The second-implementation test:** could a second implementation be written
from the document alone? An independent reader of the offline steps, using none
of Rift's crates, must agree with the importer's exit code on every real and
mutated bundle.

---

## 14.8 What a remote reader may depend on

1. A checkpoint is **independently fetchable**; only the chain orders them.
2. **Deciding is cheap**: identity, position, predecessor and root are in the
   segment, with no file contents.
3. **Verify before trust, offline**; root authenticity needs contents and comes
   second.
4. **Resume is a single token**: no session, no per-client server state.
5. **Apply is idempotent**, so a transport may be at-least-once.
6. **Reading leaves no durable state in the reader's repository.** A filtered
   fetch writes promisor settings into the fetching repository, after which every
   fetch from that remote is silently filtered: a reference advances to a commit
   whose contents never arrived, and un-setting the configuration does not
   recover. Rift has no filtered-fetch path; partial transfer uses a disposable
   object store, explicit objects, or the segment deciding which references to
   advance.

---

## 14.9 What is deliberately not built

No network transport, authentication, server, framing, compression or delta
protocol; the queue's one shipper is a git push to the configured remote.
Bundles are files you move, and import does not project, so a rebuild follows it
([Chapter 18](18-what-rift-does-not-do.md)).

---

## Related chapters

[02-configuration.md](02-configuration.md) ·
[05-ledger-and-verification.md](05-ledger-and-verification.md) ·
[09-worktrees-and-families.md](09-worktrees-and-families.md) ·
[13-storage-and-schema.md](13-storage-and-schema.md) ·
[17-invariants.md](17-invariants.md) ·
[18-what-rift-does-not-do.md](18-what-rift-does-not-do.md).
