# 9. Worktrees and families

A linked git worktree is not a second repository. It is a second *door* into one
repository: the same objects, refs, hooks and tracked policy file. A worktree
therefore shares the projection, the recall store and the agent wiring of its
repository, and every captured fact says which checkout it came from.

A **family** is one repository plus every linked worktree of it, sharing a git **common
directory**; each is a **member**, the **main** work tree or a **linked** one. **Adoption** wires a
member for the agents the family enabled; **provenance** labels which checkout a fact came from;
**containment** admits a checkpoint only when its anchor commit is reachable from the checkout's
`HEAD`; **compounding** is one member's work accruing to all.

---

## 9.1 The problem a path-keyed store has

A path-derived identifier is right while one path means one repository. `git worktree add` breaks
that: the new path reaches the same objects, refs, hooks and `.rift/config.toml`, yet a path rule
makes it a *second repository* with an empty projection and recall store, so savings reset to zero
exactly where the most agent work happens. The shared git hooks have the same shape: any member
could point all of them at whatever binary it happened to be, and git skips a dangling hook
**silently**. Each is *a second identity for work that belongs to one repository*.

---

## 9.2 One identity, keyed by the common directory

The identity is keyed by the git common directory — what git itself shares. Two
clones have separate stores and stay separate; two worktrees of one clone share both,
so their projection is one. It is written once in `<common_dir>/rift-family/`:
`family.json` (machine-local, never tracked: `family_id` which keys the capture
projection, `recall_id`, the canonical common directory, the main work tree, and
`minted_from`), `family.history.jsonl` (every mint and repair) and `binary.json`
(§9.7).

**Minted once, read thereafter.** Nothing re-derives the identity from a path afterwards, so
moving a repository is survivable (the manifest travels with the git directory) and path
canonicalization stays off the hook hot path. The manifest holds no timestamp — a pure function of the resolved main work tree, two mints agree byte for byte; *when* is the history
file's job. Minting is one chokepoint in `rift-git`'s `family` module, so a second call site cannot
fork a family; `rift-memd` never mints, and a concurrent minter loses cleanly because the mint
*hard-links* a complete temporary into place, which refuses to clobber an existing name.

---

## 9.3 Minting is adoption, not migration

A family minted from a checkout adopts the main work tree's existing identifiers
(`minted_from: adopted-main-worktree`), so the projection and recall databases are
adopted *in place* — no row moved, no migration checksum changed, no rebuild. A new
unified identifier plus a data move would copy two append-only databases whose
checksums are frozen ([13-storage-and-schema.md](13-storage-and-schema.md)) for no
gain. A bare repository derives identifiers from the common directory under distinct domain bytes
(`minted_from: fresh`).

---

## 9.4 The main checkout is proven, never guessed

Getting the main checkout wrong is catastrophic and silent, so resolution is a short list of proofs with a refusal at the end: **(a)** the asking checkout *is itself* a
main work tree; **(b)** the family's git config says so (`core.bare` means no
checkout, `core.worktree` names one), parsed directly because a hook cannot afford a
`git` spawn; **(c)** the parent directory, accepted *only if* its own `.git`
canonicalizes back to the same common directory.

If none applies, the main work tree is **unprovable from this vantage and nothing is
written** — no guess, no placeholder. A hook inside a brand-new linked worktree reaches the common directory through
`…/.git/worktrees/<name>/../..`, whose lexical parent is not a checkout; minting there would key the
family to a nonexistent repository and orphan the real projection. Refusing
costs almost nothing: the next command from the main checkout mints. A hook that
cannot resolve a family records **nothing** and exits 0 — a hook never blocks git or
an agent ([17-invariants.md](17-invariants.md)).

Repair is one explicit command, the **only** manifest writer after the first mint:

```bash
rift family remint --adopt-main --yes --reason "moved the repo and the ids drifted"
rift family remint --fresh --yes --reason "no work tree to adopt from"
```

