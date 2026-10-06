# 1. Quickstart

From a clean checkout to a repository that captures every agent session,
verifies its own ledger, serves deltas back to your agents and shows it all in
the dashboard — about fifteen minutes, most of it the first compile.
[00-start-here.md](00-start-here.md) explains *why* the three planes exist.

---

## 1.1 Before you start

### Prerequisites

| You need | Version | Why |
|---|---|---|
| Rust toolchain | pinned at **1.96.0** in `rust-toolchain.toml` | `rustup` picks it up inside the checkout, so you never remember it. |
| Minimum supported Rust | **1.94** | Declared in `Cargo.toml` and the lint config, guarded by its own CI job. Rift compiles on 1.94; it is *developed* on the pin. |
| A C compiler | any working `cc` | The SQLite driver builds its bundled amalgamation locally rather than linking whatever the machine ships: a store whose engine version varies per machine cannot make byte-stable promises. |
| `git` | any recent version | Rift is git-native: checkpoints are git objects, hooks are git hooks, family identity lives in the common directory. |

No network connection, daemon, database server, container or API key is needed.
Nothing phones home, and nothing is downloaded at initialization time (§1.3).

### A supported agent

Three agents are wired today: **Claude Code**, **Codex** (standalone CLI and
editor extension — two independent facts), and **Antigravity**. Six more —
Gemini CLI, Cursor, Copilot CLI, OpenCode, Factory Droid and Ollama — keep
their row in every listing and in Project Settings but are refused rather than
half-wired, because a half-wired integration is indistinguishable from a quiet
one and a ledger with silent holes is worse than no ledger.
[10-agent-integrations.md](10-agent-integrations.md) has the wiring tables and
each client's reason. You can finish this quickstart with none installed; you
just end with captured commits rather than a captured *agent* session.

---

## 1.2 Build the binary

```bash
cargo build --workspace --release
cp target/release/rift ~/.local/bin/rift      # or wherever your PATH points
rift --version
```

Release is worth the wait: Rift's hooks run inside your git commands, and a
debug binary makes every commit feel slow (debug is fine for developing Rift
itself — [16-development.md](16-development.md)).

> **Tip:** Choose the final location *before* `rift init`: agent descriptors
> record an **absolute** path, so a hook fired in an unknown working directory
> still finds the right `rift`. Move it later and `rift doctor` reports the
> drift and prints the repair — it never silently re-points anything.

Two binaries live in the workspace: **`rift`**, the entrypoint — bare `rift`
opens Canopy, every subcommand a short-lived process that does one job and
exits — and **`rift-memd`**, the recall projection binary (`metrics`,
`rebuild`, `status`, `doctor`, `eval`): a maintenance and reporting tool, not a
server or an agent transport.

---

## 1.3 `rift init`

From the root of the repository you want to capture:

```bash
rift init
```

That is the whole setup. What its report means, and why each step exists:

### It registers the repository

Two registries get a row: a small TOML file under your config directory and the
dashboard's append-only store. Both are keyed by repository *family* identity
rather than path, so a repository with linked worktrees gets **one** row, not
one per checkout. Run `init` inside a linked worktree and it registers the
family's main work tree and adopts the member you stand in (§1.9).

### It scaffolds `.rift/`

A minimal `.rift/config.toml` is written if none exists — deliberately almost
empty, because every absent key falls back to a secure default, making an empty
config a *safe* one rather than an unconfigured one
([02-configuration.md](02-configuration.md)).

### It installs five git hooks

`commit-msg`, `prepare-commit-msg`, `post-commit`, `post-checkout` and
`pre-push`, installed into the git
**common** directory so every worktree in the family shares them, reported with
the binary of record they point at. Three properties matter more than the list.
They **never block you**: a failing hook, or a missing Rift, must not stop a
commit — capture is a witness, not a gatekeeper. They are **fast**: at most
100 ms of added git latency across a commit's hooks — a hard ceiling, not a
median — and anything slower goes to a detached worker (the separate
agent-hook budget is p50 100 ms / p95 500 ms; see
[03-capture.md](03-capture.md)). And **`init` never re-points a family that is
already working**: healthy hooks are kept, because silently re-pointing them at
whatever binary happened to run `init` once left a repository's hooks dangling
at a deleted path for days with no visible symptom.
[03-capture.md](03-capture.md) explains the hook-to-checkpoint pipeline.

