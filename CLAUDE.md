# CLAUDE.md

Project guidance for Claude Code (and other agents — see [AGENTS.md](AGENTS.md)).

## What this is

**Rift** is a Rust **TUI + CLI for developers working with AI agents and coding
assistants**. Typing `rift` launches the **Canopy** dashboard. Underneath it
captures every agent session into an immutable, tamper-evident, git-native ledger
and projects a memory-delta graph that lets agents retrieve *deltas* instead of
re-reading the world.

This repo implements the local toolchain described in
[`plan_v1/build-plan/`](plan_v1/build-plan/) (the binding spec). Rift Cloud
(sync, web dashboard, forecasting) is Stage 7 outlook only — out of scope here,
but every local decision is made *sync-ready*.

Where implementation deliberately deviates from or refines the spec, the
decision is recorded in [`docs/adr/`](docs/adr/) — ADRs supersede the spec
text they name. Domain vocabulary is defined in
[`docs/GLOSSARY.md`](docs/GLOSSARY.md) (use those terms with those meanings),
and the executable plans for the current milestones live in
[`docs/prd/`](docs/prd/).

The three core packages, three optional ones, and one storage substrate:

| Package | Role |
|---|---|
| **rift-shadow** | Immutable, git-native system of record for AI sessions (capture, redaction, attribution, cost, hash chain). v0.1 silently wraps the Entire CLI. |
| **rift-total-recall** | Memory-delta validity plane projected over rift-shadow data; bounded MCP server so agents retrieve deltas, not full re-reads. |
| **rift-canopy** | The terminal dashboard — the `rift` entrypoint (3-column grid, nic-gauge, slash palette) behind a `RenderBackend` seam. |
| **fff / codegraph / nic-gauge** *(optional)* | Toggleable: fuzzy search, code-relationship graph, context-% meter. |
| **Turso** | SQLite-compatible async store for the rebuildable projection (behind the `CanonicalStore` seam; a `rusqlite` reference impl ships today). |

## Two hard constraints (carried into every change)

1. **Immutable by construction.** Canonical tables are append-only — no `UPDATE`,
   no `DELETE`. Corrections are new, superseding rows; tamper-evidence is hash
   chains + Merkle roots. Enforced in three layers: the type layer (no mutating
   API), the SQL layer (`BEFORE UPDATE/DELETE … RAISE(ABORT)` triggers on every
   canonical table), and a registry-driven property test in `rift-store`. **Add a
   canonical table without its guard and CI goes red.** See
   [`plan_v1/build-plan/00-overview/04-database-design-4nf-5nf-immutability.md`](plan_v1/build-plan/00-overview/04-database-design-4nf-5nf-immutability.md).
2. **Security is a per-line property.** Secrets never reach durable storage
   un-redacted (the `RedactedBytes` type makes that a compile error); untrusted
   tool output is never served raw; exact retrieval is policy-gated + audited.
   See [SECURITY.md](SECURITY.md) and
   [`…/05-security-model-and-threat-model.md`](plan_v1/build-plan/00-overview/05-security-model-and-threat-model.md).

## Prerequisites

- Rust toolchain pinned in `rust-toolchain.toml` (**1.96.0**); MSRV is **1.94**
  (`Cargo.toml` `rust-version`, `clippy.toml`).
- `rusqlite` is built with the `bundled` feature, so a **C compiler** is required
  (it compiles the SQLite amalgamation).
- The Entire binary is only needed for the live v0.1 capture path; the wrapper is
  behind the `EntireRunner` seam and tests use a mock.

## Commands

Prefer the task runner; it is the single source of truth for common workflows:

```bash
./skills.sh check       # fmt --check + clippy -D warnings + test  (pre-push gate)
./skills.sh test        # cargo test --workspace
./skills.sh lint        # cargo clippy --workspace --all-targets --all-features -D warnings
./skills.sh fmt         # cargo fmt --all
./skills.sh build       # cargo build --workspace
./skills.sh run -- status   # run the rift binary with args
```

Raw cargo equivalents work too. CI: single-leg `fmt` + `clippy -D warnings`
jobs, `test` across macOS/Linux/Windows (fail-fast off), a release build, an
MSRV build, coverage gates (≥85% line + ≥75% branch workspace-wide; ≥95% line
for `ci/security-crates.txt`, enforced by `xtask check-coverage`), `cargo
deny`/`cargo audit`, a required 60s-per-target fuzz smoke (`fuzz/`), advisory
miri, and `xtask ci-meta` (required jobs, read-only permissions, the
`ci/lint-baseline.txt` lint-table lock, no `unsafe`, no crate-root blanket
allows, no schema fast-path in bins). A scheduled `fuzz-nightly.yml` runs
10-minute budgets daily.

## Workspace layout (maps 1:1 to `plan_v1/scaffold/`)

