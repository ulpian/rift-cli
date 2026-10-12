# 1. Quickstart

From a clean checkout to a repository that captures agent sessions, verifies its
ledger and serves deltas back. [00-start-here.md](00-start-here.md) says why.

## 1.1 Before you start

You need the Rust toolchain pinned in `rust-toolchain.toml` (1.96.0; MSRV 1.94
is its own CI job), a C compiler (the SQLite driver builds its bundled
amalgamation, because a store whose engine varies per machine cannot make
byte-stable promises) and `git` (checkpoints are git objects, hooks are git
hooks). No network, daemon, database server or API key; nothing is downloaded at
initialization.

Wired agents: **Claude Code**, **Codex** (CLI and editor extension, independent
facts) and **Antigravity**. The other six clients keep their row but are
refused, since a half-wired integration is indistinguishable from a quiet one
([10](10-agent-integrations.md)). With none installed you get captured commits,
not a captured agent session.

## 1.2 Build

```bash
cargo build --workspace --release
cp target/release/rift ~/.local/bin/rift
```

Release, because hooks run inside your git commands. Choose the final location
*before* `rift init`: descriptors record an absolute path so a hook in an unknown
directory finds `rift`. If you move it, `rift doctor` reports the drift and
prints the repair; nothing re-points silently. `rift-memd` (`metrics`,
`rebuild`, `status`, `doctor`, `eval`) is the recall projection tool, not a
server.

## 1.3 `rift init`

```bash
rift init
```

- **Registers** the repository by *family* identity, not path, so worktrees share
  one row; run in a linked worktree it registers the main tree and adopts you.
- **Scaffolds `.rift/config.toml`**, nearly empty on purpose: every absent key
  has a secure default ([02](02-configuration.md)).
- **Installs five git hooks** (`commit-msg`, `prepare-commit-msg`,
  `post-commit`, `post-checkout`, `pre-push`) in the git common directory. They
  never block a commit (capture is a witness, not a gatekeeper) and add at most
  100 ms of git latency, sending slower work to a detached worker
  ([03](03-capture.md)). `init` never re-points healthy hooks, which once left
  hooks dangling at a deleted path unnoticed.
- **Wires the agents it finds** and says aloud when it skips a coming-soon one.
  "Found" is any of four evidence kinds, read by one rule ([10](10-agent-integrations.md)
  §10.5): a `PATH` command, the vendor's install, a verified editor extension, a
  JetBrains plugin. Each wired agent gets `.rift/agents/<agent>.json` plus its
  native configuration and recall channel (Claude Code: a skill under
  `.claude/skills/`; Codex: `.codex/hooks.json` and a block in `AGENTS.md`;
  Antigravity: a rules file under `.agents/`). Every document is validated before
  its first write, unrelated entries are kept, writes are atomic and
  fingerprinted, and a path another tool owns stops `init` on that path;
  edited, malformed or symlinked configuration is refused, never clobbered.
- **Enables Recall**, on by default because an unused recall plane saves
  nothing. A consent prompt would guard data leaving the machine, which
  mechanism already prevents: capture is local and the shipped binaries hold no
  network client. `--no-recall` skips Recall's skills and initial ingest but
  still wires capture.

`init` is per machine: a clone with committed `.rift/` arrives configured but not
set up until you run it there. Re-running is additive.

| Exit | Meaning |
|---|---|
| `0` | Everything requested succeeded. |
| `3` | Not an initialized Rift repository. |
| `4` | A precondition failed (a file Rift does not own, hand-edited agent configuration, an unadoptable worktree); everything else was done. |
| `2` | Usage error. |

A refusal is reported, never an abort, since aborting on the first would leave a
half-initialized repository that `doctor` reads as real. Naming an agent that is
not capturable refuses *before* touching anything.

Two files are yours: `.rift/config.toml` and `.rift/policies/serve.toml` (what
Recall may serve, to whom, under what budget). `.rift/agents/` and
`.rift/state/` are Rift's and git-ignored. The projection under
`~/.cache/rift/<family>/` is derived and rebuilds with `rift rebuild`, because
git is the system of record ([02](02-configuration.md), [13](13-storage-and-schema.md)).
Rift never writes home-directory client settings, grants trust, approves hooks or
changes authentication: a provenance tool is compromised the moment it approves
things for you.

## 1.4 First captured session

Start your agent, approve the project hooks when it prompts (that is the client
asking about what `init` wrote), change something, commit.

```bash
git add -p && git commit -m "first captured change"
rift status          # checkpoints, sessions, events, hook receipts, transitions
rift explain b6caa84d   # roots, chain position, attribution, cost
rift verify && rift doctor
```

Zero hook receipts with non-zero elsewhere means an agent is talking and git is
not, or the reverse. `verify` re-derives the hash chain and cross-checks the
projection; an integrity refusal is deliberately not retryable, since the bytes
offered are not the bytes recorded. `doctor` is read-only by default, names each
finding with its fix, and `--fix` applies only safe repairs: an interior chain
gap or divergence is evidence, never auto-fixed ([05](05-ledger-and-verification.md)).

## 1.5 Canopy

```bash
rift                 # or rift --ascii for no truecolor/3D; --skip-splash
```

On a terminal this opens the dashboard; piped it prints a status summary. `s`
opens Project Settings (edits the policy files, which stay authoritative; an
uninitialized repository opens as a resumable setup page, i.e. §1.3), `r` the
read-only viewer, `/` the palette ([11](11-canopy-tour.md)).

## 1.6 Recall

Recall reaches agents by a pointer injected at turn start and a closed set of
commands, with no server. Try it:

```bash
rift recall status
rift agent-help
rift recall known-state --task "add a retry to the uploader" --file src/upload.rs
rift memory diff-since b6caa84d     # the same questions, for a human
rift search upload                  # file names, not content
```

The ranking verbs require a task and a file, because ranking without a task is
guesswork and the surface refuses rather than guess. A repository with one
session has little to serve and says so ([06](06-recall.md),
[07](07-serving-and-injection.md)).

## 1.7 Turning things off, and worktrees

```bash
rift disable --agent claude-code     # enable puts it back
rift recall disable                  # removes only Rift-owned blocks
```

Disabling removes wiring, never history; a fragment you edited is reported as a
conflict because Rift cannot prove it wrote it. The `post-checkout` hook adopts
new worktrees; otherwise `rift worktree status|adopt|list` and
`rift projects list --worktrees`. Members share one family identity, so recall
compounds across them within branch containment ([09](09-worktrees-and-families.md)).

## 1.8 When something is wrong

`rift doctor` first. Zero hook receipts: the client has not approved the project
configuration; restart and approve. Exit `4` naming a file: something else owns
or edited it, and Rift does not overwrite it. Codex detected but not on `PATH`:
only the extension is installed. No truecolor: `rift --ascii`. `rift init` is
idempotent and never re-points healthy wiring.
