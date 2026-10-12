# Rift

**Rift is a Rust TUI + CLI for developers working with AI agents and coding
assistants.** Typing `rift` opens **Canopy**, a terminal dashboard showing every
project Rift watches, what each agent is doing, how much context and money is
being burned, and how much **rift-total-recall** is saving by not re-feeding the
model what it already knows.

Rift makes AI-assisted engineering **legible, recoverable, and cheaper**:

- **rift-shadow** captures every agent session — prompts, tool calls, file edits,
  token counts, checkpoints — as a git-native, tamper-evident, **immutable**
  ledger. Secrets are redacted *before* anything durable is written. Capture,
  condensation and archival are native Rust; pre-Rift checkpoint histories are
  readable through `rift import --from-legacy`.
- **rift-total-recall** projects a *memory-delta validity graph* over that ledger
  and exposes it through native hook injection plus bounded `rift recall`
  skills, so agents retrieve
  **deltas** (what changed, what's still valid, what's stale) instead of
  re-reading the world. It instruments `repeated_context_rate` *before* it
  optimizes, so savings are your own measured numbers.
- **rift-canopy** is the dashboard: a 3-column grid (projects · detail · live
  graphs), a context-fullness "nic-gauge", and a slash-command palette. Project
  policy and native agent wiring live in the Repository card's **Project
  Settings** modal rather than separate dashboard panels.

> **Scope.** This repo is the **local toolchain**. Rift Cloud (encrypted sync,
> web dashboard, forecasting) is a separately built surface — every
> local decision is made *sync-ready* so Cloud is additive, never a rewrite.

## Two non-negotiables

1. **Immutable by construction** — canonical data is append-only (no `UPDATE`/
   `DELETE`); corrections supersede; tamper-evidence is hash chains + Merkle
   roots, enforced at the type, SQL-trigger, and property-test layers.
2. **Security per line** — the `RedactedBytes` type makes persisting un-redacted
   bytes a *compile error*; untrusted output is never served raw; exact retrieval
   is policy-gated and audited. See [SECURITY.md](SECURITY.md).

## Workspace layout

| Crate | Role |
|---|---|
| `rift-core` | Shared: errors, ids (hash + ULID), local identity, time, `RedactedBytes`/`SecretBytes`, immutability markers. |
| `rift-config` | Policy-as-code (`.rift/config.toml` and `.rift/policies/serve.toml`), safe comment-preserving edits, redactor packs, and a backtracking-free glob matcher. |
| `rift-crypto` | ChaCha20-Poly1305 AEAD, HKDF envelope keys, zeroized secret types. |
| `rift-hashchain` | SHA-256 leaves, Merkle roots, the checkpoint chain, inclusion proofs. |
| `rift-store` | Append-only store (rusqlite reference impl; **Turso** is the production target), migration engine, integrity chain, bitemporal cost facts, column encryption. |
| `rift-shadow*` | Capture: the native checkpoint archive, the two-pass redaction gate, hooks/canonical events + state machine, line attribution + confidence, the four-class cost model. |
| `rift-recall-*` | Recall plane: handles + the four-state envelope, hash-bound validity, ledger metrics, task-aware ranking, the context-pack compiler, serving policy, `memd.*` events, the bounded hook/skill surface, the substrate reader. |
| `rift-canopy` / `rift-canopy-data` | The dashboard behind a `RenderBackend` seam plus its policy, native-adapter, and status projections. |
| `rift-agent*` | The native integration SDK and adapters for supported coding agents. |
| `rift-nic-gauge` | Read-only context-window utilization data for Canopy; it is not a project setting. |
| `rift-git` / `rift-registry` / `rift-tokens` | Native git plumbing and worktree discovery; the machine-global project registry; the one token-counting authority. |
| `bins/rift`, `bins/rift-memd` | The `rift` entrypoint and the recall binary. |

## Build & run

```bash
cargo build --workspace
cargo test  --workspace
cargo run -p rift -- status      # CLI subcommand
cargo run -p rift               # launch Canopy
cargo run -p rift -- viewer      # read-only checkpoint/Recall inspection
cargo run -p rift -- recall known-state --task "inspect Rift" --file README.md
cargo run -p rift-memd -- status # inspect the recall projection
```

`rusqlite` is built `bundled`, so a C compiler is required.

### Two binaries, one optional

