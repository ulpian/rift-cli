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

If you have captured anything here:

```console
$ rift verify
  repo_id:    4d9d222b…
  projection: ~/.cache/rift/4d9d222b…/projection.db
verify: OK — re-derived 182 checkpoint(s) from git; 182 shard(s)
redaction/Merkle-checked (103 predate the derived Merkle root and carry the
empty-set constant, asserted, with their capture bound by the checkpoint id
instead)
```

The parenthesis names what was *not* fully checked — a verifier that quietly skipped part of its own job and still said OK
would be the exact failure it exists to catch, so exemptions are named rather
than counted in silence. `rift verify --json` reports the same run as a
document, keyed `checkpoints`, `pre_rift_shards`, `shards_checked`,
`vacuous_era_shards`, `verify`.

`rift verify` is read-only and takes no arguments — safe in any state, including
a repository that never captured anything, where it verifies the wiring against
a small committed fixture and says so.

---

## 5.2 Two records, and only one of them is canonical

Rift keeps every durable fact in **git objects**, on refs of its own, plus the
*projection*, a local SQLite database at
`~/.cache/rift/<family-id>/projection.db`.

> **Git is the record. The database is a rebuildable view over it.**

Delete `projection.db`, run `rift rebuild`, and the same content comes back from
the git objects alone; the reverse is impossible:

- The database is never the *authority*: `rift verify` re-derives the chain from
  git and compares the database against it, so if they disagree the database is
  wrong — or somebody edited git.
- The database is append-only anyway (§5.10): a view that could be rewritten is
  a view whose disagreement with the record would be unresolvable.
- Losing the database is an inconvenience; losing the git refs is data loss —
  hence [Chapter 14](14-sync-readiness.md)'s push queue and bundle story.

| Ref | Holds | Pushed? |
|---|---|---|
| `rift/<short-commit>-<worktree-hash>` | in-progress, **un-redacted** session state | never |
| `refs/heads/rift/checkpoints/v1` | the redacted checkpoints (the ledger) | only on request |
| `refs/rift/chain/v1` | the canonical *order* of those checkpoints | only on request |

The first may hold material that never passed the redaction gate, so nothing on
the read side touches it ([Chapter 4](04-redaction-and-secrets.md)).

The other two are refused by the `pre-push` hook if they appear in a push you
started: Rift is local-first, and captured sessions leave the machine only if
you configure a destination. The screen covers three spellings —
`refs/heads/rift/…`, `refs/rift/…`, and the bare `rift/…` a person might type,
because a ref namespace missed is a ref namespace leaked — and any of them
rejects the whole push. (The push queue's shipper is a different code path; see
[Chapter 14](14-sync-readiness.md).)

---

## 5.3 Anatomy of a checkpoint

A checkpoint is a directory of small files — a **shard** — committed onto the
checkpoint branch under a path derived from its own id:

```
<id[:2]>/<id[2:]>/
    content.redacted   content_hash.txt   full.jsonl   metadata.json
    redaction_report.json   serve_audit.jsonl   sessions.jsonl   tool_calls.jsonl
    digest.failures.json  digest.intent.json  digest.log.json  digest.shape.json
    transcript.jsonl   transcript.compact.jsonl   usage.jsonl   window_journal.jsonl
    sessions/<index>/metadata.json      (per-session header; only when the checkpoint covers 2+ sessions)
    tasks/<tool-use-id>/{metadata.json, transcript.jsonl, transcript.compact.jsonl}   (one subtree per subagent: its own header and transcript pair)
```

That set is a **registry in code**: a test drives a maximal checkpoint through
the writer and asserts the tree git holds is exactly the declared set. It exists
because an artifact planted under an undeclared name changed no compared byte —
and a version field nothing binds documents nothing.

