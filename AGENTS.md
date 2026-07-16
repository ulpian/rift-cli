# AGENTS.md

Instructions for AI coding agents (Codex, Cursor, Gemini, OpenCode, and others
that read `AGENTS.md`). Claude Code reads [CLAUDE.md](CLAUDE.md), the fuller
version; per-agent overrides live in [.agents/](.agents/).

## TL;DR

- **Rift** is a Rust TUI + CLI for AI-assisted engineering: capture every agent
  session into an immutable, tamper-evident, git-native ledger (**rift-shadow**),
  project a memory-delta validity graph agents read deltas from
  (**rift-total-recall**), and show it in a terminal dashboard (**rift-canopy**).
- ~25-crate Cargo workspace under `crates/` + `bins/`. Toolchain pinned in
  `rust-toolchain.toml` (1.96.0; MSRV 1.94). `rusqlite` is `bundled` → needs a C
  compiler.

## Before you push

```bash
./skills.sh check     # fmt --check + clippy -D warnings + test
```

CI runs the same gate across three OSes plus a release build, `cargo deny`, and
`cargo audit`.

## Conventions (do not break)

- **Immutable canonical data.** No `UPDATE`/`DELETE` on canonical tables —
  corrections are new superseding rows. Adding a canonical table means adding it
  to the `rift-store` registry *with* its append-only guard triggers, or CI's
  property test fails.
- **Redaction is a one-way type gate.** Raw bytes are `SecretBytes`; durable
  writes accept only `RedactedBytes`, constructed *only* in
  `rift-shadow-redact::redact`. Never add another constructor or a `From<Vec<u8>>`.
- **Use the seams.** Turso → `CanonicalStore`; Entire → `EntireRunner`; gix →
  `SubstrateReader`; rmcp → `McpServer`; xre-rs → `RenderBackend`. Implement and
  test logic behind the seam; never call the external engine directly elsewhere.
- **Hashing:** SHA-256 (`rift-hashchain`) for substrate-compatible values; blake3
  for internal handles. Distinct id types — never interchange.
- **Errors:** libraries use `thiserror`/`rift_core::RiftError`; bins use `anyhow`.
  Errors carry identifiers/reasons, never key or transcript bytes.
- **Config is `.rift/config.toml`** (TOML, `deny_unknown_fields`, secure
  defaults). Built-in sensitive paths are non-droppable.
- **`#![forbid(unsafe_code)]`** workspace-wide; keep clippy `-D warnings` clean;
  inject the clock/RNG/identity in tests (no wall-clock, no network).
- **Modal-surface parity (Canopy).** Any surface drawn as a centered modal —
  overlay or mode stage — carries the shared top-right close X, and every act
  its hints teach is also clickable on that surface (and every clickable has a
  key). Hints are stage-honest. See CLAUDE.md / ADR-0046.

See [CLAUDE.md](CLAUDE.md) for the full crate map and gotchas, and
[SECURITY.md](SECURITY.md) for the threat model.
