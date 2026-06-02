# AGENTS.md

Instructions for AI coding agents working in this repo (Codex, Cursor, Gemini,
OpenCode, and others that read `AGENTS.md`). Claude Code reads
[CLAUDE.md](CLAUDE.md), which is the fuller version of this file; agent-specific
overrides live in [.agents/](.agents/).

## TL;DR

- Rust workspace for `rift`, a CLI that wraps the `entire` CLI. Crates:
  `rift-cli` (binary), `rift-entire` (wrapper), `rift-core` (domain),
  `rift-redact` (redaction), `rift-tui` (dashboard).
- Toolchain is pinned in `rust-toolchain.toml` (≥ 1.86 required by ratatui).
- The `entire` binary must be on `PATH` (or set `RIFT_ENTIRE_BIN`).

## Before you push

Run the full gate — it must pass:

```bash
./skills.sh check     # fmt --check + clippy -D warnings + test
```

Individual tasks: `./skills.sh {fmt,lint,test,build,run}` (see
[SKILLS.md](SKILLS.md)). CI runs the same gate plus a release build.

## Conventions (do not break)

- Call `entire` only through the `EntireRunner` trait, never `Command` directly.
- New `entire` subcommands need no code — they fall through `external_subcommand`.
- Git access is via the `GitReader` trait (shell-out), not `git2`/`gix`.
- `rift redact` edits working-tree copies only (atomic write + `--dry-run`).
- Libraries use `thiserror`; the binary uses `anyhow`. Keep code clippy-clean
  (`-D warnings`); `unsafe` is forbidden.
- Commit `Cargo.lock`.

See [CLAUDE.md](CLAUDE.md) for the full architecture notes and gotchas.