| Artifact | What it is |
|---|---|
| `content.redacted` | the capture document — what was said and done, after redaction |
| `content_hash.txt` | a digest over `metadata.json ‖ redaction_report.json ‖ full.jsonl`, lowercase hex plus a newline |
| `digest.*.json` | four mechanical extractions: failure signatures, the intent/outcome slot, log shapes, file shapes |
| `full.jsonl` | one line per archived canonical event |
| `metadata.json` | the header: sessions covered, recorded Merkle root, canonicalization era, task list, token rollups |
| `redaction_report.json` | counts only — what redaction removed, never what it removed |
| `serve_audit.jsonl` | the serves the recall projection attributes to the session, one canonical envelope per line; absent when there were none |
| `sessions.jsonl` | the agent each covered session was captured from, one line per session: its native id and its agent's token; absent on a checkpoint written before it existed |
| `tool_calls.jsonl` | the tool calls, reduced to identifiers: each read's call id, tool, path, content hash and token count, and, from checkpoints written after they were added, the call's instant, its turn, the revision's blob oid and the tokenizer that counted it |
| `transcript.*.jsonl` | the transcript: a full copy and a deterministic compact one |
| `usage.jsonl` | the session's settled model calls, one line each: the four token classes, how they were counted, the model and the call's key; absent when none has settled |
| `window_journal.jsonl` | every row the session's context windows held when it was archived — reads, declines, edits, evictions — one line each under its window's key; absent when they held none |

The compact transcript is not a summary but the output of a deterministic
compactor: tool calls reduced to bare identifiers, file reads elided to handles,
a recorded start boundary rounding *toward* inclusion, and no model at capture
time. A summary would be an opinion; a compaction is arithmetic, and arithmetic
is what a verifier can re-derive. The redaction report is re-verified on every
`rift verify`, and the artifact bridge refuses to project a checkpoint whose
report is missing or invalid, so "this checkpoint went through the gate" is
checkable rather than hopeful.

The sessions artifact says which agent each covered session came from.
`metadata.json` names a session by its native id alone, and a rebuild keys every session row by agent and native id together, so without it a rebuild could only guess the agent, and it guessed one for every session.
It is a sibling file rather than a new `metadata.json` member because `metadata.json` is part of `content_hash.txt`'s preimage: a member there would change the digest's input for every checkpoint that carried it, and the digest's definition does not move.
Each line is the native session id `metadata.json` already names, the agent's token, and a `schema_version`; the token comes from the closed list of first-party agents, and anything else is frozen as `withheld`, which a rebuild skips.

The window journal freezes what each of the session's context windows was handed, declined, told, wrote and lost ([§6.12](06-recall.md#612-what-a-context-window-already-holds)).
The journal files are local run state that a sweep deletes; this artifact is where the declines and evictions the savings figures rest on become tamper-evident.
It is gathered at archive time from the session's own window and the window of every subagent the capture projection links to one of its turns, and it is cumulative: every checkpoint of a session carries every row its windows hold, so re-archiving an unchanged session derives the same id.
Each line carries the window's 64-hex key, the subagent id when the window is a subagent's, the kind, the turn, the time and that kind's own members — a repository path, revisions, a line range, a line count, edit sites, a shift, a flag, an eviction reason — and its own `schema_version`, which is the artifact's and not the journal's, so a change to the journal's grammar cannot silently change a frozen line.
A row whose path is not a `/`-spelled repository path, whose revision is not 64 lowercase hex or whose reason is not declared is left out whole, never rewritten; a subagent id that is not an identifier is frozen as `withheld`.
Lines are sorted by window, time, kind and the line's own bytes, with identical lines collapsed.

A read's line also carries what the savings denominator joins it through: when the call was made, the ordinal of its turn, the blob oid of the revision read and the tokenizer behind its count.
They are a time, a count, a hash and a token from a closed list, and they are written only when the projection holds them, so a session's older reads freeze exactly the lines they always did.
A blob oid that is not an object id, or a tokenizer outside the list, is frozen as `withheld`.

The serve audit is the recall plane's record of what it served, frozen where a
rebuild cannot reach it.
Every line is a serve envelope in canonical form: the declared members only,
keys sorted, and every member that could hold text a person or a model typed
replaced by the placeholder `withheld`.
The reason a caller gives for an exact retrieval is one such member, and so is
the target of a refusal unless it is a citation.
Each line keeps its idempotency key, and every line carries its own envelope
`schema`, so the artifact has no single version of its own.
A schema-1 envelope holds no clock of its own, so the recall rebuild reads a schema-1 line only when it carries a top-level `recorded_at`, the instant its row was written at; a schema-4 line keeps its instant inside `serve_call`.
Every line also names the session that made it, `session_id`, and where the writer knew it, `window_key`, the one-way key of the context window inside that session; both are identifiers, and a line whose attribution is not one is refused, never withheld.

