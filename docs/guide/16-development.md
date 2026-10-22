# 16 — Developing Rift

For the person changing the code: how to build it, what each check proves, and
which bites first if you skip the local run. [§16.1](#161-the-five-minute-version)
gets you building; [§16.13](#1613-machine-hazards-macos-icloud-shell--and-windows)
covers failures that are not your code's fault. Related: [17-invariants.md](17-invariants.md) (the rules the checks defend), [18-what-rift-does-not-do.md](18-what-rift-does-not-do.md).

---

## 16.1 The five-minute version

```bash
git clone <the repository>
cd rift-cli
./skills.sh build # first build compiles bundled SQLite; give it a few minutes
./skills.sh check # format + lint + the whole test suite
```

- **The pinned toolchain.** `rust-toolchain.toml` pins **1.96.0** with `rustfmt`,
  `clippy`, `rust-src`. The minimum supported version is **1.94**: a promise to
  downstream builders, proved by its own CI job, not the version you develop on.
- **A C compiler.** `rusqlite` is built `bundled`. The append-only guarantee rests
  partly on SQL triggers, so a store whose dialect depends on what the host
  packaged has host-dependent guarantees ([13](13-storage-and-schema.md)).

---

## 16.2 The task runner

`./skills.sh` is the one entry point: a dependency-free bash script (no `jq`, no
`make`) that runs from its own directory. `./skills.sh list` prints the tasks:
`fmt`, `fmt-check`, `lint` (clippy over the workspace, all targets and features,
`-D warnings`), `test`, `build`, `release`, `check` (the pre-push gate: `fmt-check`
+ `lint` + `test`), `run`, `tui`, `memd`, `smoke`, `docs` (rustdoc with
`-D warnings`), `coverage` (needs nightly, [§16.6](#166-coverage-as-a-gate-not-a-dashboard))
and `gc` (stops detached workers and prunes stale build caches).
[`SKILLS.md`](../../SKILLS.md) (humans), `skills.json` (machines) and `skills.sh`
list the same tasks and must agree; a manifest that drifts from the runner is
worse than none.

---

## 16.3 What CI runs

`.github/workflows/ci.yml` grants `contents: read` only and cancels superseded
runs. Its **fifteen jobs** are all named in `REQUIRED_JOBS`
(`xtask/src/ci_meta/workflow.rs`) and checked by `ci-meta`, so a job cannot be
deleted while CI stays green.

| Job | Guards |
|---|---|
| `fmt` | formatting is mechanical |
| `clippy` | `-D warnings`, then `rift` with **no** default features: an item that exists only behind a feature is dead code in the all-off build somebody ships (a clippy pass, not `cargo check`, so it denies the same warnings and builds tests) |
| `test` | the suite on Linux, macOS and Windows with `fail-fast` off, so a Windows-only break survives a Linux failure |
| `msrv` | the declared minimum (1.94) compiles |
| `coverage` | [§16.6](#166-coverage-as-a-gate-not-a-dashboard) |
| `security` | `cargo deny` and `cargo audit` ([§16.10](#1610-supply-chain)) |
| `ci-meta` | the guards on the guards, then `actionlint` ([§16.4](#164-the-guards-on-the-guards)) |
| `gate-intercept` | the read-interception surface's shape ([§16.4](#164-the-guards-on-the-guards)) |
| `bench-harness` | `benches/native-baseline`, a non-member of the root workspace, so this is its only run; full-history checkout because its staleness arm asks git whether a measurement's subject is an ancestor of the tip ([15](15-benchmarks.md)) |
| `fuzz-smoke` | every untrusted-byte parser survives 60 s on nightly |
| `red-team` | the injection suites across validity, serve and pack: a forbidden span leaking into served output blocks the merge |
| `docs` | `cargo doc -D warnings` and `xtask rustdoc-baseline`: a doc comment cannot rot into an unfollowable link |
| `dialect-conformance` | the legacy-import golden suites and the walker suite, by name, so none is dropped from an unnamed slice of `test` |
| `miri` | crypto, hash-chain and dashboard crates; advisory, with `continue-on-error` so the failure stays visible rather than swallowed by `\|\| true` |
| `release-build` | the optimised profile compiles |

`coverage`, `fuzz-smoke` and `miri` need nightly.

---

## 16.4 The guards on the guards

`cargo run -p xtask -- ci-meta` reads the repository (source, the git index, the
resolved dependency tree); it runs no tests, opens no database, reaches no
network. Its eighteen checks each return violations; any fails the command:

1. **Required jobs present**, so deleting a job is a visible diff.
2. **Read-only workflow permissions**: no `write` scope.
3. **Lint table equals `ci/lint-baseline.txt`**: a lint change is a two-file edit.
4. **No `unsafe`** outside a sanctioned (empty) list.
5. **No blanket allow at a crate root**: it silences code nobody has written yet.
6. **No schema fast path in `bins/`**: the schema is created only through the checksum-locked migration engine.
7. **Every fuzz target runs in CI.**
8. **Property generators can reach a newline**, since capture, packs, trailers and config are line-oriented .
9. **No network client in the capture closure**, in any dependency section or spelling.
10. **No network client in either shipped binary's resolved graph**, catching one behind an intermediary.
11. **The conformance job runs exactly the declared suites.**
12. **The fuzzy matcher keeps the feature that holds its vectorised backends inside their bytes**.
13. **Deleted paths stay deleted**: on disk, in the index, or unignored, so a bulk `git add -A` cannot resurrect one.
14. **The vocabulary screen**: planning shorthand in any tracked path or content, with no exclusions (this guide is in the corpus) and failing on a scan of nothing. The short id shapes are screened only over this guide and the root documents, because elsewhere they collide with hashes, benchmark arms and fixtures. The fix is words, never an exclusion. `docs/` is *excluded* from the redaction false-positive census (a prose corpus full of hashes would swamp it) and *included* here: two questions, two corpora ([04](04-redaction-and-secrets.md)).
15. **Size ceilings**: no tracked `.rs` file over 1,500 lines or function over 80, tests and `xtask` included, comments and strings masked.
16. **An `#[ignore]` carries its reason.**
17. **One cache root in the `rift` binary** (`family::cache_root`, one temp directory per test process), so a unit test cannot write into the developer's real cache.
18. **The guide stays within 400 KiB**: the byte length of every tracked file under `docs/guide/`, refused on a scan that found none. A chapter that grows replaces older text, or raises the ceiling in a diff a reviewer reads.

`xtask gate-intercept` is the read-interception guard: nine read-only text screens over the tracked tree, no build. They hold that one
file composes a hook response, the answered channels are a reviewable list, no
decision opens a store or loads the tokenizer, no destructor in a hook-path crate
does either (read directly, since a destructor runs wherever its value dies), one module writes the sentences a
model reads, every rewrite is a pinned form, caches are keyed by family with one
named reader, and no hook reaches a managed-region writer. They err toward
reporting; `cargo test -p xtask` proves each catches a planted violation.

---

## 16.5 The lint policy

Lints live once in the workspace table; members inherit them. `unsafe_code =
"deny"`; Clippy `all` and `pedantic` at `warn` with lower priority, so a specific
lint wins. `unwrap_used`, `expect_used`, `panic` and `todo` are on because a panic
in a git hook must not abort a developer's git operation; `dbg_macro` because
debug output has no place in a capture path; `indexing_slicing` so there is no
un-audited `a[i]` (sites indexed by construction-checked bounds carry a narrow
local allow with a reason). Stylistic pedantic lints are off, as style warnings left on train people to ignore warnings; CI's `-D warnings` makes every `warn` an error.

**Tests are allowed to be blunt** (`#![cfg_attr(test, allow(clippy::unwrap_used,
…))]`): a test threading `Result` through every line is a test nobody reads.
**Changing a lint level is a two-file change**, `Cargo.toml` and
`ci/lint-baseline.txt`, so a downgrade cannot ride in an unrelated diff.
`rustfmt.toml` sets 100 columns; its nightly-only import options warn and skip on
the pinned channel while `fmt --check` still passes.

---

## 16.6 Coverage as a gate, not a dashboard

Floors: workspace line **≥ 85%** (`cargo llvm-cov report --fail-under-lines 85` in
CI), workspace branch **≥ 75%** and per-crate line **≥ 95%** for the
security-critical set (both `xtask check-coverage`). That set is
`ci/security-crates.txt`, one crate per line with its reason: crypto, hash chain,
store, recall policy, redaction, validity, the document converter (fourteen
parsers reading files anybody could have written) and file search (it decodes an
index from a directory the served agent can write).

**It fails closed**: a listed crate missing from the report is a violation, and so
is a report without branch data; a gate that passes on absent input is not a gate.
One file is excluded, the OS keychain boundary: pure OS I/O with
nothing testable on the near side.

`./skills.sh coverage` cleans (`cargo llvm-cov clean --workspace`: **stale profile
data gives plausible fiction**), runs the instrumented suite once with `--branch`
and hands the JSON to `check-coverage`. It needs `+nightly`, since `--branch` passes
a flag the pinned channel refuses and the gate would then be neither passable nor
failable; only CI applies the 85% floor. 

---

## 16.7 Determinism in tests

A test reading the network, the wall clock or the machine eventually fails for a
reason unrelated to the change that triggered it, so the workspace injects those
inputs. No test reads the network, and no assertion depends on the wall clock: a
test may read it only to bound a wait or to seed a fixture, never to decide its
verdict.

- **Identifiers:** `UlidSource` (`crates/rift-core/src/id.rs`); production's
  `MonotonicUlidSource` keeps a monotonic floor so a backwards clock still
  increases, tests use `CounterUlidSource`.
- **Identity:** `IdentitySource` (`crates/rift-core/src/identity.rs`); tests use
  `FixtureIdentity`. The trait has **no network method**, so derivation is local by
  construction.
- **Randomness:** nonces are 96 bits and reuse is catastrophic for the cipher; a
  test needing a *specific* ciphertext builds the nonce from raw bytes (the caller
  owns uniqueness), as the known-answer test against a published vector does.
- **Time:** no workspace clock trait; time enters as an explicit `Timestamp`
  argument. The one clock seam is the splash sequencer, with a deliberately slow
  clock proving the animation degrades on a slow terminal
  (`crates/rift-canopy/src/splash/director.rs`).

---

## 16.8 The invariant tests you must not weaken

Breaking these means the guarantee is gone, not that a test is inconvenient.

**The append-only keystone** (`crates/rift-store/tests/immutability.rs`): after a
full migration every real table must be a registered canonical table with *both*
guard triggers (infra tables are underscore-prefixed). A canonical table added
without its guard turns CI red; the canary `forgotten_registration_is_caught`
forgets one on purpose and asserts the keystone flags it, because a guard without
a calibration is a guard nobody has seen bite.

**The dependency graph** (`cargo test -p xtask`): acyclic (with a back-edge
calibration), only the store crate may depend on a storage backend, members match
directories and inherit the workspace, the toolchain is pinned. The snapshot
`xtask/src/snapshots/xtask__tests__crate_graph_adjacency.snap` makes a new edge a
visible diff line: regenerate and *read the diff*.

**The shrink-only baselines** (`ci/rustdoc-baseline.txt`,
`ci/proptest-newline-baseline.txt`) have **two arms**: an unlisted site is a
violation, and a listed site that stops warning is *also* one, so a baseline never
becomes a parking lot. Both are empty. For the proptest one, adding the "dot
matches newline" flag is half the fix: if the property then fails, report it,
never narrow the generator to make it pass.

---

## 16.9 Fuzzing

`fuzz/` is a **separate workspace**, so `cargo build --workspace` on stable never
builds it; it needs nightly. Fifteen targets: `config_parse`, `run_hook`,
`redact`, `redact_jsonl`, `redact_fallback`, `fuzz_pack_load`, `diff`, `glob_match`,
`checkpoint_artifact`, `mcp_request`, `palette_parse`, `doc_convert`,
`shell_read`, `shell_read_bash`, `search_query`. **An untrusted-byte parser gets a target when it
lands** and must be total: a value or a typed error for any input, never a panic.
`doc_convert` is the largest parser surface; `shell_read` guards the recogniser
that runs first on the read path, where a panic is a hook dying on a command line;
`search_query` covers the model's query and the index file in a tree the served
agent can write; `redact_fallback` drives the per-line fallback through a pack that
never converges; `shell_read_bash` is differential: every line the recogniser
accepts, `bash -n` accepts too, since a booked read the shell would not run is a
claim about output never produced. `mcp_request` fuzzes the serve request decoder behind the recall
skill CLI; renaming it would discard its corpus.

Run one with `cd fuzz && cargo +nightly fuzz run config_parse -- -max_total_time=60`.

---

## 16.10 Supply chain

`cargo deny check` (`deny.toml`) and `cargo audit --deny warnings`
(`.cargo/audit.toml`) are kept in lockstep: an ignore in one and not the other
stops applying when the other runs.

- **Advisories.** Yanked crates are denied. Each ignore comments why it does not
  reach us and what would make it relevant: `bincode` (the syntax highlighter's
  required feature) and `ttf-parser` (the PDF path; no safe upgrade exists), both
  unmaintained, not vulnerable, never fed untrusted bytes.
- **Bans.** Wildcard requirements are denied, duplicates warn, the system TLS
  bindings are denied (a pure-Rust stack is preferred).
- **Licences.** A short permissive allow list; copyleft goes through *annotated
  per-crate exceptions*, never by widening it: `uluru` (MPL-2.0) and `notify`
  (CC0-1.0, more permissive, excepted only because the list is never widened).
- **Sources.** Only crates.io; unknown registries and git sources are denied.

---

## 16.11 The git hooks in your checkout

When Rift captures a repository, **five** git hooks are installed
(`crates/rift-shadow-hooks/src/install/mod.rs`): `prepare-commit-msg`,
`commit-msg`, `post-commit`, `pre-push`, `post-checkout`. On Unix each is a link to
`rift`, routed by invoked name; on Windows a shim. A `.rift-managed-link` sidecar
records the exact target, which is how Rift tells its own link from somebody
else's hook; a pre-existing hook is chained aside, never overwritten
([03](03-capture.md)). `rift disable|enable --agent <name>` act on an *agent
client's* wiring, not the git hooks; there is no "disable the git hooks" verb, and
the health probe reports a family missing one as unhealthy so the next install repairs it.

**Holding the hooks aside** (a long scripted run) is not a product feature: rename
them yourself and back, leaving the sidecars alone.

```bash
cd "$(git rev-parse --git-dir)/hooks"
for h in commit-msg post-checkout post-commit pre-push prepare-commit-msg; do
  mv "$h" "$h.held" # reverse the two names to restore afterwards
done
```

While held, nothing is captured and `rift doctor` reports them missing. **Never**
run the hook installer meanwhile: its hidden entry point does not honour `--help`,
so asking for help performs a real install and re-links all five.

---

## 16.12 Adding things: three short checklists

**A canonical table.** Write an additive migration (never edit a shipped one: the
engine is checksum-locked and a changed migration poisons the projection),
register the table so it gets both triggers, run `cargo test -p rift-store`; the
keystone names a skipped registration ([13](13-storage-and-schema.md)).

**A crate.** Create it under `crates/`, add it to `[workspace].members`, set
`lints.workspace = true`, run `cargo test -p xtask` (it names what you forgot) and
review the adjacency-snapshot diff as carefully as the code. **Measure the shipped
binary in the commit that adds a dependency**: the stripped Linux release `rift` is
held under **30,000,000 bytes**, a review rule, not a CI check. The document engine (about 5.9 MB) is in `rift` for size accounting but **never runs in a hook's process**: conversion happens in `rift` re-spawned on its hidden
`hooks doc` route, so the deadline is a kill and a hostile document dies in another
address space. Search stays in-process, since an answer that must start a second
process cannot fit a hook's budget.

**A check.** A check that can be opted out of per item is not a check: deleting a
test, ignoring it and hiding it behind a feature flag all pass a workspace run. So
a durable check drives the shipped binary or reads a durable artifact; where it
delegates to a test it asserts that test exists **and** is not ignored; it lands
with a calibration that restores the defect and watches it bite; it counts its
corpus and refuses a scan that examined nothing. If it runs Rift's diagnostics,
point the cache at a sandbox: `verify` and `doctor` write a verification record
into the projection they resolve, which can inflate a clean-run streak.

---

## 16.13 Machine hazards: macOS, iCloud, shell — and Windows

Not Rift bugs: environment properties that each cost somebody real time.

**An iCloud-synced checkout**: `git add .` hangs on evicted ("dataless") files, so
stage explicit paths; deleted tracked files come back when the sync replays; large
trees fault in on enumeration (`brctl download .` before a bundle or gc); tests
shelling out to `git` flake under contention (re-run alone). Keep the checkout and
`target/` outside the synced tree.

**Tools.** macOS `grep` has no `-P` (use `ggrep -nP`); `git grep -E` with `\b`
silently matches nothing (use `-P`, or you "prove" an absence that is not there);
`zsh` does not word-split `$var` in `for`; `bash` is 3.2 with no `mapfile` or
`timeout(1)` (use `perl -e 'alarm N; exec @ARGV' -- cmd`). **Never wrap a measured command in a pipe**: `$?` becomes the tail's status. **Check whose process it is before killing it**: two abandoned runs on one build lock look like a colleague's (`lsof -a -d cwd -p <pid>` names the directory). **A stale `target/debug/rift`
firing as a hook after you edit a migration applies the old SQL under the new
checksum**; rebuild before schema work.

**Windows**, each with its fix:

- `canonicalize` returns `\\?\` paths and Git for Windows refuses them in
  `GIT_DIR`/`--git-dir`/`GIT_INDEX_FILE`: compare paths, never strings, and spawn
  git with `rift_git::child_process_path`.
- Hard links, which mint a family identity, need NTFS; `RIFT_FAMILY_DIR` relocates the manifest root.
- No host sets `COLORTERM`, so the console decides the render tier; use Windows Terminal 1.9+ for the mouse.
- Repository-relative paths are `/`-spelled on every platform, a property of the
  string (a shard bundled on Windows and rebuilt on Linux carries `\` as a byte):
  `repo_relative_slash` and `slash_spelled` in `rift_recall_substrate::worktree`,
  applied at ingest and lookup.

- NTFS directory mtimes read from a parent listing lag, so a fresh search index looked stale: `dir_mtime_ms` in `rift-recall-search` stats the directory itself.
- Claude Code folds `\` and `:` as well as `/` and `.` in its project directory name (`c:\Users\…\x` becomes `c--Users-…-x`): fold the plain spelling, never the verbatim one, or the name is absolute and `join` drops the projects directory.
- The shell-read grammar refuses `\` and `?` by design (widening it fights the
  fuzz target): fixtures use `shell_spelling`.
- Build fixture JSON with `serde_json::json!`, join `PATH` with `std::env::join_paths`, and gate symlink tests `#[cfg(unix)]`.

- `-D warnings` makes a Unix-only item dead code there: gate it with its users, never `allow(dead_code)`.

---

## 16.14 A savings reading through the real clients

Everything above measures Rift against fixtures; one command measures it against
the clients (`cargo run -p xtask -- dogfood --help` lists its flags):

```bash
cargo run -p xtask -- dogfood --client all --sessions 10 --keep
```

It plays fixed scripted sessions through Claude Code, Codex and Antigravity and prints tokens read, avoided and returned, the honesty figure and its share ([§8.7](08-cost-and-savings.md#87-the-honesty-ladder)) and the decisions by kind.

**It spends real API quota**, so it refuses to run under `CI`, prints the spend
first, is unreachable from `cargo test`, and no workflow calls it. A missing or out-of-quota client is **UNAVAILABLE** and the run continues.

**It is isolated from your repository.** Each client works in its own clone under
`~/rift-dogfood/`, which `rift init` gives a family of its own
([9](09-worktrees-and-families.md)); the harness refuses to continue if the
clone's projection is yours, since a reading that wrote into your evaluation window
is worthless. Claude Code runs with `--setting-sources project,local` so only the clone's wiring loads, and Antigravity with `--new-project`, without which it continues the previous conversation and the sessions stop being independent. Codex runs with hook trust bypassed for one invocation but
deliberately *not* the approvals-and-sandbox bypass, which writes a trust row into
`~/.codex/config.toml`; that file is copied aside and restored byte for byte. Documents are primed by hand first (ask, drain the conversion journal, ask again), because a first read of an unconverted document is past the gate's deadline and would measure a race.


**What the figure is:** the levers' worth on a fixed sample, not deterministic (models choose their own tool calls) and not the savings window's ordinary-session evaluation, which is the number to quote. Read it with its `n`.

**`--compare`** plays every session twice in two clones, one with `mode = "off"`
in `[interception]`, `[search]` and `[doc]`, one as shipped, and tabulates read
tokens off and on, the **measured** removal, what the levers **booked**, and
`booked ÷ measured`. The pair exists because the booked share records what Rift
decided, not what would have been read without it. Treat the ratio as a check on
the booked figure's size, never a verdict: **the arms are not two runs of one
experiment**, so a difference carries the levers and the day's choices together,
and a negative removal is printed as the negative it is. 

---

## 16.15 Where to look next

[../../CLAUDE.md](../../CLAUDE.md) · [../../SECURITY.md](../../SECURITY.md) ·
[17-invariants.md](17-invariants.md) (the standing rules) ·
[18-what-rift-does-not-do.md](18-what-rift-does-not-do.md) (the permanent limits).
