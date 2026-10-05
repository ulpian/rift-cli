# 9. Worktrees and families

A linked git worktree is not a second repository. It is a second *door* into one
repository: the same objects, refs, hooks and tracked policy file. Rift used to
disagree, and the disagreement was expensive — a worktree got a blank
projection, a blank recall store, no agent wiring, and no record of which
checkout a session came from.

Six words are used precisely. A **family** is one repository plus every linked
worktree of it — the checkouts sharing a git **common directory**, which holds
objects, refs and hooks (`.git` in an ordinary clone; a linked worktree's own
git directory is `<common_dir>/worktrees/<name>`). Each is a **member**, either
the **main** work tree or a **linked** one. **Adoption** wires a member for the
agents the family has enabled; **provenance** is the label saying which checkout
a captured fact came from; **containment** is the rule that a checkpoint reaches
a checkout only when its anchor commit is reachable from that checkout's `HEAD`;
**compounding** is work earned by one member accruing to all. If you never use
`git worktree` your repository is a family of one, but §9.2 and §9.8 still
apply.

---

## 9.1 The problem, as it actually presented

Rift keys its rebuildable stores by an identifier derived from a path, which was
right while one path meant one repository. `git worktree add` breaks it: the new
checkout is a different path to the same objects, refs, hooks and
`.rift/config.toml`, yet under a path rule it became a *second repository* with
an empty projection and an empty recall store.

The symptoms were not cosmetic. A checkout used for real work reported zero
checkpoints while the repository's refs held over a hundred; sessions run in
worktrees left no ledger and received no recall injection, so the savings recall
exists to produce reset to zero *exactly where the most agent work happens* —
parallel workers, and subagents a coding assistant runs in temporary worktrees.
And the shared git hooks pointed at whatever binary last ran an unrelated
command: on one machine, at a deleted temporary build for four days, and git
skips a dangling hook **silently**, so capture stopped with no error line
anywhere. Each is the same defect in a different hat: *a second identity for
work that belonged to one repository*.

---

## 9.2 One identity, keyed by the common directory

The unit of identity is the **repository family**, keyed by the git common
directory — precisely what git itself shares. Two clones have separate object
stores and refs, so their projections stay separate; two worktrees of one clone
share both, so their projections must be one.

The identity is written down once, in `<common_dir>/rift-family/`: `family.json`
holds the minted identity, `family.history.jsonl` appends every mint and repair,
`binary.json` records which `rift` the hooks point at (§9.7). `family.json` is
machine-local, never tracked, and carries a schema version, `family_id` (which
keys the capture projection), `recall_id`, the canonical common directory, the
main work tree (absent for a family with no checkout), and `minted_from`.

**Minted once, read thereafter.** After the first write nothing re-derives the
identity from a path, which makes moving a repository survivable — the manifest
travels with the git directory — and keeps path canonicalization off the hook
hot path and its hard latency budget ([03-capture.md](03-capture.md)).

**A reader never sees a torn file.** The mint writes a complete temporary,
flushes it, and *hard-links* it into place; hard linking refuses to clobber an
existing name, so a second process minting concurrently loses cleanly, adopting
the winner's identifiers within three attempts.

Every mint and repair appends an event kind, timestamp, optional reason and the
record to `family.history.jsonl`. The manifest holds no timestamp of its own: it
is a pure function of the resolved main work tree, so two mints agree byte for
byte and *when* is the history file's job. `RIFT_FAMILY_DIR` relocates the manifest
root, the manifest landing under it in a directory named by a hash of the
canonical common directory, so a test run can mint without writing into a real
`.git`.

---

## 9.3 Minting is adoption, not migration

When a family is minted from a checkout it does **not** invent new identifiers:
it adopts the main work tree's existing ones, the values the path rule already
produced. The projection and recall databases are therefore adopted *in place* —
no row moved, no migration checksum changed, no rebuild, no downtime — and the
manifest records `minted_from: adopted-main-worktree`. The alternative, a new
unified identifier plus a data move, would have copied two append-only databases
whose migration checksums are frozen
([13-storage-and-schema.md](13-storage-and-schema.md)) for no visible gain.