The artifact is a query, built at archive time the way the read facts beside it are.
A serve made inside a session is written to the recall projection together with one row of `memd_audit_session`, which names its session and window.
The archive joins the two serve relations to that relation for the session, rebuilds one line per row from the row's own columns, passes every line through the canonical table, and sorts the lines by key.
Which shard holds a serve is therefore decided by the row that names its session, never by which checkpoint happened to run next, and a serve no session made — a person running `rift recall` in a terminal — stays an ordinary projection row that no shard freezes.
A call row holds no handle and no reason, so a schema-4 line's payload names only its kind; a pack's section counts are in no row and are in no line.
The artifact is cumulative by construction: every checkpoint of a session carries every serve the session has made so far, so a second commit with nothing served since derives the same id and stays a no-op.
The trailer's prediction runs the same query through the same function, opening the projection read-only, so the serves a session has made by the time of the trailer are claimed exactly as its read facts are: a serve booked between the trailer and the condense lands in the archive and not in the claim, and the archive keeps its true id.

The usage artifact is the provider-reported spend of every model call the session has settled, frozen so a rebuild of the projection does not report zero.
It is a query too: the session's `model_call` rows joined to their `token_usage`, ordered by start time and then by call key.
Each line names the session, the turn, the call's own key, the model, the instant, the four token classes, the counting method and a `schema_version`.
The call's key travels as written rather than the coordinates it was minted from, because the projection keeps the key and not the event ordinal inside it; a rebuild re-inserts under that key.
There is no price and no dollar figure in it: dollars are derived when read, from the price catalog, which moves on its own.
The model, the method and the key come from provider fields and projection rows rather than from Rift's own grammar, so each is frozen only when it reads as an identifier, and anything else is `withheld`.
One value outside that grammar is frozen as written: `<synthetic>`, the model Claude Code reports for a synthetic assistant message, admitted as that exact literal so the call's tokens survive a rebuild; the grammar itself is not widened, and any other bracketed text is still `withheld`.
Like the serve audit it is cumulative per session, and the trailer's prediction reads it through the same function as the archive, so the usage settled by the time of the trailer is claimed exactly; a turn settled between the trailer and the condense lands in the archive and not in the claim.
Spend settles one hook after its turn (see [03-capture.md](03-capture.md) §3.9), so it reaches the shard one hook later too.

The `Rift-Checkpoint` trailer is therefore a claim, made before the commit exists, about the id its checkpoint will have.
It is exact when the session has no transcript in the shard and nothing lands between `prepare-commit-msg` and the condense.
A transcript the archive binds into the id moves it, because the prediction never reads a transcript, and so does a serve or a settled turn that arrives in between.
Nothing reads the trailer as the answer: a reader resolves a commit through the union of the checkpoints its trailer claims and the checkpoints recorded against it, the commit anchor each shard keeps of its own, so a commit whose claim missed still resolves to the checkpoint that landed.

`rift checkpoint explain` renders a shard for a human — metadata, digests, the
redacted prompt, files touched, tokens and cost, attribution — and with no
argument explains `HEAD`'s.

---

## 5.4 Leaf, root, chain

Tamper evidence is built from SHA-256 in four layers, each with a one-byte
**domain prefix** so a value from one layer is never mistaken for one from
another:

| Layer | Prefix | Construction |
|---|---|---|
| leaf | `0x00` | `SHA-256(0x00 ‖ bytes)` — one artifact, or one event line |
| internal node | `0x01` | `SHA-256(0x01 ‖ left ‖ right)` |
| empty root | `0x02` | a fixed constant for the empty leaf set |
| checkpoint root | `0x03` | binds session roots to the cost and attribution rollups |
| commit hash | `0x04` | chains a checkpoint to its predecessor |

The prefixes defend against *second-preimage* confusion: without them a leaf and
a node hash are both "SHA-256 of some bytes", and an attacker controlling content
could make one masquerade as the other. Two rules complete the tree. Odd levels
duplicate the last node, pairing a level's final node with itself. The empty set
has a fixed root rather than an error, so "this checkpoint committed to nothing"
is representable and, as §5.6 shows, checkable.

Hand-written hex vectors pin the construction, cross-checked against an
independent implementation, so any change to a prefix, the odd-leaf rule, the
empty-root constant or a layer composition fails there first — silent drift
would invalidate every checkpoint ever written.

The hash crate can also produce a Merkle **inclusion proof** — the sibling
hashes from one leaf to the root, each tagged with the side it sits on — so one
artifact and a short path prove membership without handing over the checkpoint.

### The leaf order is the shard's own file order

