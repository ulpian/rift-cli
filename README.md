# Rift

**A terminal dashboard and memory layer for AI coding agents.**

Rift records every agent session in your repository as a tamper-evident,
git-native history, and stops your agents paying again for context they already
hold. When an agent re-reads a file it has already seen, or reruns a command
whose output it already has, Rift hands back only what changed.

```text
rift                      open the Canopy dashboard for this repository
rift doctor               check that capture and recall are wired correctly
rift recall status        see what Total Recall knows and what it has saved
```

> **Alpha.** This is `v1.3.1-alpha`. Expect rough edges, and please
> [send findings](#feedback): that is what this alpha is for.

---

## Install

**macOS and Linux**

```sh
curl -fsS https://storage.googleapis.com/rift-e59ea04337d9/v1.3.1-alpha/install.sh | sh
```

**Windows (PowerShell)**

```powershell
irm 'https://storage.googleapis.com/rift-e59ea04337d9/v1.3.1-alpha/install.ps1' | iex
```

The installer picks the build for your OS and CPU and checks its SHA-256. It
installs `rift` in `~/.local/bin` (`%LOCALAPPDATA%\Rift\bin` on Windows), adds
that folder to your `PATH`, and opens a short setup. You don't need Rust, and
you don't need administrator rights.

When it finishes, `rift` works in the same terminal. On macOS and Linux the
installer opens a fresh shell for this. You can set these before running it:

| Variable | Effect |
|---|---|
| `RIFT_BIN_DIR=/path` | Install somewhere other than the default folder. |
| `RIFT_NO_MODIFY_PATH=1` | Leave your shell startup files and `PATH` alone. |
| `RIFT_NO_SHELL_RESTART=1` | Keep the `PATH` line but don't start a new shell. |
| `RIFT_NONINTERACTIVE=1` | Skip all prompts (CI, scripts). |

**Platforms:** macOS 11+ on Apple silicon and Intel; Linux x86-64 and ARM64
(glibc 2.28 or newer, so not Alpine/musl); Windows 10+ x64.

## Quick start

```sh
cd your-project        # any git repository
rift
```

On the first run in a repository, Rift shows what it is about to set up: which
agents it found, which hooks it will add, and your capture and privacy
settings. Nothing is written until you confirm. After that, use your coding
agent as you normally would. Rift captures each session in the background and
answers repeat reads with only what changed.

Every feature is on by default. To turn one off for a repository, open
**Project Settings** in the dashboard (press `s`).

### Supported agents

| Agent | Capture | Total Recall |
|---|---|---|
| Claude Code | ✅ | ✅ |
| Codex | ✅ | ✅ |
| Antigravity | ✅ | ✅ |
| Gemini CLI, Cursor, Copilot CLI, OpenCode, Factory Droid, Ollama | detected, not yet wired | — |

Rift won't half-wire an agent. If it can't capture an agent reliably, it says
so and leaves that agent's configuration untouched.

## What you get

**Canopy, the dashboard.** Type `rift` in a repository. It shows your
projects and worktrees, what each agent is doing, how full each context window
is, token usage and cost, and how much Recall has saved. In the session viewer
(`r`), prompts, file edits (folded per file, with arrow keys to open and close
them) and recalls are colour-coded.

**rift-shadow, the record.** Every session's prompts, tool calls, edits,
token counts and checkpoints go into an append-only, hash-chained ledger
stored in git. Secrets are redacted before anything reaches disk.

**Total Recall, the savings.** Rift tracks what each agent's context window
already holds. When the agent:

- **re-reads a file it already has**, Rift returns only the lines that changed,
  or a short note saying nothing did;
- **reads a large file for the first time**, Rift gives it an outline first;
- **runs shell commands**, Rift looks at the output of each command in a chain
  like `cd x && cat a.rs && cargo test` separately. Lines the agent has already
  seen are replaced with a reference to them. Long runs of repeated build noise
  are folded, but errors, warnings and test summaries never are.

The original output is always saved before anything is changed. Savings are measured on your repository, not estimated:
`rift recall status` shows them.

## Everyday commands

| Command | What it does |
|---|---|
| `rift` | Open the dashboard. |
| `rift doctor` | Check this repository's setup and print fixes. |
| `rift recall status` | Recall's state and measured savings. |
| `rift viewer` | Read-only browser for checkpoints, transcripts and recalls. |
| `rift install` | Change your default agents and preferences. |
| `rift worktree status` | Check how a linked git worktree is wired. |
| `rift --help` | Everything else. |

**Update:** `rift update` installs the newest release and keeps your settings.
You don't need to remember to run it: once a day, `rift` and `rift init`
check for a newer release and ask before installing it. Set
`RIFT_NO_UPDATE_CHECK=1` to turn the check off. An older release never
replaces a newer one.

**Uninstall:** `rift uninstall --dry-run` shows what would be removed, and
`rift uninstall` removes it, including the `PATH` line the installer added. Your
captured history is kept unless you add `--purge`.

## Privacy and security

- **Everything stays on your machine.** Rift has no telemetry and no account,
  and the shipped binary doesn't include a networking library.
- **Redaction happens before storage.** The type system makes writing
  unredacted bytes to disk a compile error. Raw transcripts are opt-in and come
  with a warning.
- **Stored summaries are encrypted.** Each project has its own key, kept
  apart from Rift's cache, so a copy or backup of the cache can't be read on
  its own.
- **Agent settings stay out of git.** Claude Code saves commands you approve
  with "don't ask again", secrets included, in `.claude/settings.local.json`.
  Rift makes sure git ignores that file and warns you if it is already
  committed.
- **History is append-only.** Corrections are added as new records, and hash
  chains and Merkle roots make tampering visible.
- **Downloads are pinned and signed.** The installers and `rift update` only
  accept release links from Rift's own bucket and verify every archive's
  checksum. `rift update` and the daily update check also verify the release
  manifest's Ed25519 signature against a key built into `rift`, so a
  tampered bucket can't push an update.

The alpha builds are not code-signed or notarised. See [SECURITY.md](SECURITY.md)
for the threat model and how to report a vulnerability.

## Feedback

Found a bug or something confusing? In the affected repository, run:

```sh
rift doctor --report
```

It prints a report you can paste into a bug report. The report contains counts,
states and versions only: every path, name and message is replaced by its
length, and nothing is sent anywhere. Add your OS, terminal and agent version,
what you expected and what happened.

---

## Build from source

You need Rust 1.94 or newer (the toolchain is pinned in `rust-toolchain.toml`)
and a C compiler, because SQLite is compiled in.

```sh
git clone <this repository> && cd rift-cli
cargo build --release -p rift      # → target/release/rift
cargo run -p rift                   # launch the dashboard from source
./skills.sh check                   # the pre-push gate: fmt, clippy -D warnings, tests
```

The repository is a Rust workspace:

| Area | Crates |
|---|---|
| Entry point | `bins/rift` (dashboard + CLI), `bins/rift-memd` (projection maintenance) |
| Capture | `rift-shadow*`, `rift-store`, `rift-hashchain`, `rift-crypto` |
| Recall | `rift-recall-*`, `rift-tokens` |
| Dashboard | `rift-canopy`, `rift-canopy-data`, `rift-nic-gauge` |
| Agents | `rift-agent` (SDK) and one `rift-agent-*` adapter per agent |
| Shared | `rift-core`, `rift-config`, `rift-git`, `rift-registry` |

Two rules hold across the whole codebase. Canonical data is **immutable**:
the type, SQL-trigger and property-test layers all forbid `UPDATE` and
`DELETE`. And **security is a per-line property**: every durable write takes
redacted bytes only.

**Read next:**

- [The guide](docs/guide/00-start-here.md): what each part does, and why.
- [Agent integrations](docs/guide/10-agent-integrations.md): hook and skill
  wiring for each agent.
- [CLAUDE.md](CLAUDE.md) and [AGENTS.md](AGENTS.md): guidance for AI agents
  working in this repository.
- [Release notes](release/notes/v1.3.1-alpha.md)

## License

Apache-2.0.
