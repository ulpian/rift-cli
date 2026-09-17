# Start here

What this thing is, what its moving parts are, and which chapter to read next;
every claim here is unpacked later, and the links say where. Prefer a working
install first? Skip to [Quickstart](./01-quickstart.md).

---

## 1. What Rift is

**Rift is a Rust terminal dashboard and command-line tool for developers who
work with AI coding agents.**

Type `rift` and you get **Canopy**, a dashboard over every repository Rift
watches: active agents, what they touched, how full their context windows are,
what the session costs, and how much of that cost is avoided because the agent
got a *delta* rather than the whole world again.

Rift does two things no coding agent does for itself. **It remembers**: every
session — prompts, tool calls, file edits, token counts, checkpoints — lands in
an immutable, tamper-evident, git-native record, secrets stripped *before*
anything durable is written. **It forgets on your behalf**: from that record it
projects what an agent knows and whether that is still true, so the next turn
gets *what changed*.

This repository is the **local toolchain** — your machine, your repositories,
no network calls on the capture path. A hosted service (encrypted sync, a web
dashboard, forecasting) is a future direction, out of scope here, but it shapes
local data today: formats are written to be synchronisable later, so adding a
service is additive rather than a rewrite ([Sync
readiness](./14-sync-readiness.md)).

---

## 2. The problem it exists to solve

Three recurring failure modes of working with a coding agent; Rift is organised
around them. **You cannot see what happened** — which edits were the agent's,
what it read before deciding, what the session cost; scrollback is not an
answer and by tomorrow it is gone. **You cannot trust what was kept** —
transcripts are full of secrets (a token in a prompt, a `.env` echoed by a tool
call, a key in a stack trace), so keeping them creates a new place for
credentials to live. **You pay for the same context repeatedly** — every turn
and session an agent re-reads what it has already read and you are billed for
it, while what would avoid that (*has the model seen this, is it still valid?*)
lives only in the scrollback nobody kept.

---

## 3. The three planes

Those failure modes map onto three layers — a record you can inspect, a
redaction wall making it safe to keep, a delta-serving layer turning it into
savings — separable on purpose: capture without ever serving a delta, or the
dashboard without either.

### 3.1 The shadow ledger — the system of record

The capture plane watches your repository and your agents and writes what it
sees into an **append-only, git-native ledger**.

- **Append-only** literally: canonical rows are never updated or deleted, and a
  correction is a new superseding row, so the correction's own history survives
  (§5 has the enforcement layers).
- **Git-native**: durable artifacts live in your repository's object store
  under Rift-owned references — not a sidecar that drifts from your code — and
  checkpoints hang off the commits they describe.
- **Tamper-evident**: records are hashed, hashes chained, chains rolled into
  roots, and verification reports a byte that moved. Rift cannot stop someone
  with disk access editing history; it makes the edit obvious.

Capture runs from git hooks and per-agent hooks, and it runs *fast*: hooks sit
on the critical path of your own `git commit`, so a strict latency budget
applies and slow work goes to a detached worker. A blocking hook gets
uninstalled by an annoyed developer, so "never block the developer" is a design
rule. Checkpoint histories from earlier, non-Rust tooling stay *readable*
through an import path; nothing writes that shape any more, and the importer
exists so older history is not stranded.

### 3.2 Total Recall — the memory-delta plane

The recall plane projects a **validity graph** over the ledger. Its central
idea, stated precisely:

> An agent's knowledge of a file is a *claim*: "as of this content, this is
> what that file says." A claim is either still true, drifted, gone, or never
> made. Serving the agent means answering that question, not resending the
> file.

So rather than a cache keyed by filename, Rift binds summaries to the exact
content they summarise: change the content and the binding stops matching, so
the summary is *structurally* unable to be served as current, not merely
flagged. A stale summary that can still reach a model is a false memory, worse
than no memory, so that state is unrepresentable rather than unlikely.