A checkpoint's leaves are its content artifacts **in bytewise full-path order**,
exactly the order git sorts the tree into: `content.redacted`, the four
`digest.*.json`, each line of `full.jsonl` as its own leaf,
`redaction_report.json`, `serve_audit.jsonl`, `sessions.jsonl`, `sessions/<index>/metadata.json` (bytewise by index
name, so "10" precedes "2"), `tasks/<id>/…` (bytewise by tool-use id),
`tool_calls.jsonl`, then `transcript.compact.jsonl` and `transcript.jsonl`,
`usage.jsonl`, and last `window_journal.jsonl`.

**Absence is the compatibility contract.** A checkpoint lacking an artifact
derives, leaf for leaf, the sequence it would have derived before that artifact
existed — so the transcript pair (sorting last) and the subagent subtrees
(sorting *between* `redaction_report.json` and `tool_calls.jsonl`, because
`ta` < `to`) were both addable without changing any existing root.
So was `serve_audit.jsonl`, which lands right after `redaction_report.json`
and before `sessions/`: `r` < `s` at the first byte, and again at the third.
So was `sessions.jsonl`, which lands after `serve_audit.jsonl` and before the
`sessions/` subtree, because `.` sorts before `/`.
So was `usage.jsonl`, which sorts after every other name (`t` < `u`): its one
leaf is appended to the sequence, never inserted into it, so no shard without
it derives a different root.
So was `window_journal.jsonl`, which sorts after `usage.jsonl` (`u` < `w`) and
is never written empty: a shard without it derives the root it always did.
A future
artifact must respect that rule: pick the name knowing where it lands.

Two artifacts are excluded from the leaves. `metadata.json` carries the root
itself, so a root over it is not computable, and `content_hash.txt` digests a
sequence including `metadata.json`, inheriting the circularity. That second
description was once read as "the digest of `metadata.json`" and published that
way in a format registry before anyone measured it — it matches **zero** live
checkpoints. The digest covers `metadata.json ‖ redaction_report.json ‖
full.jsonl` (the last only when the checkpoint carries one), now pinned by
arithmetic in a test.

Leaves combine pairwise in sequence, so two artifacts swapping bytes changes the
root even though the multiset of leaves does not; the tested claim is that
**changing any single artifact's bytes changes the root.**

---

## 5.5 The checkpoint's name is the binding

A checkpoint records its Merkle root in `metadata.json` — a file the same person
who edited `content.redacted` can edit: recompute the root, write it back, and
every check comparing recorded to derived root passes. That happened as a planted
attack in review, and the answer is the one binding an editor cannot rewrite by
writing a file:

> **The checkpoint's id is derived from its Merkle root, and the id is its
> path.**

The derivation is `sha256(session ‖ NUL ‖ hex(root))`, over the sessions the
checkpoint covers and the root **re-derived from the artifacts**, not the root
the file claims. Editing an artifact moves the root, which moves the id, so the
edited bytes no longer address the directory they sit in — and re-deriving a
path is not something writing a file can fix.

Three consequences, each load-bearing:

- A byte change mints a new checkpoint rather than corrupting an old one, which
  is what should happen when content genuinely differs.
- Re-archiving an unchanged session is a no-op: same bytes, same root, same id,
  same path. Capture is therefore idempotent, and the post-commit path can run
  repeatedly without inflating anything.
- `metadata.json` and `content_hash.txt` sit outside the id — they move when a
  second commit lands in the same session, and keeping them out stops that
  drift minting a spurious checkpoint.

The id binds the *root* rather than per-artifact digests because folding in one
more digest needs a new formula, a new accepted-derivation entry and a new
paragraph of published format description every time an artifact is added. The
root already commits to all of them, in order, and is already derived rather
than supplied, so a future artifact extends the leaf list and the rule holds.

---

## 5.6 Four canonicalization eras

Rift has written checkpoints for longer than it has had the machinery above and
will not rewrite what it already wrote, so each checkpoint declares in
`metadata.json` the **canonicalization era** its rules were minted under, and
the verifier selects rules by that declaration.

| Era | What it means |
|---|---|
| **0** | no era field at all — written by the tool that predates Rift, or a native checkpoint whose field was deleted |
| **1** | native redaction, but the recorded root was the empty-set constant on *every* checkpoint: nothing threaded events into the archive, so the root committed to nothing. The vacuous era. |
| **2** | the root is **derived** from the checkpoint's own leaves and cannot be supplied by a caller, so the recorded value is a claim about these bytes that the verifier re-derives. The id still bound only two artifacts. |
| **3** | the id is derived from that root, so the tree is a function of the path and every content artifact is bound by both. What Rift writes today. |