`rift` is the whole product for daily use: capture, serving and Canopy run in
that one process, and `rift recall` keeps the recall projection fresh on its
own. `rift-memd` is the separate projection binary (`metrics`, `rebuild`,
`status`, `doctor`, `eval`) — it reports the measured baseline and rebuilds the
projection from git, so it must stay out of the hook-path binary (size cap; no
network crate in the shipped graph).

### Automatic Total Recall setup

`rift init` also enables Recall and safely provisions validated native project
wiring for detected agents. This alpha captures Claude Code, Codex and
Antigravity. Gemini CLI, Cursor, Copilot CLI, OpenCode, Factory Droid and
Ollama are not wired for capture, yet all nine stay in the shared adapter
registry, so every integration keeps its row in listings and Project Settings.
The reasons differ: five of the six declare verb surfaces the real agents never
send, so an install that appeared to succeed would capture nothing; the sixth
is outside this alpha's scope. Either way Rift refuses to enable them rather
than half-wire them — a refusal is honest about what is recorded, a half-wired
integration is not. The
supported set is not prose: it is the `ALPHA_AGENTS` constant in
`rift-shadow-hooks` that the refusal itself reads, so what this paragraph
claims and what the code does are the same list. Use `rift init --no-recall`
to opt out, or manage it later with `rift recall init`, `rift recall status`,
and `rift recall disable`.
Client trust, hook approval, authentication, upgrades, and restarts remain
explicit user actions.

In Canopy, select a repository and use `[ Project Settings ]`, press `s`, or run
`/settings`. The modal controls Recall, per-turn delta injection, tool-call and
token-usage capture, raw transcripts, and each native integration. Repository
policy files are authoritative; edits are validated, conflict-checked, and
atomically replaced while preserving comments and unrelated keys. Raw
transcripts always require an explicit risk confirmation. Uninitialized or
partially initialized repositories use the same modal as a resumable setup
page.

Use `[ Recall Viewer ]`, press `r`, run `/viewer [TARGET] [--repo PATH]`, or
launch `rift viewer [TARGET] [--repo PATH]` to inspect durable checkpoints,
redacted transcripts and tasks, metadata-only shadow state, Recall facts,
recorded hunks, and verification evidence. The viewer is read-only: standalone
launch skips ingest and write-capable Canopy startup, and an unregistered
`--repo` remains session-only.

The Codex row distinguishes the standalone CLI from the Codex extension in VS
Code and compatible editors; the extension's bundled runtime does not count as
a CLI on `PATH`. It also reports the project capture descriptor and
`.codex/hooks.json` separately. Enabling Codex merges fingerprinted Rift hook
groups while preserving unrelated hooks, and disabling removes only those
matching groups. Edited, malformed, or symlinked hook configuration is refused
for repair instead of overwritten.

## Develop

Common tasks run through [`skills.sh`](skills.sh) (see [SKILLS.md](SKILLS.md)):

```bash
./skills.sh check      # pre-push gate: fmt-check + clippy -D warnings + test
./skills.sh test
./skills.sh run -- status
./skills.sh list       # all tasks
```

CI runs the same gate across macOS/Linux/Windows plus a release build,
`cargo deny`, and `cargo audit`.

## Worktrees

A linked `git worktree` is a **shadow member** of its repository family, not a
second project. The family shares one identity minted in the git common dir:
one projection, one recall store, one binary of record, one registry row.
Every captured session and checkpoint records which member it happened in, and
recall serves any member within branch containment — so savings compound
across worktrees, including the temporary ones coding agents spin up for
subagent work.

```bash
rift worktree status     # this member's wiring, identity and observations
rift worktree adopt      # wire a linked worktree into its family
rift worktree prune      # report (or, with --yes, prune) departed members
rift projects --worktrees  # list members indented under their repository
```

New worktrees are adopted automatically by the family's `post-checkout` hook;
`adopt` is the explicit path for everything else. `rift doctor` reports a
member whose wiring has drifted and prints the command — it never re-points
hooks or re-mints an identity as a side effect.

## For AI agents

- [CLAUDE.md](CLAUDE.md) — full project guidance (Claude Code).
- [AGENTS.md](AGENTS.md) — condensed cross-tool guidance; `.agents/` holds
  per-agent overrides.
- [SECURITY.md](SECURITY.md) — threat model + reporting.
- [The guide](docs/guide/00-start-here.md) — what each plane does and why each
  standing decision was made; [agent integrations](docs/guide/10-agent-integrations.md)
  covers native hook and CLI-skill configuration for Claude Code, Codex, and
  Antigravity.

## License

Apache-2.0.