Above validity sit ranking (of everything valid, what is worth this turn's
budget?), a pack compiler (deltas small enough to inject), and a serving policy
(what may be revealed to which agent, under what trust, with what audit trail).
The plane also acts where the token is actually spent: it keeps a record of the
ranges each context window was handed, and at the moment an agent asks to read
lines it already holds, that read is declined once with a reason it can act on
— on clients whose own documentation defines such a reply, and always with the
repeat allowed. The same applies to the other two ways context gets spent
without being read: a document is converted once and read as Markdown, and a
search for a file's name is answered from an index rather than re-listing the
tree.
Recall is honest about arithmetic too: it measures how much context your agents
repeated *before* optimising anything, so a claimed saving comes from your own
repository, not a vendor's benchmark, and an upper bound is labelled as one.

### 3.3 Canopy — the dashboard

Canopy is a three-column terminal UI — projects left, the selected
repository's detail middle, live measurements right — plus a context-fullness
gauge, a slash-command palette, and a Project Settings modal reachable from the
Repository card, the `s` key, or `/settings`. A read-only viewer (`r`, `/viewer`,
`rift viewer`) inspects checkpoints, redacted transcripts, recall facts and
verification evidence without the write paths.

Two things to know first. **The repository's policy files are the truth, not
the UI**: the modal edits `.rift/config.toml` and the serving policy through
the same conflict-checked, comment-preserving editor the CLI uses, so a
hand-edit shows up in Canopy and a Canopy edit leaves your comments and
unrelated keys intact — nothing Rift does not own is overwritten.
**The renderer sits behind a seam**: its plain-ASCII implementation is always
available, so there is no hard dependency on any graphics engine, and the
richer backend is an opt-in build feature.

---

## 4. The fourth thing: agent integrations

Every agent wires up differently, so Rift ships an integration SDK plus one
adapter per supported client, all in one registry so listings, Project Settings
and the CLI agree on what exists.

Nine clients have adapter rows; **three are wired for capture today** — Claude
Code, Codex, and Antigravity. The rest are listed as coming soon and Rift
*refuses* to enable them rather than half-wire them, because a half-wired
integration that looks healthy is worse than an honest "not yet": you discover
the difference only when a session you needed was never recorded. Detection is
fussier than it looks — for one client "installed" is two independent facts,
its project hook groups a third, separately fingerprinted one.
[Agent integrations](./10-agent-integrations.md) has the wiring tables, the
per-client reasons and the custody rules.

---

## 5. Two rules that bind every part

Two constraints ride on every change here; [Invariants](./17-invariants.md)
unpacks both with their reasoning.

**1. Immutable by construction.** Canonical tables are append-only: no updates,
no deletes, corrections supersede. Enforcement is layered — public types offer
no mutating operation, the database refuses the write, a property test walks
the schema registry for any new table missing its guard — because a rule
enforced in one place gets bypassed. Add a canonical table without a guard and
the build goes red before you notice.

**2. Security is a per-line property, not a review step.** Raw captured bytes
have a type that cannot be persisted, so persisting an un-redacted byte is a
compile error rather than an incident; untrusted tool output is never served
raw, and exact retrieval is policy-gated and audited. [Redaction and
secrets](./04-redaction-and-secrets.md) has the wall,
[SECURITY.md](../../SECURITY.md) the threat model.

> Tip: A change that seems to require an `UPDATE` is almost certainly a
> modelling problem, not an exception — see the superseding-row pattern in
> [Storage and schema](./13-storage-and-schema.md).

---

## 6. What Rift deliberately is not

- **Not a language model, or a caller of one.** No inference anywhere in this
  toolchain — an absence that is a rule, not an omission.
- **Not a semantic index of your code.** No embeddings, no code graph, nothing
  claiming to know what a function *means*: recall is about what an agent
  already knows and whether it is still true. Rift does keep an index of file
  *names* — path names and modification times, never content — so that a search
  for a file can be answered instead of your repository being listed again.
- **Not a background daemon.** Work happens on hooks and explicit commands; no
  always-on service holds your repository open.
- **Not networked on the capture path.** Capture reads and writes your machine;
  nothing is uploaded.
- **Not a wrapper around another tool.** Capture, condensation, redaction and
  archival are native Rust end to end; Rift runs no other binary for its own
  work.
- **Not a hosted service today.** The dashboard is your terminal.

---

## 7. Where things live