An era buys only the rules the checkpoint was written under, and four details
keep that honest. The era is read off the checkpoint, **never off a date** — not "everything after
this commit", not "everything on this branch" — so cloning, rebasing,
re-importing or restoring from a bundle cannot change what it says about
itself.

Permissiveness about the past is deliberate and asymmetric: eras up to 2 accept
both older id derivations, because refusing them would call an honest old
checkpoint tampered — a failure this project had exactly once, on real history —
while era 3 accepts *only* the root binding, since an older derivation there
would re-open the hole the era exists to close.

Relabelling downward does not buy the looser rule: the two eras' rules produce
*different ids for the same bytes*, so a checkpoint's path is evidence of the era
that minted it. A test is named for it.

An era split never silences a check that already ran — the redaction pass and
the recorded-leaf recompute run on every checkpoint of every era, and a split
that turned off an existing check would be a hole wearing a migration's name.

Era 1 has one checkable claim of its own: its recorded root **must** be the
empty-set constant, because that is what the writer of the day produced, so an
era-1 checkpoint recording anything else is flagged. That does not close the
relabelling hole — whoever rewrites the era field also writes the constant — but
it catches a checkpoint whose one invariant fails; the relabeller is caught by
the id pass, which runs on era 1 too.

---

## 5.7 The chain ref: where the order lives

A checkpoint knows its content but not its **place**, and place matters:
"checkpoint *k* was written after *k−1*" is half of what a hash chain asserts.

That order was once inferred by linking checkpoints in **sorted content hash**
order. A content hash has no relation to write time, so a new checkpoint sorted
into the middle about half the time and named a predecessor that already had a
successor; append-only forbids correcting the older row, so the projection forked
and `rift verify` called it tamper.

`refs/rift/chain/v1` is a ref whose history *is* the order:

- **one commit per checkpoint**, each one's parent being the previous entry;
- the checkpoint id rides a **commit trailer**, not a tree, so a consumer reads
  the order without fetching a single tree or blob;
- every entry's tree is the empty tree, so the ref costs one small commit per
  checkpoint and nothing else;
- the subject line is a constant, so an entry is recognizable without parsing.

Nothing rewrites a parent, nothing deletes an entry, and creating the ref refuses
if it already exists: append-only applies to a ref as to a table.

```console
$ rift chain status    # positions recorded, head entry, head checkpoint
$ rift chain init      # only on a repository that has never recorded its order
```

`init` derives the order from the checkpoint branch's own first-parent history,
the only append-stable witness. It is a verb a person types rather than
a step some routine command performs on the way elsewhere: creating an
append-only artifact is a decision, and a decision needs an author.

Neither verb linearizes anything: a merge on the checkpoint branch has no single
write order, and *inventing* one is the defect class this ref exists to end, so
such a branch is reported and refused.

After each archive the detached capture worker appends the new checkpoint's
position, and if an earlier entry never landed that append closes the tail gap
and says so once — a healed gap is a fact about a checkpoint that spent time
outside the order. Chain maintenance is **loud but never fatal**: it must not
strand capture input, nor be swallowed, since an unrecorded checkpoint is what
the next verify refuses.

---

## 5.8 What `rift verify` actually checks

Three parts, ordered because an earlier failure makes the later ones
meaningless.

### First — the order is complete, both directions

Every checkpoint on the branch has exactly one position in the chain ref, and
every position names a checkpoint on it.

| Finding | Meaning | Verdict |
|---|---|---|
| a duplicate position | one checkpoint at two positions — the mirror of the fork this ref prevents | tamper |
| the orders disagree | both sources have it, in different places: one of two append-only histories was rewritten | tamper |
| an orphan entry | a position names a checkpoint the branch does not carry — the branch was rewritten, or this chain is another repository's; nothing can heal it | tamper |
| an unchained checkpoint | a checkpoint with no position: the order has not caught up, nothing was rewritten, the next archive heals it | structure |

**Incomplete is not falsified**: collapsing the two would either cry wolf on an
ordinary lag or hide a real rewrite behind a reassuring word.

### Second — the per-checkpoint passes

For each checkpoint, in order:

