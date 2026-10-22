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
./skills.sh gc        # sweep nested e2e targets, incremental caches, and stale queue locks
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
- **Use the seams.** Turso → `CanonicalStore`; gix →
  `SubstrateReader`; xre-rs → `RenderBackend`. Recall serving goes through the
  transport-independent `ServeDispatcher`. Implement and test logic behind the
  seam; never call an external engine directly elsewhere.
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
  key). Hints are stage-honest. See CLAUDE.md.
- **Worktree & resource hygiene.** Keep `~/.local/bin/rift`, `.git/hooks/*`, and
  agent hooks pointed at `target/release/rift` (`./skills.sh release`), never
  `target/debug/rift` (preserve `.codex/hooks.json` trust hashes). Never run
  `git worktree remove --force` without verifying `git status --porcelain` and
  `git log main..HEAD`; delete the matching `~/.cargo-targets/rift-<stream>`
  cache when retiring a merged worktree and run `./skills.sh gc`. Serialize full
  `./skills.sh check` runs across worktrees.
- **Token & subagent hygiene.** Never poll background commands or log files across
  turns (block inside a single command with `while kill -0 "$PID" 2>/dev/null; do sleep 30; done`).
  Never bypass the native `Read` tool with Bash `cat`/`sed`/`head`/`tail` on
  source files. Never reuse a subagent across multiple briefs (`/compact` does
  not compact subagents; retire subagents before 200K tokens and spawn fresh
  ones per brief).

See [CLAUDE.md](CLAUDE.md) for the full crate map and gotchas, and
[SECURITY.md](SECURITY.md) for the threat model.

<!-- RIFT-MANAGED:codex:begin -->
Rift automatically captures this repository and delivers bounded recall context through native hooks.
- `rift agent-help` — inspect the live recall and recovery surface.
- `rift recall status --json` — verify hook, runtime, context, and degradation status.
Documents (.docx, .pdf, .xlsx and the like) are converted once for this repository and kept as Markdown under `.rift/state/doc/`; a hook points a document read there, or names the file to read in its place. Read that Markdown rather than the binary, or ask `rift recall doc-read <path>` for it; where there is none, read the document as usual.
If Rift reports degradation, let the raw agent call run and use the diagnostic above; correctness never depends on this file.
<!-- RIFT-MANAGED:codex:end -->
