# 5. The ledger and verification

[Chapter 3](03-capture.md) ended with a commit condensing into a checkpoint.
This chapter is what that checkpoint *is*: where it lives, what it contains, how
it is bound to every byte it claims, and how you can prove later that nobody
quietly changed it.

The promise is narrow. Not "your history cannot be edited" — anything on your
disk can be edited — but: **if it is edited, `rift verify` says so.** That is
*tamper evidence*.

---

## 5.1 Try it first

```console
$ rift verify
verify: OK — re-derived 182 checkpoint(s) from git; 182 shard(s)
redaction/Merkle-checked (103 predate the derived Merkle root …)
```

The parenthesis names what was *not* fully checked: a verifier that quietly skipped
part of its job and still said OK would be the failure it exists to catch. `--json`
reports the run. It is read-only and takes no arguments.

---

## 5.2 Two records, and only one of them is canonical

Rift keeps every durable fact in **git objects** on refs of its own, plus the
*projection*, a local SQLite database at
`~/.cache/rift/<family-id>/projection.db`.

> **Git is the record. The database is a rebuildable view over it.**

Delete `projection.db`, run `rift rebuild`, and the same content returns from git
alone; the reverse is impossible. `rift verify` re-derives the chain from git and
compares the database against it, so a disagreement means the database is wrong or
somebody edited git. Losing the database is an inconvenience; losing the git refs is
data loss ([Chapter 14](14-sync-readiness.md)).

| Ref | Holds | Pushed? |
|---|---|---|
| `rift/<short-commit>-<worktree-hash>` | in-progress, **un-redacted** session state | never |
| `refs/heads/rift/checkpoints/v1` | the redacted checkpoints (the ledger) | only on request |
| `refs/rift/chain/v1` | the canonical *order* of those checkpoints | only on request |

The first may hold material that never passed the redaction gate, so nothing on the
read side touches it ([Chapter 4](04-redaction-and-secrets.md)). The `pre-push` hook
rejects a whole push carrying the other two, in three spellings (`refs/heads/rift/…`,
`refs/rift/…`, bare `rift/…`: a namespace missed is a namespace leaked), because
sessions leave the machine only through a destination you configure
([Chapter 14](14-sync-readiness.md)).

---

## 5.3 Anatomy of a checkpoint

A checkpoint is a directory of small files — a **shard** — committed onto the
checkpoint branch under a path derived from its own id, `<id[:2]>/<id[2:]>/`. The
file set is a **registry in code**: a test drives a maximal checkpoint through
the writer and asserts git holds exactly the declared set, because an artifact
planted under an undeclared name changed no compared byte.

| Artifact | What it is |
|---|---|
| `content.redacted` | the capture document, after redaction |
| `content_hash.txt` | digest over `metadata.json ‖ redaction_report.json ‖ full.jsonl` |
| `digest.{failures,intent,log,shape}.json` | four mechanical extractions |
| `full.jsonl` | one line per archived canonical event |
| `metadata.json` | header: sessions, recorded Merkle root, `canon_version`, task list, token rollups |
| `redaction_report.json` | counts only — what redaction removed, never what |
| `transcript.jsonl`, `transcript.compact.jsonl` | the transcript, full and compacted |
| `sessions/<index>/metadata.json`, `tasks/<tool-use-id>/…` | per-session headers (2+ sessions); one subtree per subagent |
| `sessions.jsonl` | each covered session's native id and agent token |
| `tool_calls.jsonl` | each read's call id, tool, path, content hash, token count, and when held its instant, turn, blob oid, tokenizer |
| `tool_results.jsonl` | measured tool results, the search half of the savings denominator |
| `usage.jsonl` | settled model calls: token classes, counting method, model, call key |
| `session_worktrees.jsonl` | one line per distinct worktree state a session was seen in |
| `serve_audit.jsonl` | the serves attributed to the session, as canonical envelopes |
| `window_journal.jsonl` | every row the session's context windows held |

The compact transcript is deterministic, with no model at capture time: a summary
would be an opinion, a compaction is arithmetic, and a verifier can re-derive it. The
redaction report is re-verified on every `rift verify`, and the artifact bridge
refuses a checkpoint whose report is missing.