1. **Redaction.** The report must exist, parse and be internally valid. A
   missing report is a redaction finding; a malformed one is structural.
2. **Recorded-leaf recompute.** If a checkpoint writes out its own leaf list —
   the older dialect's `merkle_leaf.txt` — the root over those leaves must equal
   the root it records. Runs on every era.
3. Everything below is native-only, skipped for checkpoints read from the
   pre-Rift ref. That exemption is gated on the **dialect** — which ref the
   checkpoint was read from — never on what the checkpoint says about itself:
   where the checkpoint *is* is the one thing editing it cannot rewrite.
4. **Subagent subtrees.** The subtree's directory name and the `tool_use_id` in
   its header must agree (the name is the one part of a subagent that is not
   itself a leaf). The parent's recorded token rollup is **re-summed** from the
   subagents' own leaf-bound headers — a stripped rollup hides spend, an
   invented one claims spend nothing reported.
5. **Strict Merkle** (era 2 and up). Re-derive the root from the artifacts read
   back out of git and demand a match, calling *the same function the writer
   calls*, so writer and verifier cannot drift.
6. **Vacuous root** (era 1 only). The recorded root must be the empty-set
   constant.
7. **Checkpoint id** (every native checkpoint, every era). Re-derive the accepted
   ids for the declared era and require the checkpoint's own path to be one.

A missing artifact the root or the id commits to is tamper, not
incompleteness: every checkpoint of that era writes it unconditionally, so its
absence removes bytes the checkpoint's identity depends on.

### Third — the projection agrees

The chain re-derived from git is compared position by position against the chain
in the local database; a length difference or any positional disagreement is an
integrity failure naming the first divergent checkpoint. Two guards make that
comparison mean something.

**Levelling before comparing.** Nothing writes the database row at the instant
the archive writes its checkpoint to git, so a projection is routinely *behind*
git by the checkpoints just condensed, which a naive comparison reads as tamper.
Verify levels the projection first, by the ordinary bridge derivation from the
git objects, then compares — inside verify's own entry point rather than in its
callers, because the last bug here was a caller skipping it.

**Verify may not seed the thing it is about to compare.** Filling an empty
projection from `source` and comparing it to `source` proves nothing: it made
every fresh repository "verify clean" by construction, which is how a projection
could disagree with git for months without one red run. The switch is now an
explicit value at each call site, and seeding is permitted only on the fixture
path, where the run is proving the wiring and says so.

### Verdicts and exit codes

| Exit | Verdict | Meaning |
|---|---|---|
| 0 | OK | everything checked, everything agreed |
| 1 | TAMPER | bytes on offer are not the bytes we recorded |
| 3 | REDACTION | a redaction report is missing or invalid |
| 5 | STRUCTURE | something is incomplete, not falsified |
| 70 | internal | the projection could not be opened or migrated — a bug, never a silent 0 |

A run reports its **worst** finding and never downgrades: tamper outranks
redaction outranks structure. Exit 3 is also what you get running `rift verify`
outside a repository family, where there is nothing to verify; the message names
the vantage that would fix it. In CI, `rift verify --quiet` is enough — the exit
code is authoritative on its own — and `--json` records the counts.

---

## 5.9 The verify-green streak

`rift doctor` prints a line like this:

```
  soak:           56/20 clean condensations (68/20 distinct, 56 witnessed run(s), 0 divergence(s))
```

No old implementation could write the ledger at all, so there was nothing to run
the new one beside — and "no evidence" is not an acceptable end state for that
code. The tamper-evidence machinery is therefore the oracle: N consecutive clean
verifications of *real* captures, recorded append-only, with N = 20, using the
same code `rift verify` runs, since a streak whose oracle differed from the
command you trust would certify nothing.

Four properties, each paid for by a defect:

- Every observation is recorded, clean or not, so one bad run resets the count
  to zero.
- Zero runs never clears the gate: absence of evidence is not evidence of safety.
- The population is condensations, not verifications — a bare `rift verify` you
  type contributes nothing, enforced structurally, since reaching the writer
  needs a witness value and holding one needs a named checkpoint.
- The second number counts *distinct* checkpoints inside the streak, and both
  numbers must reach the threshold: without the streak conjunct one sweep over
  twenty idle sessions clears it, without the distinct conjunct twenty
  re-condensations of one unchanged session do.