A bare repository has no checkout to adopt from, so it derives its identifiers
from the canonical common directory under distinct domain bytes that can never
collide with a checkout's, recording `minted_from: fresh`. The two identifier
spaces never interchange.

---

## 9.4 The main checkout is proven, never guessed

Adoption needs to know which checkout is the main one, and getting that wrong is
catastrophic and silent, so resolution is a short list of proofs with a refusal
at the end. **(a) The vantage**: the checkout asking *is itself* a main work
tree — its git directory is the common directory and a `.git` entry sits beside
the work tree, either a directory resolving back to it or a `.git` *file* whose
git directory carries no `commondir`. **(b) The family's git config**:
`core.bare = true` means no checkout, so the family takes the derived
identifiers, and `core.worktree` names one explicitly, resolved relative to the
common directory and canonicalized — both parsed directly rather than by running
`git`, since a hook cannot afford a process spawn. **(c) The parent directory**,
accepted *only if* its own `.git` canonicalizes back to that same common
directory.

If none applies, the main work tree is **unprovable from this vantage and
nothing is written**. Not a guess, not a placeholder, not a fresh identity.

That refusal is the most load-bearing decision in this chapter. A hook firing
inside a brand-new linked worktree sees a common directory reached through
`…/.git/worktrees/<name>/../..`, whose *lexical* parent is `…/.git/worktrees` —
not a checkout of anything. Minting there would have keyed the family to a
nonexistent repository and orphaned the real projection permanently, from a hook
nobody ran on purpose. Rule (c)'s canonicalization refuses it, and refusing
costs almost nothing: the next command from the main checkout mints.

