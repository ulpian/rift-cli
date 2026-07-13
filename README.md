# Rift

**Rift is a Rust TUI + CLI for developers working with AI agents and coding
assistants.** Typing `rift` opens **Canopy**, a terminal dashboard showing every
project Rift watches, what each agent is doing, how much context and money is
being burned, and how much **rift-total-recall** is saving by not re-feeding the
model what it already knows.

Rift makes AI-assisted engineering **legible, recoverable, and cheaper**:

- **rift-shadow** captures every agent session — prompts, tool calls, file edits,
  token counts, checkpoints — as a git-native, tamper-evident, **immutable**
  ledger. Secrets are redacted *before* anything durable is written. v0.1
  silently wraps the [Entire](https://github.com/entireio/cli) CLI; the long-term
  path is a native-Rust rewrite.
- **rift-total-recall** projects a *memory-delta validity graph* over that ledger
  and exposes it to agents through a bounded MCP server, so they retrieve
  **deltas** (what changed, what's still valid, what's stale) instead of
  re-reading the world. It instruments `repeated_context_rate` *before* it
  optimizes, so savings are your own measured numbers.
- **rift-canopy** is the dashboard: a 3-column grid (projects · detail · live
  graphs), a context-fullness "nic-gauge", and a slash-command palette.

> **Scope.** This repo is the **local toolchain**. Rift Cloud (encrypted sync,
> web dashboard, forecasting) is acknowledged as Stage 7 outlook only — every
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
| `rift-config` | Policy-as-code (`.rift/config.toml`), a backtracking-free glob matcher, the optional-package `FeatureRegistry`. |
| `rift-crypto` | ChaCha20-Poly1305 AEAD, HKDF envelope keys, zeroized secret types. |
| `rift-hashchain` | SHA-256 leaves, Merkle roots, the checkpoint chain, inclusion proofs. |
| `rift-store` | Append-only store (rusqlite reference impl; **Turso** is the production target), migration engine, integrity chain, bitemporal cost facts, column encryption. |
| `rift-shadow*` | Capture: the silent Entire wrapper seam, the two-pass redaction gate, hooks/canonical events + state machine, line attribution + confidence, the four-class cost model. |
| `rift-recall-*` | Recall plane: content-addressed handles + the four-state envelope, hash-bound validity, ledger metrics, task-aware ranking, the context-pack compiler, serving policy, `memd.*` events, the bounded MCP surface, the substrate reader. |
| `rift-canopy` | The dashboard behind a `RenderBackend` seam (ASCII fallback; `xre-rs` 3D backend behind the `xre` feature). |
| `rift-fff` / `rift-codegraph` / `rift-nic-gauge` | Optional, toggleable packages. |
| `bins/rift`, `bins/rift-memd` | The `rift` entrypoint and the recall binary. |

## Build & run

```bash
cargo build --workspace
cargo test  --workspace          # ~220 tests
cargo run -p rift -- status      # CLI subcommand
cargo run -p rift               # (would launch Canopy)
cargo run -p rift-memd -- serve  # the recall plane / MCP surface
```

`rusqlite` is built `bundled`, so a C compiler is required.

### Capture backend provisioning

`rift init` **installs a local capture backend by default**: a pinned,
checksum-verified binary placed in a Rift-private directory (never on `PATH`),
run local-only — its cloud/auth verbs are refused, its environment scrubbed,
and no telemetry leaves the machine. Opt out with `rift init --no-provision`
or `RIFT_NO_PROVISION=1`; capture then stays inert. Both the install and the
skip are recorded as append-only audit rows (ADR-0001).

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

## For AI agents

- [CLAUDE.md](CLAUDE.md) — full project guidance (Claude Code).
- [AGENTS.md](AGENTS.md) — condensed cross-tool guidance; `.agents/` holds
  per-agent overrides.
- [SECURITY.md](SECURITY.md) — threat model + reporting.
- [.mcp.json](.mcp.json) — project MCP servers (empty by default).

## License

Apache-2.0.