**A divergence from any source vetoes the streak, permanently.** The divergence
log is append-only, so one divergence ends that projection's streak for good and
only a rebuild from git clears it. Narrower scoping was argued in full and
rejected: a tamper finding is a fact about the *repository*, and the streak is a
claim about it. The false positives behind that argument were removed at the
source by levelling before comparing (§5.8).

`rift doctor` also consumes any pending note the detached worker left, so a
reset streak shows its reason rather than a bare `0/20`.

The streak is per repository family, not per checkout
([Chapter 9](09-worktrees-and-families.md)).

---

## 5.10 Three layers of append-only

The local database has its own discipline, enforced three independent times so
no single mistake removes it.

**The type layer.** The store trait has `insert` and `insert_or_ignore`, no
`update` and no `delete`: there is no mutating API to call.

**The SQL layer.** Every canonical table is created with `BEFORE UPDATE` and
`BEFORE DELETE` triggers raising an abort that names the table as append-only,
so raw SQL bypassing the Rust layer still cannot mutate a row. The store
classifies those aborts into a distinct error kind, letting a caller tell "the
guard fired" from "the write failed".

**The registry-driven property test.** The canonical tables are a *registry* in
code — 45 for capture, 27 for recall, 8 for the dashboard store. A test iterates
it, inserts a valid sample row into each table and asserts that both `UPDATE`
and `DELETE` are refused. **Add a table without its guard and CI goes red.**

Two rules complete the picture, documented where they live: corrections are new
superseding rows, bitemporal for cost facts
([Chapter 8](08-cost-and-savings.md)), and migrations are additive and
checksum-locked, so a destructive one cannot land at all
([Chapter 13](13-storage-and-schema.md)).

---

## 5.11 Two hashes over the same bytes

Rift computes SHA-256 where byte-compatibility with the git substrate matters —
the chain, `content_hash.txt`, the checkpoint id — and blake3 for internal
handles (worktree content hashing is plain SHA-256, so it matches the hash
capture records at read time). These are **distinct types** with no conversion
between them, proven by a test that asserts the conversion does not compile: a
blake3 handle can never masquerade as a chain hash. Git's own blob object id is
a third framing over the same bytes, bridged by an append-only mapping table —
[Chapter 6](06-recall.md) covers that bridge, handles and the delta model.

---

## 5.12 Reading older histories

Rift reads checkpoints written by the capture tool that preceded it, in both
on-disk layouts it shipped — a single sharded branch with short hex ids, and one
ref per checkpoint with time-sortable ids:

```console
$ rift import --from-legacy
```

It is **read-only** — nothing in the source repository is written, moved or
deleted, and imported checkpoints are minted read-only by construction — goes
through the ordinary write path, redaction gate included, and is idempotent, the
positions one run appends being its own import epoch.

`rift verify` verifies imported history exactly as native history. Those
checkpoints declare era 0, are read from the pre-Rift ref, and are counted and
reported separately so their exemption from native-only passes is visible. The
capability is permanent and implies nothing about what Rift writes: Rift is
native end to end and runs no other binary.

---

## 5.13 When verify is not OK

| Finding | What to do |
|---|---|
| STRUCTURE, "checkpoint(s) on the branch have no chain entry." | the order has not caught up: if this repository has recorded an order before, the next commit heals it — `rift doctor` confirms the capture queue is draining; if it never has, run `rift chain init` |
| REDACTION, "missing redaction report" | not automatically repairable — the artifact is part of what the checkpoint's own id addresses, so its absence is a real change to the checkpoint. Treat it as tamper-adjacent and investigate |
| TAMPER | something append-only was rewritten. Do not "fix" it by rebuilding: a rebuild derives the projection from git, so if git changed it agrees with the change. Work out *what* changed — the message names the checkpoint and the disagreement. If the projection drifted rather than git, deleting `projection.db` and running `rift rebuild` restores agreement safely, because the database was never the record |

`rift doctor` gives the surrounding context in one screen — sync state, ingest
watermark, queue, verify-green streak — usually enough to tell "behind" from
"broken".

---

## Related chapters

[Chapter 3 — Capture](03-capture.md) ·
[Chapter 4 — Redaction and secrets](04-redaction-and-secrets.md) ·
[Chapter 6 — Recall](06-recall.md) ·
[Chapter 13 — Storage and schema](13-storage-and-schema.md) ·
[Chapter 14 — Sync readiness](14-sync-readiness.md) ·
[Chapter 17 — Invariants](17-invariants.md)