### It wires the agents it finds

Detection is not eligibility. `init` checks every registered adapter, wires the
supported ones, and **says out loud** when it left a coming-soon agent alone —
a silently skipped agent looks exactly like one never installed. "Found" means
any of four kinds of evidence — a command on `PATH`, the vendor's own install,
a verified editor extension (Claude Code run only from VS Code counts), or a
JetBrains plugin — read by the one rule every surface shares
([10-agent-integrations.md](10-agent-integrations.md) §10.5), and each row of
the report says which (`present via VS Code extension 2.1.267`). Each wired
agent gets a capture descriptor under `.rift/agents/` plus the client's own
native configuration:

| Client | Capture wiring | Recall channel |
|---|---|---|
| Claude Code | `.rift/agents/claude-code.json` | a managed skill file under `.claude/skills/` |
| Codex | `.rift/agents/codex.json` + `.codex/hooks.json` | a managed block in `AGENTS.md` |
| Antigravity | `.rift/agents/antigravity.json` | a managed rules file under `.agents/` |

Rift validates every document before its first write, preserves unrelated
entries, replaces changed files atomically, and fingerprints its own writes so
it can later tell them from yours. If another tool owns a path Rift wants,
initialization **stops on that path**; edited, malformed or symlinked
configuration is refused for repair, never clobbered.

> Tip: `init` is per machine. A clone of a repository whose `.rift/` is
> committed arrives configured but not set up — no projection, no git hooks,
> no agent wiring, all of which are machine-local — and every repo-scoped
> command says so until you run `rift init` there. Re-running `init` is
> additive: it keeps the committed configuration, keeps healthy hooks, and
> wires only the agents detected here that are not wired yet.

### It enables Recall

Recall, the memory-delta plane, lets an agent ask "what changed since I last
looked?" instead of re-reading the repository. `init` enables it rather than
leaving it to a separate ritual, because a recall plane nobody turned on saves
nobody anything. Opt out with `rift init --no-recall`, which skips Recall's
skills and initial ingest and still wires the capture hooks of every agent it
detects; turn Recall on later, or inspect or remove it, with `rift recall
init`, `rift recall status` and `rift recall disable`. That last removes only
Rift-owned, fingerprint-matching blocks and keeps every captured byte:
disabling is not a delete.

### There is nothing to provision

A `--no-provision` flag and a `RIFT_NO_PROVISION` environment variable appear
in older documentation and harnesses; both are gone, and the flag is now a
usage error. Capture is native Rust inside the `rift` binary you just built: no
download, no pinned foreign binary, no checksum at setup, no second process to
keep in step with your toolchain.

The reasoning that made provisioning a default rather than an opt-in still
stands. A ledger off by default is empty by default, and the promise of a
complete, tamper-evident record fails *silently* when empty. The risk a consent
prompt would guard against is data leaving the machine, and mechanism handles
that instead: capture is local by construction and the shipped binaries' graph
holds no network client at all, so a prompt would buy paperwork rather than
safety. Every enable and every skip is an append-only audit row, so the choice
stays visible and reversible afterwards rather than only at the moment of
asking. A distribution channel with stricter norms wraps `init` with the
opt-out it needs; the default does not change for everybody else.

### Exit codes

`init` does everything it can, then reports.

| Code | Meaning |
|---|---|
| `0` | Everything requested succeeded. |
| `3` | Not an initialized Rift repository (another command asking you to run `init`). |
| `4` | A precondition failed: a native file Rift does not own, an agent whose configuration a human edited, a worktree that could not be adopted. Everything else was done. |
| `2` | Usage error: a flag that does not exist. |

A refusal is **reported, never an abort**: aborting on the first one used to
leave a half-initialized repository that `rift doctor` read as real. Every
agent is attempted, every refusal prints its reason, and the non-zero code
arrives at the end so a script still knows. Name agents explicitly and one is
not capturable yet, and `init` refuses **before** touching anything, naming
every offender at once and leaving the repository as it found it.

---

## 1.4 What landed on disk