It appends to the history *first*: a crash between the two steps leaves a recoverable
record of an intended identity, where the reverse would leave a live identity nothing
recorded. `--yes` is required because data under the previous identifiers is not moved
and the only undo is another remint; `rift doctor --fix` prints the command rather than
running it, since an identity that can change as a side effect is not a record. The doctor's
`family_identity` leg reports `moved` (the manifest travelled; nothing to do), `split` (a store
exists under both identities; the only state that needs you) or `unprovable`.

---

## 9.5 What is shared, and what stays put

The split is by *what the state describes*. **Family state** lives in the common directory: git
hooks, manifest and history, the binary of record, the queue of references waiting to be shipped,
and the document sidecars. **Per-checkout progress** lives under the family's cache directory
at `run/<member>/` (session ownership caches and the capture and agent-event queues), because a
session lives in one checkout at a time and work in A must never be credited to a commit in B. **Working-tree state**
stays in the checkout's `.rift/` ([02-configuration.md](02-configuration.md)), and the read gate
finds sibling members from git's own files
([07-serving-and-injection.md §7.4](07-serving-and-injection.md#a-read-another-window-holds)).

Run-state readers use the new directory first and the unmoved legacy directory
for missing names. Queue migration and sweeps require the matching checkout owner
and a ready migration. Conflicting tickets remain visible debt. Private queue
publication stages remain as evidence of unfinished publication and are never
deleted merely for being old. File contents are synced, but directory updates
are not guaranteed to survive power loss.

**The push queue** is the exception, and its reasoning is the test for anything else that might
move: an entry names a **reference**, and references live in the object store the family shares.
Per checkout every consequence was a loss — a push from a linked worktree shipped nothing, and
removing a worktree deleted its queue. An older per-checkout queue is drained into the shared one on
first access; entries are idempotent, so a failed cleanup costs a re-read, never a double ship.

---

## 9.6 Adoption: making a new worktree work

A fresh `git worktree add` has no agent descriptors or client hook fragments, so an agent there runs
with **zero** Rift hooks and nothing says so. Adoption provisions the member for every agent the
family enabled, using the binary of record, on the `post-checkout` hook, `rift worktree adopt
[PATH]`, `rift init` inside a member (which registers the family *root*, §9.11) and `rift doctor
--fix`. The hook exits
early when there is nothing to wire, stays inside the git-latency budget
([03-capture.md §3.8](03-capture.md#38-the-two-latency-budgets)), and on failure writes a
marker (`.rift/state/adoption.json`) that stops retries until `adopt` or `doctor --fix`
clears it; it never fails your `git worktree add`, and a member with nothing to wire gets no marker.

Adoption refuses where a side effect would be a harm. **The family's intent is read read-only**,
never through the migrating open (a hook that migrates a projection is a failure paid for once),
and a missing or unreadable projection is **unknown**, never "the family wants nothing", which
would silently un-wire a member on one bad read. **Descriptors name the binary of record, never the running one** (§9.7). **A tracked file is never rewritten as a side effect**: tracked hook
bundles and skill regions are *verified and reported*, and writing stays with the explicit `rift
enable <agent>`, because a git hook that dirtied the tree you just created is not a repair.
**Global client configuration is read, never written**: a fresh `.codex/hooks.json` is untrusted
until you approve it *in Codex* ([10-agent-integrations.md](10-agent-integrations.md)). One agent's refusal is named without aborting the rest, and `rift enable`/`disable` record
family-wide intent, so they wire and unwire every existing member under the same rules.

---

## 9.7 The binary of record, and hooks that stay put

Git resolves `hooks/<name>` through the common directory, so **one install covers every
member** — and any member's `rift init` could re-point *all* of them at a temporary build
later deleted.

**The default install never re-points a healthy family.** Every install that is a *side
effect* (`rift init`, `rift enable`, Project Settings, adoption) keeps the hooks when every
managed hook and its target exist and the target is executable; only a dangling link or a
vanished binary causes a repair, printed, never silent. A managed link counts as managed **even when its target is gone**, so it is healed in place, not
chained aside as a stranger's hook. 

**Replacement is a separate verb**, `rift hooks install --repoint --binary PATH`, refused when the
path is **transient**: `temp-root` (a root the system empties), `build-artifact` (a profile's
`deps/` or `build/` executable, replaced by the next `cargo test`) or `linked-member` (inside a
linked member's tree, which `git worktree remove` deletes; the main checkout's `target/` is
allowed). A refusal writes nothing and offers `--allow-transient`. `binary.json` records the path
and which act recorded it, with sixteen superseded records behind it, so "whichever process ran
last" is a fact with a history.

---

## 9.8 Provenance: which checkout did this?

**A session's worktree is an observation, not an attribute.** A subagent runs under the *parent's*
session id, possibly in a worktree removed seconds after it finishes, so one session is observed
in several checkouts. The record is an **append-only relation of observations**, one row per
observed *state*, its `observation_id` content-addressed over the state rather than the time, so a
repeat is an ignore-on-conflict insert. `work_tree` is an absolute path and so a **local address** that never enters a shard; a detached or
unreadable `branch` or `head_oid` is *absent*, never an empty string. A session's **current**
checkout is its newest observation, its **history** the ordered set.

`basis` says where the value came from (`payload_cwd`, `process_cwd`, `commit`, …). The payload's directory wins — it is the agent speaking about itself — but
only when it resolves to a checkout of the *same* family; otherwise `process_cwd` of the discovered
repository is used, because filing a session under another family's worktree is the mis-credit the
discriminator forbids and a basis that lied would hide it. A recognised shell read whose leading `cd` lands in another member outranks the payload (basis
`shell_cd`, a second relation `session_worktree_cd` because a CHECK changes only by rebuilding its
table; readers read the union).

**A checkpoint carries the checkout it was archived in.** Shard metadata has an absent-when-unknown
`"worktree"` member (name, token, linked, branch), **never an absolute path or username**, since it
crosses machines in bundles; it is not a Merkle leaf, so it moves no id and no verification rule. It
names the checkout of the **first archive that minted the shard**, which is why every surface prints
`archived_in`, never "worked in". A shard without it projects **no row**, so a rebuild reproduces what the bridge wrote; unlabelled ones print `unknown`, never `main`, and back-filling sealed shards is forbidden. Transcripts are labelled *through* their shard, with no per-line marker and no commit trailer.

---

## 9.9 Containment: one store, but not one world

Sharing a store creates a hazard separate stores hid: a checkpoint minted on `main`, three commits ahead, must not be presented as the last known state of a checkout on `feature/x`.

Containment lives at the one place a recall query bounds itself, after the developer filter and
before the last-N window. A checkpoint whose **anchor commit** is reachable from the caller's
`HEAD` (or equal) is **in scope**; one with no anchor commit, one outside, or **any** checkpoint
when `HEAD` is unknown or reachability cannot be decided is **out of scope** and counted, unless
an explicit grant widens it and the reason is audited: **never widen on uncertainty.**
Reachability is a read-only commit-graph walk asked **once per serve**, memoized and never
persisted, bounded at 50,000 commits (`ANCESTRY_WALK_CAP` in `rift-recall-types/src/scope.rs`) so
a serve cannot spend a hook's budget on history; past the bound the verdict is unknown. It is
**narrow when it cannot see** (a commit missing in a shallow clone contributes no parents) and
follows **every** parent, as `git merge-base --is-ancestor` does: a first-parent-only walk would
call a merged-in branch's commits foreign to the branch that merged them, deleting the
compounding a shared store exists to produce.

An enumerating response carries `"containment": { "out_of_branch", "unknown_head",
"cross_branch_grant" }`, absent when there is nothing to say (three zeroes cost tokens on every
manifest); `unknown_head` means the axis could not decide *at all*, a different sentence from
"there was nothing". `--cross-branch <REASON>` is policy-gated, needs a reason, and is never
available to the hook injector ([07-serving-and-injection.md](07-serving-and-injection.md)).

Validity and what a context window holds stay per checkout ([06-recall.md](06-recall.md)); a subagent
in its own worktree is a *different window*. Injection is decided at two levels, the file stamp and
one scoped to the member's in-scope checkpoints, so a sibling's divergent branch never bills a turn
that teaches nothing ([07-serving-and-injection.md](07-serving-and-injection.md)).

---

## 9.10 Compounding: what a shared store buys

With one store per family these compound by construction: read facts, token counts and hunk
indexes (one row per *content*); converted documents; validity bindings, so a summary computed in A
is served in B on the same bytes and counted as avoided; checkpoint lineage; and accounting.

A converted document is stored once per family and each member sees a **hardlink**; a temporary
worktree's view disappears with it while the answer stays.

A subagent's hooks fire in its own checkout under the parent's session id, so the scope serves it
the family's deltas on its branch. If its worktree is removed before the detached worker runs, the
condense degrades to commit-only (§9.13) and savings already booked stay, because the ledger is
the family's. An assistant that cuts subagent worktrees from the **default branch** gives subagents that tip, so a
parent on a feature branch does not compound into them; `rift doctor` advises the setting.

[15-benchmarks.md](15-benchmarks.md) measures it.

---

## 9.11 The verbs

`rift worktree list` and `status [PATH]` are read-only (a member with no dateable archive reads
`never`, never a fabricated instant); `adopt [PATH]` is §9.6; `prune [--yes]` executes nothing
without `--yes`, because removing an administrative entry is your decision, and then runs `git
worktree prune` through the argument-array seam, no shell. `rift family remint` is §9.4.

Registering a path resolves its family first. The family **root** arriving over a member's row
**supersedes** it (a remove and an add event, since the log is append-only); any other second row
for a family is **refused** as an error, because a silent success would say the worktree was
registered when it was not. `rift init` inside a member registers the **root** and adopts the member.

`rift doctor` enumerates every member's git directory, and `--fix` drains and adopts across
members; its family legs are `phantom_gitdirs`, `orphan_projections`, `family_identity`,
`push_queue`, `prunable_worktrees`, `hooks_target`, `binary_of_record` and `worktree_wiring`. The
dashboard shows members as child rows with the **family total by default**, decomposable by
worktree ([11-canopy-tour.md](11-canopy-tour.md)).

---

## 9.12 Two kinds of debris

**Phantom git directories.** Discovery accepts a `.git` **directory** only with `HEAD` plus `objects/`
or `commondir`; otherwise a bare `mkdir .git` would become a repository with its own identity. A hook
there records **nothing**.

**Orphan projections.** A projection keyed by a checkout's path, from before the family mint, is a
stray database, reported and never merged or silently deleted: `rift projects prune
--orphan-projections` reports path, id and event count, and `--yes` removes them. Merging is impossible — append-only rows cannot move — and facts that exist nowhere else are not recoverable, which is why removal asks for the word. The family's own database is excluded **by identity, not by name**, in one enumeration the doctor
leg and the prune verb share.

---

## 9.13 Removing a worktree

`git worktree remove` deletes the member's checkout state and git directory; nothing Rift holds there
is authoritative. A commit captured in a checkout gone before the detached worker reaches it
([03-capture.md §3.10](03-capture.md)) degrades the condense to **commit-only**: the commit object is
in the shared store, so its diff, revisions and hunks stay derivable, while the tree does not. **No
checkpoint is archived** — archiving from a surviving member would write a shard under *that*
member's token for another member's session. An archive-decision row records
`worktree-missing:commit-only` with `archived = false`, so the gap is a row, not a silence.

A moved worktree keeps its family but gets a new token and new provenance rows. Identity drift is
reported, never repaired (§9.4).

---

## Related chapters

[02-configuration.md](02-configuration.md) ·
[03-capture.md](03-capture.md) ·
[06-recall.md](06-recall.md) ·
[07-serving-and-injection.md](07-serving-and-injection.md) ·
[10-agent-integrations.md](10-agent-integrations.md) ·
[11-canopy-tour.md](11-canopy-tour.md) ·
[13-storage-and-schema.md](13-storage-and-schema.md) ·
[15-benchmarks.md](15-benchmarks.md) ·
[17-invariants.md](17-invariants.md)