Repository policy and local state live in `.rift/` in your checkout; the
durable capture record in your git object store under Rift-owned references;
the family's minted identity in the git *common* directory, one per family,
shared by all its worktrees; rebuildable projections under `~/.cache/rift/`,
where delete-and-rebuild is supported; the machine-wide registry at
`~/.config/rift/projects.toml`. [Configuration](./02-configuration.md) has the
full state-roots table.

In two lines: **anything under your repository is authoritative, anything under
your cache is rebuildable.** A confusing projection is cheap to rebuild; a
confusing policy file is the truth and must be edited.

Worktrees catch people: a linked git worktree is a *member of a repository
family*, not a separate project. One identity, minted once in the common
directory, keys the projection, the recall store and the registry row for the
whole family; members are adopted explicitly, and nothing re-mints an identity
as a side effect — see [Worktrees and
families](./09-worktrees-and-families.md).

---

## 8. The shape of the source tree

| Area | What lives there |
|---|---|
| `crates/rift-core` | Errors, identifiers, timestamps, redacted/secret byte types, immutability markers. |
| `crates/rift-config` | Policy as code, comment-preserving TOML edits, redaction packs, globs. |
| `crates/rift-crypto`, `crates/rift-hashchain` | Encryption primitives; hashing, chains, roots, proofs. |
| `crates/rift-store` | Append-only store, migrations, integrity chain, cost facts, column encryption. |
| `crates/rift-shadow*` | Capture: archive, redaction gate, hooks and events, attribution, cost model. |
| `crates/rift-recall-*` | Handles and envelopes, validity, metrics, ranking, packs, policy, serving. |
| `crates/rift-canopy`, `crates/rift-canopy-data` | The dashboard and its data plane. |
| `crates/rift-git`, `crates/rift-registry` | Git plumbing behind a seam; the machine-wide project registry. |
| `crates/rift-agent*` | The integration SDK and the per-client adapters. |
| `bins/rift`, `bins/rift-memd` | The `rift` entrypoint, and the projection binary. |

Dependencies point one way only, and a committed snapshot of the crate
adjacency list makes an accidental cycle a review-visible diff. Heavy or
pre-1.0 external engines sit behind seams: store, git reader and renderer each
have a tested reference implementation, with the external engine pluggable
without touching callers.

---

## 9. How to read this guide

Twenty chapters, in a deliberate but not mandatory order. Four tracks: *just
run it* = [01](./01-quickstart.md) → [02](./02-configuration.md) →
[11](./11-canopy-tour.md) → [10](./10-agent-integrations.md); *the savings
claim* = [06](./06-recall.md) → [07](./07-serving-and-injection.md) →
[08](./08-cost-and-savings.md) → [15](./15-benchmarks.md) →
[18](./18-known-gaps.md); *trusting it with our repositories* =
[04](./04-redaction-and-secrets.md) → [05](./05-ledger-and-verification.md) →
[17](./17-invariants.md) → [SECURITY.md](../../SECURITY.md); *changing the
code* = [16](./16-development.md) → [13](./13-storage-and-schema.md) →
[17](./17-invariants.md) → the chapter for the plane you are touching →
[18](./18-known-gaps.md).

> Tip: [Known gaps](./18-known-gaps.md) is not an appendix. It lists what is
> designed but unbuilt, measured but not proven, or true only under conditions.
> Read it before you rely on anything.

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
| 18 | [Known gaps](./18-known-gaps.md) | Unbuilt pieces, upper bounds, guard gaps. |
| 19 | [Glossary](./19-glossary.md) | Every term, defined once, with its chapter. |

### Conventions

Commands are typed from the repository root; example hashes are truncated
(`b6caa84d…`); `> Tip:` marks a skippable aside; and a stated *design rule*
always comes with its reason, because a rule without one gets broken during the
next refactor.

### Related documents outside this guide

- [README.md](../../README.md) — the short pitch and command summary.
- [CLAUDE.md](../../CLAUDE.md) — working guidance for AI agents here.
- [AGENTS.md](../../AGENTS.md) — the condensed, cross-tool version.
- [SECURITY.md](../../SECURITY.md) — threat model and reporting.
- [SKILLS.md](../../SKILLS.md) — the task runner's catalogue.