Two files in the checkout are yours and authoritative: `.rift/config.toml`
(capture, redaction, retention) and `.rift/policies/serve.toml` (what Recall
may serve, to whom, under what budget). The rest is Rift's:
`.rift/agents/*.json`, one descriptor per wired agent; `.rift/state/`,
per-checkout run state and adoption markers, which git ignores through the
`.rift/.gitignore` `init` writes where none exists — one it finds is kept as
found, and when that one lets `.rift/state/` through, `init` says so in one
line rather than edit it; the five hooks and their sidecar
markers in the git common directory's `hooks/`; this family's minted identity
in `<git common dir>/rift-family/`, never to be hand-edited; the project list
at `~/.config/rift/projects.toml`; and the projection database at
`~/.cache/rift/<family>/`. [02-configuration.md](02-configuration.md) has the
full table with what is safe to delete — that last one being the important
reassurance, since the projection is *derived*, rebuilds from the checkpoint
branch with `rift rebuild`, and costs nothing but the rebuild to lose because
the system of record is git
([13-storage-and-schema.md](13-storage-and-schema.md)).

Client configuration outside the repository is never touched: Rift does not
write home-directory settings, grant project trust, approve hooks, restart a
client, upgrade a binary, or change authentication. Those stay explicit human
actions — a provenance tool is compromised the moment it starts approving
things on your behalf.

---

## 1.5 Your first captured session

Do ordinary work: start your agent here, approve the project hooks when the
client prompts (that prompt is the client asking about the configuration `init`
just wrote), let it change something, commit.

```bash
git add -p && git commit -m "first captured change"
rift status
```

`rift status` prints five counters: **checkpoints**, the durable hash-chained
records of work — the ledger; **sessions**, distinct agent sessions seen;
**captured events**, the prompts, tool calls and edits inside them; **hook
receipts**, proof the wiring fired, where a zero with non-zero everywhere else
means an agent is talking and git is not, or the reverse; and **transitions**,
session state-machine movements, how Rift knows a session ended rather than
merely went quiet. Then go deeper:

```bash
rift session list               # ages, agents, models, token totals
rift checkpoint list            # newest first, with truncated roots
rift checkpoint show b6caa84d   # one checkpoint: roots and chain position
rift explain b6caa84d           # roots, chain position, attribution, cost
```

Checkpoint identifiers are long, so every command accepts a unique prefix and
listings print a truncated form like `b6caa84d…`.

### Verify it

```bash
rift verify
rift doctor
```

`verify` re-derives the hash chain from the checkpoint branch and cross-checks
it against the projection. It has its own exit codes, and an integrity refusal
is deliberately *not* retryable: the bytes on offer are not the bytes that were
recorded.

`doctor` diagnoses capture health with no network access at all — stuck
sessions, a queue not draining, a gap at the tail of the chain, a projection
watermark fallen behind, drifted wiring — and with `--fix` applies the safe
repairs. It never auto-fixes an interior chain gap or a divergence: those are
evidence to look at, not to paper over.

> **Tip:** `rift doctor` is the right first move whenever anything looks wrong:
> read-only by default, it explains what it found and prints the command that
> fixes each finding rather than performing surgery you did not ask for.

[05-ledger-and-verification.md](05-ledger-and-verification.md) explains what
`verify` proves.

---

## 1.6 Canopy: the first look

```bash
rift
rift --ascii        # plain renderer: no truecolor, no 3D
rift --skip-splash  # or set RIFT_SPLASH=never
```

Bare `rift` on a terminal opens **Canopy**; piped or redirected it prints a
status summary instead, so `rift | tee log` in a script does something sensible
rather than try to draw a UI.

After a first-run splash you get a three-column grid: projects on the left
(worktree members indented under their family), the selected repository's
detail in the middle, live graphs on the right — the context-fullness gauge,
token flow, and what Recall saved by not re-feeding what the model already
held. `s` opens **Project Settings**, `r` the read-only **Recall Viewer** (also
`rift viewer`), `/` the command palette, `q` quits; the hint bar always shows
the keys live *right now*. Project Settings edits the repository's own policy
files, which stay authoritative — the modal is a front end, not a parallel
source of truth — and an uninitialized repository opens it as a resumable setup
page, so you can do everything §1.3 did from the dashboard. Full walkthrough
and key table: [11-canopy-tour.md](11-canopy-tour.md); architecture:
[12-canopy-architecture.md](12-canopy-architecture.md).