The sibling artifacts (from `sessions.jsonl` down) are separate files, not
`metadata.json` members, because `metadata.json` is in `content_hash.txt`'s preimage
and the digest's definition does not move. Each is built at archive time from
identifiers, counts, instants and closed words only; a value outside its grammar is
frozen as `withheld` or its line dropped, never rewritten; each is cumulative per
session, so re-archiving an unchanged session derives the same id.

- **Sessions:** `metadata.json` names only a native id; without this file a rebuild
  would guess the agent.
- **Tool results:** a search resolves to no file, so no `tool_calls.jsonl` line
  (path and content hash required) can hold it.
- **Worktrees** ([§9](09-worktrees-and-families.md)): only agent-hook observations
  freeze; `commit`, `attach` and `import` record a verb's act after its archive, and
  freezing one would mint a new checkpoint for an unchanged session. The head (every
  commit moves it) and the checkout path (a local address) are never members.
- **Window journal** ([§6.12](06-recall.md#612-what-a-context-window-already-holds)):
  the journal files are run state a sweep deletes; this is where the declines the
  savings figures rest on become tamper-evident.
- **Serve audit:** every member that could hold typed text (an exact retrieval's
  reason, a refusal's target) is replaced by `withheld`. A schema-1 line has no
  clock, so a rebuild reads it only with a top-level `recorded_at`. The
  `memd_audit_session` row decides which shard holds a serve; a serve no session
  made freezes nowhere.
- **Usage:** frozen so a rebuild does not report zero; no price, since dollars derive
  at read time from a catalog that moves on its own. Spend settles one hook after its
  turn ([03-capture.md](03-capture.md) §3.9), so it reaches the shard one hook later.

The **`Rift-Checkpoint` trailer** claims, before the commit exists, the id its
checkpoint will have; it is exact when the session has no transcript in the shard and
nothing lands before the condense. Nothing reads it as the answer: a commit resolves
through the union of the trailer's claim and the checkpoints recorded against it (each
shard keeps its own commit anchor). `rift checkpoint explain` renders a shard for a
human (no argument: `HEAD`'s).

---

## 5.4 Leaf, root, chain

Tamper evidence is SHA-256 in layers, each with a one-byte **domain prefix** so no
layer's value can masquerade as another's (second-preimage confusion):

| Layer | Prefix | Construction |
|---|---|---|
| leaf | `0x00` | `SHA-256(0x00 ‖ bytes)` — one artifact, or one event line |
| internal node | `0x01` | `SHA-256(0x01 ‖ left ‖ right)` |
| empty root | `0x02` | a fixed constant for the empty leaf set |
| checkpoint root | `0x03` | binds session roots to the cost and attribution rollups |
| commit hash | `0x04` | chains a checkpoint to its predecessor |

Odd levels pair the last node with itself. The empty set has a fixed root rather than
an error, so "committed to nothing" is representable and checkable (§5.7). Hex
vectors cross-checked against an independent implementation pin all of it.

**Leaf order is the shard's own file order**: content artifacts in bytewise
full-path order, as git sorts the tree, each `full.jsonl` line its own leaf
(`sessions/<index>/…` bytewise, so "10" precedes "2"; `tasks/<id>/…` between
`redaction_report.json` and `tool_calls.jsonl`; `usage.jsonl` and
`window_journal.jsonl` last).

**Absence is the compatibility contract.** A checkpoint lacking an artifact derives,
leaf for leaf, the sequence it would have had the artifact never existed, so an
artifact can be added without moving any existing root, provided its name sorts where
it lands (`usage.jsonl` sorts last, so its leaf is appended, never inserted).
`metadata.json` (it carries the root) and `content_hash.txt` (it digests
`metadata.json`) are excluded. Leaves combine pairwise in sequence, so swapping bytes
between artifacts changes the root; the tested claim is that **changing any single
artifact's bytes changes the root.**

---

## 5.5 The checkpoint's name is the binding

A checkpoint records its Merkle root in `metadata.json` — a file whoever edited
`content.redacted` can also edit: recompute the root, write it back, and every check
comparing recorded to derived root passes. The one binding an editor cannot rewrite
by writing a file is the path:

> **The checkpoint's id is derived from its Merkle root, and the id is its path.**

The id is `sha256(session ‖ NUL ‖ hex(root))` over the sessions covered and the root
**re-derived from the artifacts**, not the one the file claims. Editing an artifact
moves the root and the id, so the edited bytes no longer address the directory they
sit in. A byte change mints a new checkpoint rather than corrupting an old one;
re-archiving an unchanged session is a no-op, so capture is idempotent; and
`metadata.json` and `content_hash.txt` sit outside the id, since they move when a
second commit lands in the same session and would mint a spurious checkpoint. The id
binds the *root* rather than per-artifact digests because each digest would need a new
formula and accepted-derivation entry whenever an artifact is added; the root already
commits to all of them, in order.

---

## 5.6 The chain ref: where the order lives

A checkpoint knows its content but not its **place**, and "*k* was written after
*k−1*" is half of what a hash chain asserts. Linking in sorted content-hash order
fails: a hash has no relation to write time, so a new checkpoint would sort into the
middle half the time and name a predecessor that already has a successor, and
append-only forbids correcting the older row.

`refs/rift/chain/v1` is a ref whose history *is* the order: one commit per
checkpoint, parent the previous entry, the id in a **commit trailer** (the order is
readable without fetching a tree or blob), an empty tree. Nothing rewrites a parent
or deletes an entry, and creating the ref refuses if it exists.

```console
$ rift chain status    # positions recorded, head entry, head checkpoint
$ rift chain init      # only on a repository that has never recorded its order
```

`init` derives the order from the branch's first-parent history, the only
append-stable witness. It is a verb a person types: creating an append-only artifact
is a decision and needs an author. A merge on the branch has no single write order and
*inventing* one is the defect this ref exists to end, so it is refused. The detached
worker appends each new position. Chain maintenance is **loud but never fatal**: it
must not strand capture input, nor be swallowed, since an unrecorded checkpoint is
what the next verify refuses.

---

## 5.7 What `rift verify` actually checks

Three parts, ordered because an earlier failure makes the later ones meaningless.

**First — the order is complete, both directions.** Every checkpoint on the
branch has one chain position and every position names a checkpoint on it.

A duplicate position, disagreeing orders (one append-only history was rewritten) or
an orphan entry (the branch was rewritten, or this is another repository's chain) are
**tamper**; an unchained checkpoint (the order has not caught up; the next archive
heals it) is **structure**.

**Incomplete is not falsified**: collapsing the two would cry wolf on ordinary lag
or hide a real rewrite behind a reassuring word.

**Second — per-checkpoint passes**, in order: (1) **redaction**: the report must
exist, parse and be valid; (2) **recorded-leaf recompute** for a checkpoint that
writes its own leaf list (the older dialect's `merkle_leaf.txt`); the rest is
native-only, skipped for checkpoints read from the pre-Rift ref — gated on the
**dialect** (which ref it came from), never on what the checkpoint says about itself,
because where it *is* cannot be rewritten by editing it; (3) **subagent subtrees**:
directory name and header `tool_use_id` agree, and the parent's token rollup is
**re-summed** from the leaf-bound headers (a stripped rollup hides spend); (4) **strict Merkle**: re-derive the root with
*the function the writer calls*, so they cannot drift (vacuous-root exception:
§5.11); (5) **checkpoint id**: re-derive the accepted ids for the declared
`canon_version` and require the checkpoint's own path to be one. A missing artifact
the root or id commits to is tamper: the writer always emits it.

**Third — the projection agrees.** The git-derived chain is compared position by
position with the database's; any difference is an integrity failure naming the first
divergent checkpoint. The projection is routinely *behind* git by the checkpoints just
condensed, so verify **levels it first**, inside its own entry point so no caller can
skip it. And it **may not seed what it compares**: filling an empty projection from
`source` and comparing to `source` would make every fresh repository "verify clean";
seeding is allowed only on the fixture path, which says so.

| Exit | Verdict | Meaning |
|---|---|---|
| 0 | OK | everything checked and agreed |
| 1 | TAMPER | bytes on offer are not the bytes recorded |
| 3 | REDACTION | a redaction report is missing or invalid |
| 5 | STRUCTURE | incomplete, not falsified |
| 70 | internal | projection could not be opened or migrated — never a silent 0 |

A run reports its **worst** finding (tamper over redaction over structure).
Exit 3 is also what `rift verify` returns outside a repository family. `rift verify --quiet`
suffices in CI.

---

## 5.8 The verify-green streak

`rift doctor` prints `soak: 56/20 clean condensations (68/20 distinct, …)`. The
tamper-evidence machinery is the oracle for the native write path: 20 consecutive
clean verifications of *real* captures, recorded append-only, by the same code `rift
verify` runs (a different oracle would certify nothing). One bad run resets the
count; zero runs never clears the gate; the population is condensations, not
verifications (a bare `rift verify` contributes nothing — reaching the writer needs a
witness value); and the *distinct*-checkpoint count must also reach 20, else twenty
re-condensations of one unchanged session clear it. **A divergence from any source
vetoes the streak permanently**: the log is append-only, so only a rebuild from git
clears it. Narrower scoping was rejected: a tamper finding is a fact about the *repository*. It is per repository family
([Chapter 9](09-worktrees-and-families.md)).

---

## 5.9 Three layers of append-only

The local database enforces append-only three independent times so no single mistake
removes it. **Type layer:** the store trait has `insert` and `insert_or_ignore`, no
`update`, no `delete`. **SQL layer:** every canonical table has `BEFORE UPDATE` and
`BEFORE DELETE` triggers that abort, so raw SQL cannot mutate a row. **Registry-driven
property test:** a test inserts a sample row into every registered table and asserts
both `UPDATE` and `DELETE` are refused — **add a table without its guard and CI goes
red** ([Chapter 13](13-storage-and-schema.md) counts and details them). Corrections
are new superseding rows, bitemporal for cost ([Chapter 8](08-cost-and-savings.md)).

---

## 5.10 Two hashes over the same bytes

SHA-256 wherever byte-compatibility with git matters (the chain, `content_hash.txt`,
the checkpoint id; worktree content hashing too, to match the hash capture records at
read time); blake3 for internal handles. They are **distinct types** with no
conversion, proven by a test that asserts the conversion does not compile. Git's blob
id is a third framing, bridged by an append-only table ([Chapter 6](06-recall.md)).

---

## 5.11 Reading older histories

```console
$ rift import --from-legacy
```

Rift reads checkpoints written by the capture tool that preceded it, in both layouts
it shipped. The import is **read-only**, goes through the ordinary write path with the
redaction gate, and is idempotent. `rift verify` verifies imported history as native,
reading it from the pre-Rift ref and reporting it separately so its exemption from
native-only passes is visible.

**Verifying an older capture.** Each checkpoint declares a `canon_version` in
`metadata.json`, and the verifier selects rules by it — never by date or branch, so
cloning, rebasing or restoring a bundle cannot change what a checkpoint says about
itself. Version 3 is what Rift writes (§5.5). Version 1's recorded root must be the
empty-set constant (its capture is bound by the id); version 2's root is derived but
its id bound only the capture and read facts. Up to version 2 the older id
derivations are accepted, because refusing them would call an honest old checkpoint
tampered; version 3 accepts *only* the root binding, since an older rule there would
reopen the hole. Relabelling downward buys nothing: the rules give *different ids
for the same bytes*, so a path evidences its version. Redaction and leaf recompute
run on every version, and a native checkpoint whose label was deleted is held to the
native passes.

---

## 5.12 When verify is not OK

- **STRUCTURE, "no chain entry":** the order has not caught up. If this repository has
  recorded an order before, the next commit heals it (`rift doctor` confirms the queue
  drains); if never, run `rift chain init`.
- **REDACTION, "missing redaction report":** not repairable — the artifact is part of
  what the id addresses, so its absence is a real change. Treat as tamper-adjacent.
- **TAMPER:** something append-only was rewritten. Do not "fix" it by rebuilding: a
  rebuild derives the projection from git, so if git changed it agrees with the
  change. The message names the checkpoint. If only the projection drifted, deleting
  `projection.db` and running `rift rebuild` restores agreement safely.

---

## Related chapters

[Chapter 3 — Capture](03-capture.md) ·
[Chapter 4 — Redaction and secrets](04-redaction-and-secrets.md) ·
[Chapter 6 — Recall](06-recall.md) ·
[Chapter 13 — Storage and schema](13-storage-and-schema.md) ·
[Chapter 14 — Sync readiness](14-sync-readiness.md) ·
[Chapter 17 — Invariants](17-invariants.md)
