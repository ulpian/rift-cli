# Start here

What Rift is, why it is shaped this way, and which chapter to read next. Prefer
a working install first? Go to [Quickstart](./01-quickstart.md).

## 1. What Rift is

**A Rust terminal dashboard and CLI for developers who work with AI coding
agents.** Type `rift` and you get **Canopy**, a dashboard over every repository
Rift watches: active agents, what they touched, context fullness, session cost,
and how much cost was avoided because the agent got a *delta* instead of the
whole world again.

Rift does two things no agent does for itself. **It remembers**: every session
lands in an immutable, tamper-evident, git-native record, secrets stripped
*before* anything durable is written. **It forgets on your behalf**: from that
record it projects what an agent knows and whether that is still true, so the
next turn gets *what changed*. It is local: your machine, your repositories, no
network on the capture path, and formats written to be synchronisable
([Sync readiness](./14-sync-readiness.md)).

## 2. The problem

You cannot see what happened (scrollback is not an answer). You cannot trust
what was kept, since transcripts hold secrets and keeping them makes a new place
for credentials to live. You pay repeatedly for context an agent already read.
Three planes answer these, separable on purpose: capture without serving, or the
dashboard without either.

## 3. The planes

**Shadow ledger (the record).** Append-only (a correction is a new superseding
row), git-native (artifacts live in your object store under Rift-owned
references, not a sidecar that drifts), tamper-evident (hashes chained into
roots; Rift cannot stop disk access editing history, it makes the edit obvious).
Hooks sit on the critical path of your `git commit`, so a strict latency budget
applies and slow work goes to a detached worker: a blocking hook gets
uninstalled. [03](./03-capture.md), [05](./05-ledger-and-verification.md).

**Total Recall (the delta plane).** An agent's knowledge of a file is a *claim*
about exact content. Summaries are bound to the content they summarise, so a
stale one is structurally unservable: a stale summary reaching a model is a false
memory, worse than none. Recall also acts where tokens are spent: a read of
lines the agent already holds is declined once with an actionable reason (always
with the repeat allowed), a document is converted once, a file-name search is
answered from an index. It measures repetition *before* optimising, so a saving
comes from your repository and an upper bound is labelled as one.
[06](./06-recall.md), [07](./07-serving-and-injection.md),
[08](./08-cost-and-savings.md).

**Canopy (the dashboard).** Three columns, a palette, a Project Settings modal
and a read-only viewer. The repository's policy files are the truth: the modal
edits them through the CLI's conflict-checked, comment-preserving editor. The
renderer sits behind a seam whose ASCII implementation is always available.
[11](./11-canopy-tour.md), [12](./12-canopy-architecture.md).

**Agent integrations.** One SDK, one adapter per client, one registry. Nine
clients have rows; Claude Code, Codex and Antigravity are wired for capture. The
rest are refused rather than half-wired, because a healthy-looking half-wired
integration is discovered only when a session you needed was never recorded.
[10](./10-agent-integrations.md).

## 4. Two rules that bind every part

**Immutable by construction**: no updates or deletes, enforced in layers (types
offer no mutation, the database refuses the write, a property test checks every
table's guard) because a rule enforced in one place gets bypassed. **Security is
a per-line property**: raw bytes have a type that cannot be persisted, so an
un-redacted write is a compile error, not an incident. See
[Invariants](./17-invariants.md), [Redaction](./04-redaction-and-secrets.md),
[SECURITY.md](../../SECURITY.md).

## 5. What Rift deliberately is not

No language model or inference. No index of what your code means (its search index holds
file *names* and modification times, never content, so a file search is
answered instead of re-listing the tree). No background daemon: work happens on
hooks and commands. Nothing uploaded. No wrapper around another tool: Rift runs
no other binary. No hosted service. [What Rift does not do](./18-what-rift-does-not-do.md)
lists the limits that remain by decision.

## 6. Where things live

Policy in `.rift/` in your checkout; the capture record in your git object store;
the family identity in the git *common* directory; rebuildable projections under
`~/.cache/rift/`; the registry at `~/.config/rift/projects.toml`. **Under your
repository is authoritative, under your cache is rebuildable.**
[Configuration](./02-configuration.md) has the table. A linked worktree is a
*member of a repository family*, not a project: one identity keys the projection,
recall store and registry row, and nothing re-mints it as a side effect
([09](./09-worktrees-and-families.md)).

## 7. Reading order

*Run it*: 01 → 02 → 11 → 10. *The savings claim*: 06 → 07 → 08 → 15 → 18.
*Trust*: 04 → 05 → 17 → [SECURITY.md](../../SECURITY.md). *Changing the code*:
16 → 13 → 17 → the plane's chapter. Code follows crate boundaries with one-way
dependencies; heavy externals sit behind seams (store, git reader, renderer).

### The chapters

| # | Chapter | What it gives you |
|---|---|---|
| 00 | Start here (this one) | Rift, the three planes, this map. |
| 01 | [Quickstart](./01-quickstart.md) | Prerequisites, `rift init`, a first session. |
| 02 | [Configuration](./02-configuration.md) | Every policy key, where state lives, the variables. |
| 03 | [Capture](./03-capture.md) | Hooks to events to checkpoints; latency budgets. |
| 04 | [Redaction and secrets](./04-redaction-and-secrets.md) | The one-way type wall, detectors, packs. |
| 05 | [Ledger and verification](./05-ledger-and-verification.md) | Checkpoints, roots, the chain, verify and repair. |
| 06 | [Recall](./06-recall.md) | Known-state anchors, content-bound validity, what a window holds. |
| 07 | [Serving and injection](./07-serving-and-injection.md) | Three tiers of push, the pull verbs, budgets, trust. |
| 08 | [Cost and savings](./08-cost-and-savings.md) | Token classes, tokenizer authority, the honesty ladder. |
| 09 | [Worktrees and families](./09-worktrees-and-families.md) | Family identity, adoption, compounding. |
| 10 | [Agent integrations](./10-agent-integrations.md) | Per-client wiring, detection, custody, refusals. |
| 11 | [Canopy tour](./11-canopy-tour.md) | Columns, cards, keys, palette, viewer. |
| 12 | [Canopy architecture](./12-canopy-architecture.md) | Render seam, data edge, layout, modal parity. |
| 13 | [Storage and schema](./13-storage-and-schema.md) | Three stores, guards, frozen migrations, hashes. |
| 14 | [Sync readiness](./14-sync-readiness.md) | Stream cursor, push queue, bundles. |
| 15 | [Benchmarks](./15-benchmarks.md) | The protocol, the floors, re-measuring. |
| 16 | [Development](./16-development.md) | Task runner, CI jobs, coverage floors. |
| 17 | [Invariants](./17-invariants.md) | The must-and-never rules, each with its reason. |
| 18 | [What Rift does not do](./18-what-rift-does-not-do.md) | The permanent limits: upper bounds, by-design omissions, guard residues. |
| 19 | [Glossary](./19-glossary.md) | Every term, defined once, with its chapter. |

### Outside this guide

[README.md](../../README.md) · [CLAUDE.md](../../CLAUDE.md) ·
[AGENTS.md](../../AGENTS.md) · [SECURITY.md](../../SECURITY.md) ·
[SKILLS.md](../../SKILLS.md)
