# Rift CLI (`rift`)

A Rust CLI that wraps and extends the [`entire`](https://github.com/entireio/cli) CLI for
AI-assisted engineering provenance. `rift` has three parts:

1. **Wrapper** — proxies the `entire` binary. Any subcommand `rift` doesn't own is forwarded
   verbatim to `entire` (e.g. `rift checkpoint list`, `rift session resume <branch>`,
   `rift doctor`), so `entire` can grow new commands without a `rift` release.
2. **Additional features**
   - `rift scan [PATH]` — detect whether `entire` is set up in a repo (looks for
     `.entire/settings.json`, `.git/entire-sessions/`, and the `entire/checkpoints/v1` branch),
     and optionally `--init` it via `entire enable`.
   - `rift redact` — auto-redact secrets/PII from captured transcripts and prompts
     (`full.jsonl`, `prompt.txt`, `tool_calls.jsonl`). Working-tree copies only; `--dry-run`
     reports findings without writing.
3. **TUI** — `rift tui` opens a [ratatui](https://github.com/ratatui/ratatui) dashboard:
   a thin bottom command bar, a left 20% option list (projects/files), and a right 80%
   metrics panel (token usage, file changes).

## Prerequisites

- A recent stable Rust toolchain (pinned in `rust-toolchain.toml`).
- The `entire` CLI installed and on `PATH` (verified against v0.6.2). Override its location
  with `--entire-bin <path>` or the `RIFT_ENTIRE_BIN` environment variable.

## Workspace layout

| Crate         | Role                                                              |
| ------------- | ----------------------------------------------------------------- |
| `rift-cli`    | The `rift` binary: clap entrypoint + thin command dispatch.       |
| `rift-entire` | Wrapper around the `entire` subprocess (the `EntireRunner` seam). |
| `rift-core`   | Shared domain: `entire` data model, source scanning, git reads.   |
| `rift-redact` | Pluggable redaction pipeline (regex + entropy + custom packs).    |
| `rift-tui`    | The ratatui dashboard.                                            |

## Build & run

```bash
cargo build --workspace
cargo run -p rift-cli -- --help
cargo run -p rift-cli -- status            # forwarded to `entire`
cargo run -p rift-cli -- scan .            # detect an entire setup
cargo run -p rift-cli -- tui               # launch the dashboard
```

## Develop

Common tasks run through the [`skills.sh`](skills.sh) runner (see
[SKILLS.md](SKILLS.md)):

```bash
./skills.sh check      # pre-push gate: fmt-check + clippy -D warnings + test
./skills.sh test
./skills.sh run scan . # run the binary with args
./skills.sh list       # all tasks
```

Raw cargo works too (`cargo test --workspace`, etc.).

## For AI agents

- [CLAUDE.md](CLAUDE.md) — full project guidance (Claude Code).
- [AGENTS.md](AGENTS.md) — condensed cross-tool guidance; `.agents/` holds
  per-agent overrides.
- [.mcp.json](.mcp.json) — project MCP servers (empty by default; add entries as
  needed).