```
crates/
  rift-core         shared: RiftError, ids (Sha256Id/Blake3Id/Ulid + semantic),
                    identity, Timestamp, RedactedBytes/SecretBytes, Immutable
  rift-config       policy-as-code (.rift/config.toml), glob matcher, FeatureRegistry
  rift-crypto       ChaCha20-Poly1305 AEAD, HKDF envelope keys, zeroized secret types
  rift-hashchain    SHA-256 leaves, Merkle roots, chain + inclusion proofs
  rift-store        append-only store (rusqlite ref impl), migrations, integrity
                    chain, bitemporal cost facts, column encryption (Turso = target)
  rift-shadow*      capture: entire wrapper seam, redaction gate, hooks/events +
                    state machine, attribution math, four-class cost model
  rift-recall-*     recall plane: types (handles, four-state envelope), validity
                    (hash-bound summaries), ledger metrics, ranking, pack compiler,
                    policy, events, bounded MCP, substrate reader (gix seam)
  rift-canopy       dashboard: layout, nic-gauge, command palette, splash, render seam
  rift-git          native git plumbing: worktree discovery, blob-read seam,
                    argv-safe shell-outs, the production IdentitySource
  rift-registry     the machine-global project registry (config-dir resolver)
  rift-agent*       the agent SDK + per-agent adapters (claude-code, codex, …)
  rift-fff / rift-codegraph / rift-nic-gauge   optional packages
bins/
  rift              the `rift` entrypoint (Canopy + subcommands)
  rift-memd         rift-total-recall binary (ingest | serve --mcp | …)
```

Dependency direction is acyclic: everything depends on `rift-core`;
`rift-store` → `rift-hashchain` + `rift-crypto`; `rift-shadow` → store + the
shadow sub-crates; recall crates layer over `rift-recall-types`/`rift-store`
(the ledger additionally backs validity/rank durably — see stage-01
`ERRATA.md` D6); `rift-canopy` → `rift-core` + `rift-registry` (config-dir
resolver only). The committed snapshot `xtask/src/snapshots/
xtask__tests__crate_graph_adjacency.snap` is the reviewable adjacency list.

## Architecture conventions (don't break these)

- **Seams over hard deps for pre-1.0 / heavy externals.** Turso → `CanonicalStore`
  (rusqlite reference impl); the Entire CLI → `EntireRunner`; `gix` →
  `SubstrateReader`; `rmcp` → the `McpServer` dispatcher; `xre-rs` →
  `RenderBackend` (ASCII fallback always available, behind the `xre` feature). The
  *logic* is implemented and tested behind the seam; the external engine plugs in
  without touching callers.
- **The redaction gate is one-way and total.** Raw bytes are `SecretBytes`
  (local-only, zeroized, no `Debug`). Durable writes accept only `RedactedBytes`,
  whose buffer is private and whose sole constructor (`RedactedBytes::seal`) is
  called in exactly one place: `rift-shadow-redact::redact`. Never add a
  `From<Vec<u8>>` or a second constructor.
- **Hashing split:** SHA-256 (`rift-hashchain`, `sha2`) where substrate
  byte-compatibility matters (chain, `content_hash.txt`); blake3 for internal
  handles / worktree hashing. They are distinct id types and never interchange.
- **Errors:** libraries use `thiserror` via `rift_core::RiftError`; bins use
  `anyhow` at the edge. Error variants carry identifiers/reasons only — never key
  or transcript bytes.
- **Config format:** `.rift/config.toml` (TOML, not YAML — the dossier pins
  `toml`; `serde_yaml` is unmaintained). `deny_unknown_fields` everywhere; secure
  defaults when absent; built-in `sensitive_paths` are unioned in and non-droppable.
- **Determinism in tests:** inject the clock/RNG/identity (`UlidSource`,
  `CryptoRng` via explicit nonces, `IdentitySource`). No test reads the wall clock
  or network.

## Lints & tests

- Workspace `#![forbid(unsafe_code)]`; CI denies warnings (`clippy -D warnings`,
  pedantic on; `unwrap`/`expect`/`panic`/`indexing_slicing` flagged). Crate-level
  `#![cfg_attr(test, allow(clippy::unwrap_used, …))]` permits them in tests only.
- Security-critical crates (`rift-shadow-redact`, `rift-crypto`, `rift-hashchain`,
  `rift-recall-policy`, `rift-recall-validity`, `rift-store`) target ≥95% coverage;
  others ≥85%.
- Every crate carries property tests for its invariants (immutability,
  tamper-evidence, validity-never-stale, budget ceilings, scope containment).

## Gotchas

- Commit `Cargo.lock` (this workspace produces binaries).
- `rustfmt.toml` sets `imports_granularity`/`group_imports`, which are
  nightly-only; on stable they warn-and-skip (`fmt --check` still passes).
- The W2 false-memory guard lives in `rift-recall-validity`: a stale summary has a
  different key and `ValidSummary` is unconstructible outside the crate — keep it
  that way; `envelope_for` is the single way to build a served artifact.
- `rift-canopy` builds without `xre-rs` by default; enable the `xre` feature for
  the real 3D backend.