Callers that own the repository they stand in may mint; library code and tests
pass a policy that forbids writing, so a read never has a write as a side
effect. Each failure arm is spelled as the refusal you need — outside a
repository nothing is created, an unprovable main work tree says *"run `rift
init` from the main work tree of this repository, which is the vantage that can
prove it"*, and an unparseable manifest reports that nothing was repaired and
names the repair command, never auto-applying it. An agent hook in any of those
states records **nothing** and still exits 0: a hook never blocks git and never
blocks an agent ([17-invariants.md](17-invariants.md)).

Repair is one explicit command, and the **only** writer of the manifest after
the first mint:

```bash
rift family remint --adopt-main --yes --reason "moved the repo and the ids drifted"
rift family remint --fresh --yes --reason "no work tree to adopt from"
```

It appends its record to the history *first* — a crash between the two leaves a
recoverable record of an intended identity, while the reverse would leave a live
identity nothing recorded — then replaces the manifest. `--yes` is required
because data filed under the previous identifiers is not moved and there is no
undo but another remint, and `rift doctor --fix` prints this command rather than
running it: an identity that can change as a side effect is not a record.

The doctor's `family_identity` leg is what tells you a remint is due. It reads
the manifest without minting, re-derives the identifiers by the rule the family
was minted by — the main work tree's path-derived identifiers when the manifest
records one, the common directory's work-tree-less identifiers when it does not —
and compares. It is silent when they match. When they differ and nothing is
stored under the derived identifiers it says `moved`: the manifest travelled with
the git directory, which is the design working, and there is nothing to do. When
a projection or recall store already exists under the derived identifiers it says
`split` — data is being filed in two places — and names both identities, both
stores and the remint command, adding that the data under the old identifiers is
not moved. A vantage that cannot prove the main work tree, or a repository with
no manifest yet, reads `unprovable` with what to run and from where; an
unparseable manifest reports the refusal above. The leg never re-mints,
`--fix` included.

---

## 9.5 What is shared, and what stays put

One identity does not mean one pile of state; the split is by *what the state
describes*. **Family state** lives in the common directory: the git hooks, the
family manifest and history, the binary of record, the queue of references
waiting to be shipped, and the converted-document sidecars of §9.10 — anything
whose value is a property of the repository rather than of one checkout. **Per-checkout progress** stays in each member's own git
directory — session ownership caches, the capture queue, the agent-event queue —
because a session lives in one checkout at a time and one worked in checkout A
must never be credited to a commit in checkout B; the shadow branch naming a
checkout's raw capture is per checkout for the same reason.
**Working-tree state** stays in the checkout's `.rift/`: your policy, agent
descriptors, the adoption marker, injection markers, manifest baselines
([02-configuration.md](02-configuration.md) has the full table).

The one exception is **the push queue**, whose reasoning is the test for
anything else that might move: a queue entry names a **reference**, and
references live in the object store the whole family already shares. Per
checkout, every consequence was a loss — a push from a linked worktree shipped
nothing, the main checkout's queue accrued unreachably (dozens of stranded
entries measured on one machine), and removing a worktree deleted its queue,
references and all. So the push queue moved while the capture and agent queues
stayed. On first access an older per-checkout queue is drained into the shared
one and removed; entries are idempotent by reference and sequence, so a failed
cleanup costs a re-read, never a double ship.

---

## 9.6 Adoption: making a new worktree work

A fresh `git worktree add` gives you a checkout with no `.rift/agents/*.json`,
no `.rift/state/native-integrations.json` and no client hook fragments, so an
agent launched there runs with **zero** Rift hooks: capture stops at the worktree
boundary and nothing says so. Adoption closes that, provisioning the member for
every agent the family has enabled, using the family's binary of record. Four
things trigger it: the `post-checkout` git hook on `git worktree add`; `rift
worktree adopt [PATH]` on demand (defaulting to the checkout you stand in);
`rift init` inside a member, which also registers the family *root* (§9.11); and
`rift doctor --fix`, across every member that needs it.

### The worktree-adoption git hook

Rift installs five git hooks; `post-checkout` is the one this chapter added.
Git runs it after `git worktree add` with the new worktree as the working
directory and — measured on git 2.50.1 — with `GIT_DIR` **unset**, so the
forwarded git-directory argument is a relative `.git` that in a linked member
names a *file*. The handler therefore resolves its member from the working
directory, never from that value.

The hook's shape is its latency budget spelled out. The third argument — `1` for
a branch checkout, `0` for a file checkout — is read before anything touches the
disk, and each later exit costs a discovery plus a `stat`: a file checkout, an
ordinary `git switch` in the main checkout, an already-provisioned member, or a
member whose previous adoption left a failure marker all return without
adopting. Nothing on that fast path opens a projection, spawns `git`, or
resolves the effective hooks directory, and what remains is published as a
measured harness row: a `git switch` in a provisioned member measured **35.05
ms** median and **37.32 ms** at the 95th percentile against a 100 ms
git-latency bar, and `git worktree add` with the hook installed measured
**69.86 ms** and **77.56 ms**.

If the hook cannot finish it writes `.rift/state/adoption.json` —
`attempted_at`, `outcome`, `reason` — and leaves a note in that member's capture
queue, phrased act-first (*"`rift worktree adopt` or `rift doctor --fix` retries
it"*) because the note is truncated and the sentence that has to survive is the
one you can act on. The hook never fails your `git worktree add`: a panic inside
it is swallowed and the exit code stays 0. While a failed marker stands the hook
does not retry, since paying for a whole adoption on every branch switch to fail
the same way is the cost the budget has no room for, until `rift worktree adopt`
or `rift doctor --fix` clears it. A member the family has *nothing* to wire gets
**no** marker: an empty intent is a success with nothing to do, and a marker
there would read to the doctor as a refusal.

### What adoption does, and the four things it refuses

For each enabled agent, adoption writes `.rift/agents/<agent>.json`, refreshes
`.rift/state/native-integrations.json`, and writes the client hook fragments
through the shared owned-region emitter; running it twice changes nothing.
Almost every other rule is a refusal:

- **The family's intent is read read-only.** The enabled set is the projection's
  append-only enablement relation, folded newest-first per agent, opened
  *read-only* and never through the migrating open, because this code runs from
  a git hook and a hook that migrates a projection is a failure paid for once. A
  missing or unreadable projection is **unknown**, never "the family wants
  nothing", since that would silently un-wire a member on one bad read: unknown
  defers, writes the marker with the reason, and leaves it to the doctor.
- **Descriptors name the binary of record, never the running one** (§9.7).
- **A tracked file is never rewritten as a side effect.** Tracked hook bundles
  and managed skill regions are *verified and reported*; writing them stays with
  the explicit `rift enable <agent>`, because a git hook that dirtied the tree
  you just created is not a repair.
- **Global client configuration is read, never written**: a member's fresh
  `.codex/hooks.json` is untrusted until you approve it *in Codex*, and adoption
  reports Codex's trust hook by hook (`codex trust: trusted`, `error` with the
  remedy, or `unverifiable`)
  ([10-agent-integrations.md](10-agent-integrations.md)).

Adoption never touches the git hooks and never registers the member as a project
of its own, and a refusal from one agent's installer — a fragment Rift does not
own, or one you edited — is named in the report without aborting the rest.

`rift enable <agent>` records the family's intent, which is family-wide, so it
also wires every *other* member that exists now; `rift disable` unwires them the
same way. The same three rules hold — binary of record, a refusing member named
rather than overwritten, tracked files and git hooks untouched — and each
member's outcome is one report line.

---

## 9.7 The binary of record, and hooks that stay put

Git resolves `hooks/<name>` through the common directory, so **one install
covers every member** — and the flip side was that any member's `rift init`
re-pointed *every* member's hooks at whatever binary that process happened to
be. That is how a family ended up on a deleted temporary build, silently, from a
command that was not about hooks at all. Two rules close it.

**The default install never re-points a healthy family.** Every install that is
a *side effect* of something else — `rift init`, `rift enable`, Project
Settings, adoption — uses a keep-if-healthy policy: if every managed hook and
its target exist and the target is executable, the hooks are left alone and the
recorded binary reported. Only a dangling link or a vanished binary causes a
repair, printed, never silent. A managed link whose sidecar marker names its own
target counts as managed **even when that target no longer exists**, so it is
healed in place rather than chained aside as a stranger's hook, and a user's own
hook is preserved. The effective hooks directory is resolved through git itself
at every install site and at the health probe, because `core.hooksPath` and
per-worktree config can leave `<common_dir>/hooks` reading healthy while git
runs elsewhere.

**Explicit replacement is a separate verb**,
`rift hooks install --repoint --binary /usr/local/bin/rift`, refused when the
canonicalized new path is **transient**: `temp-root`, a temporary root the
system empties (`$TMPDIR`, `/tmp`, `/private/tmp`, `/var/folders` and their
`/private` twins, matched after canonicalization); `build-artifact`, a `debug/`
or `release/` profile's `deps/` or `build/` executable, which the next `cargo
test` replaces; or `linked-member`, a binary inside a *linked* member's work
tree, which `git worktree remove` deletes — the main checkout's `target/` is
deliberately allowed, being the binary you are developing with. A refusal writes
nothing at all and says so in one sentence, naming the rule and offering
`--allow-transient`; the same screen applies to a self-heal target.

**`binary.json`** turns "whichever process ran last" into a fact with a history:
the absolute path, when it was recorded, and which act recorded it (`init`,
`hooks-install`, `self-heal`, `repoint`), with superseded records behind it,
capped at sixteen. Recording the path already of record changes nothing, so
`rift init` twice leaves one record, and a garbled record is not an install
failure — the next replaces it, and `rift doctor`'s binary-of-record leg names
the file.

---

## 9.8 Provenance: which checkout did this?

Once a family shares one projection, "which worktree did this session, this
transcript, this checkpoint come from?" needs a durable answer — two of them,
for two different questions.

### A session's worktree is an observation, not an attribute

A coding assistant can run an in-process subagent under the *parent's* session
id, and a subagent isolated in its own worktree runs its tools in a different
checkout — measured, with the worktree removed seconds after it finished. So one
session id can be observed in several checkouts over its life, and a checkout
can disappear while the session continues. The record is therefore an **append-only relation
of observations**, one row per observed *state*.

Each row carries an `observation_id` — a content address over the observed state
(session, token, branch, head, subagent), every field NUL-terminated and absence
encoded explicitly so `(branch="a", head=absent)` and `(branch=absent,
head="a")` cannot collide, and taken over the *state* rather than the time, so a
repeat is an ignore-on-conflict insert. Beside it: `session_id`;
`worktree_token`, twelve lowercase hex of the path digest; `worktree_name` and
`is_linked`; `work_tree`, an absolute path and therefore a **local address**,
which is why this relation is projection-only — paths never enter shards;
`branch` and `head_oid`, nullable because detached or unreadable is *absent*,
never an empty string; `subagent_id`; `basis`; and `first_observed_at`.

`basis` is a closed vocabulary: `payload_cwd` (the agent's own payload carried a
working directory), `workspace_path` (an adapter sending a workspace path
instead), `process_cwd` (the hook process's own directory), and `commit`,
`attach`, `import` for the non-hook writers. The payload's directory wins when
the adapter carries one — that is the agent speaking about itself, while the
hook process's directory is wherever the shell that spawned it happened to be —
but only when it resolves to a checkout of the *same* family. A payload naming a
directory in another repository falls back to the discovered repository with
`process_cwd`: filing one family's session under another family's worktree is
exactly the mis-credit the per-checkout discriminator forbids, and a basis token
that lied about where the value came from would hide it.
One thing outranks the payload's directory: a shell read the grammar recognises whose leading `cd` lands in a different member of the same family is filed under that member with the basis `shell_cd`, because the payload says where the agent's shell sat and the `cd` says where it read.
Those observations live in a second relation, `session_worktree_cd`, whose CHECK admits the old vocabulary plus `shell_cd` — a CHECK changes only by rebuilding its table, so the wider vocabulary is a new table — and every reader reads the union of the two.

Every agent hook observes; the ingest keeps the last seen triple in the session
cache and skips the insert when unchanged, so the hot path pays one comparison.
A session's **current** checkout is its newest observation, its **history** the
ordered set, and nothing is ever updated.

### A checkpoint carries the checkout it was archived in

Shard metadata gains an absent-when-unknown member —
`"worktree": { "name": "feature-a", "token": "0a1b2c3d4e5f", "linked": true,
"branch": "feature/parser" }` — plus, for shards covering several sessions, a
`session_worktrees` map for the sessions whose last observation differs from the
archiving checkout. **No absolute path and no username**: this member crosses
machines in bundles, while the local relation above keeps the address. It is not
a Merkle leaf, so adding it changed no id derivation and no verification rule,
which is why no canon version was bumped.

The semantics are narrower than they look: **the member names the checkout of
the first archive that minted the shard**, and re-archiving byte-identical
content from a different checkout keeps that first label — which is why every
surface prints it as `archived_in` and never as "worked in", the moving-session
question being answered by the observations.

A rebuildable projection of that member — checkpoint id, token, name, linked,
branch — is written by the artifact bridge, and a shard without the member
projects **no row**, so a rebuild reproduces exactly what the bridge wrote; the
recall projection gets its own twin, because that store never joins across
databases. Sessions and shards from before the label have no observation and no
member, so every surface prints `unknown`, never `main`: back-filling sealed
shards is forbidden.

Every surface printing a session or checkpoint carries the label — `rift session
list|info|show`, `rift checkpoint explain|show`, `rift why`, `rift blame`, `rift
dispatch`, `rift recap`, `rift review`, and the dashboard's session rows and
checkpoint catalogue. Transcripts are labelled *through* their shard, with no
per-line marker, because transcript lines must stay byte-stable, and no commit
trailer, because trailers are claims in public history and the checkpoint a
trailer names already carries the member.

---

## 9.9 Containment: one store, but not one world

Sharing a store creates a hazard separate stores hid by accident. A known-state
answer served in a checkout on `feature/x` must not present a checkpoint minted
on `main`, three commits ahead, as that checkout's last known state: the files
it holds are not the files that changed. While every checkout had its own store,
"one database = one repository = one branch line" held by accident and the
branch axis was a no-op; one store per family ends the accident, so that axis
had to become real.

There is exactly one place a recall query can bound itself, and containment
lives there — after the developer filter, before the last-N window. A checkpoint
whose **anchor commit** is reachable from the caller's `HEAD` (or equal to it)
is **in scope**; one with no anchor commit, one outside that set, or **any**
checkpoint when `HEAD` is unknown or the walk hit its cap, is **out of scope**
and counted, unless an explicit grant widens it and the reason is audited:
**never widen on uncertainty.** Two recall-side relations make that possible
without joining across databases — one mapping each checkpoint to its anchor
commit, one to the checkout it was archived in — both written by the recall
ingest from the same shard metadata.

Reachability comes from a read-only walk over the commit graph, behind a small
trait so the containment tests can drive it with an in-memory table. Three
properties the serving path depends on: it is asked **once per serve** and
memoized for the life of the oracle (no verdict is persisted); it is **capped at
50,000 commits**, past which the verdict is
*unknown* — exclude and report, because a serve must not spend an unbounded
slice of a hook's budget on containment; and it is **narrow when it cannot
see**, since a commit whose object is missing in a shallow or partial clone
contributes no parents rather than aborting the walk, so the set comes out
*smaller* than the true ancestry. The walk is breadth-first over **every**
parent, the question `git merge-base --is-ancestor` answers: a first-parent-only
walk would call a merged-in branch's commits foreign to the branch that merged
them, deleting exactly the compounding a shared store exists to produce.

An enumerating response carries a containment block —
`"containment": { "out_of_branch": 1, "unknown_head": false,
"cross_branch_grant": false }` — where `out_of_branch` counts what the branch
axis excluded **before** the response's own size cap, `unknown_head` says the
axis could not be decided *at all* (a different sentence from "there was
nothing"), and `cross_branch_grant` says a grant widened this serve. The block
is **absent when there is nothing to say**: it costs real tokens on every
manifest, and three zeroes buy an agent nothing.

Crossing a branch on purpose is `--cross-branch <REASON>`: policy-gated, a
non-empty reason required, and never available to the hook injector, because no
argument on that path could widen it
([07-serving-and-injection.md](07-serving-and-injection.md)).

Some things are inherently per checkout. Validity is not shared and must not
be: the "unchanged since you last saw it" verdict compares the caller's known
hash against the **current checkout's file bytes**, and branch content differs
([06-recall.md](06-recall.md)). Injection markers, manifest baselines and
turn-boundary snapshots are per session and per checkout. So is the record of
what a context window holds
([§6.12](06-recall.md#612-what-a-context-window-already-holds)) — twice over:
the bytes it names differ per tree, and a subagent running in its own worktree
under the parent's session id is a *different window* with its own file. That
is the case a shared record would get wrong in the most expensive direction,
by declining a read of lines only the parent ever received.

### Injection gets a second level

With a shared store, "the recall store's file moved" no longer means "moved for
me": a sibling landing facts on a divergent branch changes the file and changes
nothing this member may be told, and injecting on that would bill the member for
a turn it learns nothing from. So the decision is two-level — the file stamp
first, then a **scoped stamp** over this member's *in-scope* checkpoint set and
its highest ordinal, computed through the very same chokepoint the manifest
would be built from, with the file stamp re-armed afterwards. A sibling landing
facts on the member's own ancestry does re-inject, which is the compounding you
asked for; a sibling on a fork does not, which closes the double-billing hazard
([07-serving-and-injection.md](07-serving-and-injection.md)).

---

## 9.10 Compounding: what a shared store buys

The requirement behind all of this was one sentence: *ensure the benefits of
recall are shared so that savings compound; worktrees are often used by
subagents, and this adds to the token savings.* Once the store is shared, these
compound by construction: read facts, token counts, blob-to-object maps and
changed-hunk indexes, each one row per *content* served to every member;
converted documents, keyed by the document's own bytes so a spreadsheet
converted once in any member is read as Markdown in all of them;
validity bindings and digests, so a summary computed in checkout A is served in
checkout B on the same bytes and counted as avoided; checkpoint lineage,
ingested once per family on one watermark; packs, whose input-keyed keys let
identical inputs from two members hit one cache entry; and accounting, since
serve audit, call and session events land in one relation. One subtlety had to be fixed for pack caching to
work at all: the in-focus file list is normalised to repository-relative paths
before it enters a pack key, or one member's absolute path would defeat every
other member's cache hit.

Documents show the mechanism at its plainest. The sidecar is written once into
the family's store under a key made of the document's bytes, the converter's
version and the caps that shape its output, and each member sees it as a
**hardlink** in its own state directory — one inode, no second conversion, and
a copy only where a filesystem refuses the link. A temporary worktree's view
disappears with the worktree while the family's answer stays, which is exactly
the direction that pays: the throwaway member costs nothing and leaves the work
behind. An unused sidecar is collected on its own clock, and a view whose family
answer is gone is collected by that member as an orphan.

A subagent's hooks fire in its own checkout's working directory under the
parent's session id: discovery resolves the family, the observation labels the
subagent's checkout, and the scope serves it the family's deltas on its branch.
When the worktree is removed before the detached worker runs, the queued
condense degrades to commit-only (§9.13), and savings already booked are not
lost because the ledger is the family's. One caveat: an assistant that cuts subagent worktrees from the
repository's **default branch** gives you subagents on that branch's tip, so a
parent working on a feature branch does not compound its own feature checkpoints
into them unless the assistant is configured to cut from the current head
instead. `rift doctor` advises the setting when the family has that assistant
enabled and the key is absent, and never writes it, because that is a tracked
file.

### It is measured, not asserted

A savings claim has to be a deterministic fixture through the real binary, so
compounding is one: a family whose main checkout captures and whose two linked
members capture nothing, one member cut from the tip and one forked off an
earlier commit, so the out-of-branch count is a *measurement* rather than a
fixture constant. Six assertions, one test each, establish one store per family, a sibling's
summary and delta served to a member, the forked member refused the other line
with `out_of_branch: 1`, a granted cross-branch serve carrying the neutral tag
with the reason in its own relation, and a family total equal to the sum of what
its members avoided.

The published harness row measures one serve in a linked member out of the
family's shared store — a member that captured none of those reads itself — at
**1,383 tokens avoided**, 118 ms median over three iterations. A store per
checkout would hold nothing that member captured, so there is no second figure
to divide by: the number itself is the compounding
([15-benchmarks.md](15-benchmarks.md)).

> **Honest limit.** The second assertion proves the *serving* half only. Its
> binding is planted at the seam the future ingest will use, because the writer
> that computes bindings in production does not exist yet — no shipped path
> calls it, and known-state passes no summary today. Served-once-computed-once
> across members is proven; *computing* the summary automatically is not
> ([18-known-gaps.md](18-known-gaps.md)).

---

## 9.11 The verbs

The operator's vocabulary grows by one noun, not a new workflow: `worktree` sits
beside `session`, `projects` and `doctor`, answering the same three questions —
what is there, what state is one of them in, what can be swept.

**`rift worktree list`** is read-only: one row per member with its kind, branch,
provisioning state, live session count and last-capture stamp, a `!` on a member
whose checkout is gone, and a summary line counting members, prunable entries
and linked members not adopted. The last-capture column joins the archived-in
projection to the checkpoint table in **one** read for all members; a member
with no dateable archive reads `never`, and so does an unreadable store — never
a fabricated instant.

**`rift worktree status [PATH]`** shows one member in full: its row, the client
wiring Rift can verify *without writing anything* (per agent: where its hooks
point, whether a recall skill is present, and Codex's per-hook trust: `codex: trust trusted`, `error` with the remedy, or `unverifiable`),
and its observations. An agent with no hook fragment produces no line, because
the row's `not adopted` already says the member is unwired.

**`rift worktree adopt [PATH]`** is §9.6, idempotent, never touching git hooks,
tracked files or global client configuration. **`rift worktree prune [--yes]`**
reports members whose checkout is gone while their administrative entry
survives; **without `--yes` nothing is executed**, because removing an entry is
your decision, and with `--yes` it runs `git worktree prune` through the
argument-array seam — explicit environment, no shell, no user-supplied string in
the argument list. **`rift family remint`** (§9.4) is the only writer of the
manifest after the first mint, and **`rift projects list --worktrees`** lists
each repository's members indented under it.

Registering a path resolves its family first. If a registered row already shares
the family, the family **root** arriving over a member's row **supersedes** it —
a remove event and an add event, no deletion, because the log is append-only.
Anything else is **refused** — one row per repository, a linked worktree listed
under it rather than registered on its own — as an error rather than a silent
success, because a quiet no-op would have told you your worktree was registered
when it was not. `rift init` inside a member registers the **root**, adopts the
member, and says so.

`rift doctor`'s session sweep, queue legs and memory-chain leg enumerate every
member's git directory, and `--fix` drains and adopts across members. Its
family-related legs are `phantom_gitdirs` and `orphan_projections` (§9.12),
`family_identity` (§9.4), `push_queue`, `prunable_worktrees`, `hooks_target` (a
dangling link `--fix` self-heals with a record), `binary_of_record`, and
`worktree_wiring`.

In the dashboard, members are child rows of their repository with status derived
from **family** data rather than a projection that will never fill, and token
and savings figures show the **family total by default**, decomposable by
worktree, with an `unknown` row for sessions never observed and a separate
family-only remainder for facts carrying no session key. `/worktrees` reads,
`/adopt` mutates and therefore confirms, and `A` on an unadopted member is the
same act by key ([11-canopy-tour.md](11-canopy-tour.md)).

---

## 9.12 Two kinds of debris

**Phantom git directories.** Discovery accepts a `.git` **directory** only when
it really is one: it must contain `HEAD`, plus either `objects/` or `commondir`.
Anything else named `.git` is skipped and the walk continues upward, while a
`.git` *file* is parsed as before, its git directory named by the file and never
guessed. Without the rule a bare `mkdir .git` became a repository with an identity of
its own, and one tree ended up with five. Now such a hook records **nothing**
(exit 0, no session cache, no projection), and `rift doctor` lists the phantoms
it finds without deleting them.

**Orphan projections.** Before the family mint a checkout's projection was keyed
by *its* path, so one repository in seven checkouts had seven databases. Those
strays are reported, never merged and never silently deleted:

```bash
rift projects prune --orphan-projections            # reports path, id, event count
rift projects prune --orphan-projections --yes      # removes them
```

Merging is impossible — append-only rows cannot move — and the facts inside one
that exist nowhere else (per-turn usage, the serve audit) are not recoverable
into the family store, which is why removal asks for the word. The event count
makes the line actionable: an orphan with nothing in it is a file, one with tens
of thousands of events is a decision. The enumeration re-derives the *retired*
path rule — the one place a product path may — and excludes the family's own
database **by identity, not by name**, shared by the doctor leg and the prune
verb so they cannot disagree about what an orphan is.

---

## 9.13 Removing a worktree

`git worktree remove` deletes the member's checkout state and its git directory;
nothing Rift holds there is authoritative — caches, and queues the family either
drains or degrades honestly. The interesting case is a commit captured in a
checkout gone before the detached worker gets to it, typically a subagent's. The
condense degrades to **commit-only**, because that is what survives: the commit
object is in the family's shared store, so its diff, file revisions and hunks
stay derivable through any surviving member, while the tree does not.

And **no checkpoint is archived**: archiving from a surviving member would write
a shard under *that* member's token for a session that lived in another one, the
exact mis-credit the discriminator forbids. So an archive-decision row records
`worktree-missing:commit-only` with `archived = false` — the gap becomes a row
instead of a silence, and the attribution replay lowers its own confidence from
what the row's siblings do and do not hold. `rift worktree prune` (§9.11)
retires the administrative entry afterwards.

---

## 9.14 Edges worth knowing

- **A moved worktree keeps its family but gets a new token**, and its provenance
  rows are new rows: nothing is rewritten.
- **`rift-memd` never mints.** It refuses with a message when a family has no
  recorded identity: minting is a product act, and that binary projects.
- **Adoption cannot grant Codex trust**, and does not try — that approval is
  yours to give in Codex.
- **Identity drift is reported, never repaired.** The doctor's
  `family_identity` leg compares the recorded identifiers against what the
  current main checkout derives and calls the result `matches`, `moved`, `split`
  or `unprovable` (§9.4). Only `split` needs you: `rift family remint
  --adopt-main` is the repair, run by hand, and the history file is the audit
  trail.
- **The dead-schema table for raw shadow branches is still empty** and has no
  writer; §9.8's two relations are the provenance. Nothing on the dashboard
  reads them any more ([18-known-gaps.md](18-known-gaps.md)).

---

## Related chapters

[02-configuration.md](02-configuration.md) ·
[03-capture.md](03-capture.md) ·
[06-recall.md](06-recall.md) ·
[07-serving-and-injection.md](07-serving-and-injection.md) ·
[11-canopy-tour.md](11-canopy-tour.md) ·
[13-storage-and-schema.md](13-storage-and-schema.md) ·
[15-benchmarks.md](15-benchmarks.md) ·
[17-invariants.md](17-invariants.md) ·
[18-known-gaps.md](18-known-gaps.md) ·
[10-agent-integrations.md](10-agent-integrations.md)