---

## 1.7 Recall's first answers

Recall reaches your agents two ways, neither a server you run: a small pointer
injected at the start of a turn, and a closed set of commands the agent shells
out to when the pointer says something is worth fetching. Nothing listens on a
port and nothing runs in the background.

```bash
rift recall status    # per-client configuration and capability
rift agent-help       # the agent-facing map of the serving verbs
rift recall known-state --task "add a retry to the uploader" --file src/upload.rs
```

`rift agent-help` renders live from the binary's command tree, so it cannot
describe a verb that no longer exists. The ranking-sensitive verbs *require* a
stated task and at least one focused file: ranking without a task is guesswork,
so the surface refuses rather than guess and call it an answer. For the same
questions asked by a human — who need not declare what they already know — use
the `rift memory` family:

```bash
rift memory diff-since b6caa84d
rift memory file-if-changed src/upload.rs
rift memory cost-hotspots
rift search upload     # where are the files called something like this
rift-memd status       # the projection's own view
```

Early on, expect honest empties: a repository with one session has almost no
delta to serve, and Rift says so rather than manufacture something. Savings
appear once there is history to save *from*. [06-recall.md](06-recall.md)
covers the delta model,
[07-serving-and-injection.md](07-serving-and-injection.md) what may be served
and under what budget.

---

## 1.8 Turning things off

Everything is reversible, and nothing reversible destroys data:

```bash
rift disable --agent claude-code    # remove one agent's hook wiring
rift recall disable                 # remove Rift-owned recall blocks
rift enable --agent claude-code     # put it back
```

Disabling removes wiring, not history: checkpoints, chain and projection
survive, you simply stop adding to them. A fragment you edited by hand is
reported as a conflict rather than removed — Rift will not delete something it
can no longer prove it wrote.

---

## 1.9 If you use worktrees

A linked git worktree is a **shadow member** of its repository family, not a
second project: one family identity in the git common directory keys one
projection, one recall store, one binary of record, one dashboard row. Usually
you do nothing — the family's `post-checkout` hook adopts a new member on `git
worktree add`. When that is not possible, these do it on demand:

```bash
rift worktree status    # this member's wiring, identity and observations
rift worktree adopt     # wire a linked worktree into its family
rift worktree list      # every member of the family
rift projects list --worktrees   # members indented under repositories
```

Adoption is always explicit or hook-driven: nothing mints a family identity as
a side effect of another command. Because members share one identity, recall
serves across them within branch containment, so savings compound across
worktrees, including the temporary ones agents create for subagent work
([09-worktrees-and-families.md](09-worktrees-and-families.md)).

---

## 1.10 When something is not right

`rift doctor` names the finding and prints the command that repairs it — for
hooks pointing at a binary you moved, a projection that fell behind (`rift
rebuild`), a member never adopted (`rift worktree adopt`, or `A` in Canopy).
Three symptoms have causes it cannot see. Zero hook receipts means the client
never approved the project configuration `init` wrote: restart it, approve when
prompted, then `rift recall status`. Exit `4` naming a file means something
else owns that path or a human edited it — read the reason and resolve the
conflict yourself, because Rift will not overwrite it. Codex detected but not
on `PATH` means the editor extension is installed and the standalone CLI is
not; separate facts on purpose, so wire whichever you use. A terminal that will
not render truecolor wants `rift --ascii`. And `rift init` is idempotent: safe
to re-run, never re-points healthy wiring, often the fastest way to find what
is missing.

---

## Related chapters

[02-configuration.md](02-configuration.md) · [03-capture.md](03-capture.md) ·
[04-redaction-and-secrets.md](04-redaction-and-secrets.md) ·
[05-ledger-and-verification.md](05-ledger-and-verification.md) ·
[06-recall.md](06-recall.md) ·
[07-serving-and-injection.md](07-serving-and-injection.md) ·
[10-agent-integrations.md](10-agent-integrations.md), the client-integration
reference · [11-canopy-tour.md](11-canopy-tour.md) ·
[17-invariants.md](17-invariants.md) · [19-glossary.md](19-glossary.md).
