# CLAUDE.md

Project guidance for Claude Code (and other agents — see [AGENTS.md](AGENTS.md)).

## What this is

`rift` is a Rust CLI that **wraps and extends the `entire` CLI** (a separately
installed Go binary, `github.com/entireio/cli`). It has three parts:

1. **Wrapper** — forwards any unrecognized subcommand verbatim to `entire`.
2. **Extra features** — `rift scan` (detect/enable an entire setup) and
   `rift redact` (redact secrets/PII from captured transcripts/prompts).
3. **TUI** — `rift tui`, a ratatui dashboard (option list + live usage metrics +
   command bar).

It is one repo in a multi-repo product (sibling `../rift-site` is the web app;
`Rift_Product_Spec.md` is the product source-of-truth).

## Prerequisites

- Rust toolchain is pinned in `rust-toolchain.toml` (currently **1.96.0**).
  ratatui 0.30 requires Rust ≥ 1.86, so do not downgrade below that.
- The `entire` binary must be on `PATH` for wrapper/status/tui commands.
  Override with `--entire-bin <path>` or `RIFT_ENTIRE_BIN`. Tested against
  `entire` v0.6.2.

## Commands

Prefer the task runner; it is the single source of truth for common workflows:

```bash
./skills.sh list        # show all tasks
./skills.sh check       # fmt --check + clippy -D warnings + test  (run before pushing)
./skills.sh test        # cargo test --workspace
./skills.sh lint        # cargo clippy --workspace --all-targets --all-features -D warnings
./skills.sh fmt         # cargo fmt --all
./skills.sh build       # cargo build --workspace
./skills.sh run -- scan .   # run the rift binary with args
```

Raw cargo equivalents work too (`cargo test --workspace`, etc.). CI runs the
same gates as `./skills.sh check` plus a release build.

## Workspace layout

| Crate         | Role                                                                 |
| ------------- | -------------------------------------------------------------------- |
| `rift-cli`    | The `rift` binary: clap entrypoint + thin command dispatch.          |
| `rift-entire` | Wrapper around the `entire` subprocess (the `EntireRunner` seam).    |
| `rift-core`   | Shared domain: data model, scanning, JSONL, git reads, branch metrics. |
| `rift-redact` | Pluggable redaction pipeline (secret-regex + entropy + PII + packs). |
| `rift-tui`    | The ratatui dashboard (app state, event loop, widgets, refresh).     |

Dependency direction is acyclic: `rift-cli` → all libs; `rift-tui` →
`rift-core`/`rift-entire`; `rift-redact` → `rift-core`; `rift-core` → none.

## Architecture notes & conventions

- **Wrapper seam:** all `entire` invocations go through the `EntireRunner` trait
  (`rift-entire/src/runner.rs`); never call `std::process::Command` for `entire`
  directly. Tests use `MockRunner` or a fake `entire` script.
- **clap:** rift owns only `scan`, `redact`, `tui`, `status`; everything else is
  an `external_subcommand` forwarded to `entire`. Global flags (`--entire-bin`)
  must precede an external subcommand.
- **Git reads:** read-only access to the `entire/checkpoints/v1` branch goes
  through the `GitReader` trait in `rift-core/src/git.rs` (shell-out to `git`,
  no `git2`/`gix`). `rift-core::metrics::read_branch_metrics` aggregates token /
  file-change / tool-call counts off that branch for the TUI.
- **Redaction is working-tree-only:** `rift redact` rewrites checked-out copies
  with atomic temp-file + rename and supports `--dry-run`. It never rewrites the
  orphan-branch history.
- **TUI never blocks on IO:** metrics load on a background thread
  (`rift-tui/src/refresh.rs`) and arrive via a channel; the draw loop is sync.
- **Error handling:** libraries use `thiserror`; the binary uses `anyhow`.
- **Lints:** workspace denies warnings in CI (`clippy -D warnings`) and forbids
  `unsafe_code`. Keep code clippy-clean.
- **Tests:** pure logic is unit-tested inline; cross-process behavior uses
  integration tests with a fake `entire` (`tests/fixtures/fake-entire/`) and a
  synthetic repo fixture (`tests/fixtures/repo/`).

## Gotchas

- ratatui 0.30 widget APIs differ from older versions: `Sparkline::data` takes an
  `IntoIterator<Item: Into<SparklineBar>>`; `BarChart::data` takes
  `impl Into<BarGroup>`; use `frame.area()` and `Layout::*.areas()`.
- `entire hooks` is a flat internal command, not `hooks <agent> <verb>`; agent
  hooks are managed via `entire agent add/remove/list`.
- Commit `Cargo.lock` (this workspace produces a binary).
