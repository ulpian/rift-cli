# 16 — Developing Rift

This chapter is for the person changing the code: how to build it, what the
checks are, what each is proving, and which will bite you first if you skip the
local run. Every command here is one you can run. Two sections stand alone:
[§16.1](#161-the-five-minute-version) gets you building, and
[§16.13](#1613-machine-hazards-macos-icloud-and-shell) explains the failures that
are not your code's fault.

Related chapters: [01-quickstart.md](01-quickstart.md) for using Rift,
[17-invariants.md](17-invariants.md) for the rules the checks defend,
[18-known-gaps.md](18-known-gaps.md) for what is honestly unfinished.

---

## 16.1 The five-minute version

```bash
git clone <the repository>
cd rift-cli
./skills.sh build          # first build compiles bundled SQLite; give it a few minutes
./skills.sh check          # format + lint + the whole test suite
```

Two prerequisites, both non-negotiable:

- **The pinned Rust toolchain.** `rust-toolchain.toml` pins channel **1.96.0**
  with the `rustfmt`, `clippy` and `rust-src` components; `rustup` materialises
  it the first time you run `cargo` here. The declared minimum supported version
  is **1.94** (`rust-version` in `Cargo.toml`, `msrv` in `clippy.toml`) — a
  promise to downstream builders, checked by its own build job, not the version
  you develop on.
- **A C compiler.** `rusqlite` is pinned with the `bundled` feature, which
  compiles the SQLite amalgamation from source. No compiler, no build. On macOS
  that means the Command Line Tools; on Debian-family Linux, `build-essential`.

Why a source-built SQLite: the projection's append-only guarantee is enforced
partly by SQL triggers, so a store whose dialect depends on whatever the host
packaged has host-dependent guarantees. Bundling makes the dialect a property of
the repository. See [13-storage-and-schema.md](13-storage-and-schema.md).

> Tip: `./skills.sh check` runs the full suite and is not fast. While iterating
> run the one crate you are touching (`cargo test -p rift-store`), and save the
> full check for just before you push.

---

## 16.2 The task runner

`./skills.sh` is the single documented entry point for common workflows: a
dependency-free bash script (no `jq`, no `make`) that `cd`s to its own
directory first, so it behaves the same anywhere, and runs under
`set -euo pipefail`.

```bash
./skills.sh list          # or `help`, or no arguments at all
```

| Task | What it does | Underlying command |
|---|---|---|
| `fmt` | Format every crate | `cargo fmt --all` |
| `fmt-check` | Verify formatting, write nothing | `cargo fmt --all --check` |
| `lint` | Clippy over the workspace, warnings denied | `cargo clippy --workspace --all-targets --all-features -- -D warnings` |
| `test` | The full workspace suite | `cargo test --workspace --all-features` |
| `build` | Debug build | `cargo build --workspace` |
| `release` | Optimised build (what CI builds) | `cargo build --workspace --release` |
| `check` | The pre-push gate: `fmt-check` + `lint` + `test` | — |
| `run` | Run the `rift` binary, forwarding arguments | `cargo run -p rift -- …` |
| `tui` | Launch the dashboard (bare `rift`) | `cargo run -p rift` |
| `memd` | Run the projection binary, forwarding arguments | `cargo run -p rift-memd -- …` |
| `smoke` | Both binaries report status | — |
| `coverage` | Instrumented run plus the coverage gate — **needs nightly** | [§16.6](#166-coverage-as-a-gate-not-a-dashboard) |
| `docs` | Rustdoc at a zero-warning baseline | `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features` |

Three files describe the same list and must agree:
[`SKILLS.md`](../../SKILLS.md) for humans, `skills.json` for machines,
`skills.sh` for the shell. Adding a task means editing all three; the redundancy
is the point, because a machine-readable manifest that drifts from the runner is
worse than no manifest. Raw `cargo` works too.

> Tip: `./skills.sh run -- status` and `./skills.sh run -- doctor` are worth
> muscle memory. `--fix` gates `doctor`'s repairs, but even a plain run opens —
> and can create and migrate — the projection it resolves, so point a check that
> drives it at a sandbox cache first (§16.12).

**One stale example.** All three files illustrate the `memd` task as
`memd -- serve`; there is no `serve` subcommand, the projection binary taking
`metrics`, `rebuild`, `status`, `doctor` and `eval`
(`bins/rift-memd/src/main.rs`). The illustration predates the retirement of a
long-running server surface — recall verbs are served through hook injection and
the recall skill CLI. Use `./skills.sh memd -- status`. See
[07-serving-and-injection.md](07-serving-and-injection.md).

---

## 16.3 What CI runs

`.github/workflows/ci.yml` triggers on pushes to the default branch and every
pull request, grants `contents: read` and nothing else, and cancels superseded
runs for the same ref. It declares **fifteen jobs**; fourteen are named in a
required-job list inside `xtask` (`REQUIRED_JOBS` in `xtask/src/ci_meta.rs`)
checked by the `ci-meta` job itself, so a job cannot be quietly deleted while CI
stays green. The fifteenth, `release-build`, is not required.

| Job | Runs | What it proves |
|---|---|---|
| `fmt` | `cargo fmt --all --check` | Formatting is mechanical; review never spends time on it |
| `clippy` | `-D warnings` over the workspace, then a default-features pass over the two crates carrying an opt-in paid feature, then the `rift` binary with **no** default features | Lint drift behind an opt-in stays visible, unmasked by `--all-features`; and the build with every optional feature off is a build somebody ships |
| `test` | `cargo test --workspace --all-features` on Linux, macOS and Windows | Platform breaks found on the platform; `fail-fast` is **off**, so a Windows-only break survives a Linux failure |
| `msrv` | `cargo build --workspace --all-features` on 1.94 | The declared minimum version compiles |
| `coverage` | Instrumented run on nightly plus the coverage gate | [§16.6](#166-coverage-as-a-gate-not-a-dashboard) |
| `security` | `cargo deny check`, `cargo audit --deny warnings` | Licences, advisories, bans, sources |
| `ci-meta` | `xtask ci-meta`, then `actionlint` over the workflow | The guards on the guards — [§16.4](#164-the-guards-on-the-guards) |
| `gate-intercept` | `xtask gate-intercept` — seven read-only legs over the tracked tree | One place composes a hook response; the channels answered on are a reviewable list; nothing a hook decides with opens a store; one module writes the sentences a model reads; tool-call rewrites stay within three pinned forms; a cache is keyed by the repository family |
| `bench-harness` | Format, lint and test inside `benches/native-baseline` | Not a root-workspace member, so the three-OS `test` job never reaches it; this is its only run |
| `fuzz-smoke` | Sixty seconds per fuzz target, on nightly | Every untrusted-byte parser survives a smoke budget per pull request |
| `red-team` | The prompt-injection suites across validity, serve and pack | A forbidden span leaking into served output blocks the merge — never advisory |
| `docs` | `cargo doc` with `RUSTDOCFLAGS: -D warnings`, then `xtask rustdoc-baseline` | A doc comment cannot rot into an unfollowable link |
| `dialect-conformance` | The legacy-import golden suites and the substrate walker suite, by name | A suite cannot be dropped from an unnamed slice of `test` and go unnoticed |
| `miri` | Miri over the crypto, hash-chain and dashboard crates, on nightly | Advisory — `continue-on-error` is set so the failure state is *visible* rather than swallowed by `\|\| true` |
| `release-build` | `cargo build --workspace --release` | The optimised profile compiles (not required) |

**Why the feature-off leg exists.** The binary's optional features — the
terminal backend, document conversion, search — carry real code, and
`--all-features` compiles every one of them, so an item that only exists behind
a feature and is named by code outside it is invisible to a workspace lint run
and breaks for whoever builds without it. The leg is a clippy pass rather than a
`cargo check`, so it denies the same warnings its neighbours do, and it builds
the test targets too, since the first hard error the configuration ever had was
in a test. It runs locally under the same `lint` task, so a developer meets it
before a pull request does. Only the two ends of the matrix are built:
everything on and everything off. Each single feature is clean today and would
cost a second full lint of the binary crate to keep proving it — the leg to add
is one line, on the day two of these seams reach for each other.

`bench-harness` checks out with full history on purpose: its staleness arm asks
git whether a recorded measurement's subject is an ancestor of the current tip,
and a shallow clone answers that wrongly. See
[15-benchmarks.md](15-benchmarks.md). A second workflow,
`.github/workflows/fuzz-nightly.yml`, runs each fuzz target for ten minutes
daily off-peak and uploads crash artifacts when a target fails.

> Tip: `coverage` and `fuzz-smoke` need nightly and `miri` picks it for its
> component; everything else runs on the pinned channel, and
> `rustup toolchain install nightly` reproduces one locally without moving the
> pin.

---

## 16.4 The guards on the guards

`xtask` is the workspace's own tooling binary:

```
usage: xtask <check-coverage <cov.json> | ci-meta | rustdoc-baseline>
```

`cargo run -p xtask -- ci-meta` is the check that watches the other checks. It
reads the repository — source files, the git index, the resolved dependency
tree — and runs no test suite, opens no database, reaches no network. Each of
its fifteen sub-checks returns violations, and the command fails if any list
is non-empty:

| # | Check | What it refuses |
|---:|---|---|
| 1 | Required jobs present | A required-job name missing from the workflow, so deleting a job is a visible two-file diff |
| 2 | Read-only permissions | Any `write` scope creeping in; the workflow declares `contents: read` at the top |
| 3 | Lint table matches its baseline | A `[workspace.lints.*]` table differing from `ci/lint-baseline.txt`, so a deliberate change stays a reviewable two-file edit |
| 4 | No `unsafe` outside a sanctioned list | Any `unsafe`; the list is empty and the workspace `#![forbid(unsafe_code)]` end to end, so the first foreign-function seam must be added by name |
| 5 | No blanket allow at a crate root | `#![allow(clippy::…)]` of a workspace-managed lint; narrow allows belong at the site, test-only `#![cfg_attr(test, allow(…))]` is fine |
| 6 | No schema fast path in the binaries | Production code in `bins/` creating the schema directly instead of opening through the checksum-locked migration engine |
| 7 | Every fuzz target runs in CI | A file under `fuzz/fuzz_targets/` missing from the fuzz manifest or either fuzz workflow |
| 8 | Property generators can reach a newline | A regex string generator that cannot produce `\n`, unless listed in `ci/proptest-newline-baseline.txt` (a list that only shrinks) |
| 9 | No network client in the capture closure | An HTTP client declared by any crate reachable from the `rift` binary, in any dependency section and any spelling TOML allows |
| 10 | No network client in the resolved graph | The same, over both shipped binaries' default-resolved trees, catching a client behind an external intermediary. Fails closed on empty output |
| 11 | The conformance job runs every suite | A `--test` set in that job's body that is not the declared suite list |
| 12 | The fuzzy matcher reads inside its bounds | The workspace pin for the search matcher without the feature that keeps its vectorised backends inside the bytes they were handed. It is a dependency's compile-time setting, so nothing in this repository's own source would notice it going missing |
| 13 | The paid model path stays opt-in | A manifest enabling the paid evaluation feature through a feature array or a default feature |
| 14 | Deleted paths stay deleted | A deliberately removed path — a retired kill-switch file, two reversed crate directories, a retired server entry point — back on disk, in the index, or (where naming it in `.gitignore` is safe) unignored, so a bulk `git add -A` cannot resurrect one. It reports how many paths it examined |
| 15 | The retired-vocabulary screen | Retired planning shorthand in any tracked file's path or content bytes. No exclusions — lockfiles, snapshots, fixtures and this guide are in the corpus; symlinks are read as link text, never followed; a scan of nothing is itself a failure |

The newline check matters because the capture document, the pack lines, the
commit trailers and the config format are all line-oriented. The schema and
blanket-allow scanners read only the production half of a file: everything before
its trailing test module.

The vocabulary screen you will meet. The repository once carried a parallel
vocabulary — decisions by number, work by campaign label, constraints by index
— efficient for those holding the index in their heads, opaque to everyone
else. The screen stops it growing back: write *why* a rule exists, not which
numbered note ordained it. A hit names the file, the line and the token, and
asks for a plain-language rewrite.

> Tip: the screen scans this guide too. If you add a chapter and CI reds on it,
> the fix is never to add an exclusion — it is to say the thing in words.

Note the asymmetry with redaction: `docs/` is **excluded** from the redaction
false-positive census (a prose corpus full of hashes would swamp it) and
**included** in the vocabulary screen — two questions, two corpora. See
[04-redaction-and-secrets.md](04-redaction-and-secrets.md).

---

## 16.5 The lint policy

Lints are configured once in the workspace table and every member inherits them
(`lints.workspace = true`). The policy in `Cargo.toml`:

**Rust lints:** `unsafe_code = "deny"`, `missing_docs = "warn"`,
`rust_2018_idioms` (group, lower priority), `unreachable_pub = "warn"`.

**Clippy:** the `all` and `pedantic` groups at `warn` with a lower priority, so
any individual lint below wins over its group. The weight-carrying ones:

| Lint | Level | Why |
|---|---|---|
| `unwrap_used`, `expect_used`, `panic`, `todo` | warn | A panic inside a git hook must not abort a developer's git operation |
| `dbg_macro` | warn | Debug output has no business in a capture path |
| `indexing_slicing` | warn | No un-audited `a[i]` that can panic; sites indexing by construction-checked bounds carry a narrow local allow with a reason |

Purely stylistic pedantic lints are off project-wide — deliberate numeric casts
for display and estimates, items after statements, broad-but-deliberate
signatures, exact float comparison in tests — because leaving style warnings on
trains people to ignore warnings. Since CI runs clippy with `-D warnings`, every
`warn` above is an error in CI and a nudge locally.

**Tests are allowed to be blunt.** A crate root carries `#![cfg_attr(test,
allow(clippy::unwrap_used, clippy::expect_used, …))]`, and integration test
files a plain `#![allow(…)]` of the same set: a test threading `Result` through
every line to satisfy a production lint is a test nobody reads. Production code
has no such latitude.

**Changing a lint level is a two-file change:** `Cargo.toml` *and*
`ci/lint-baseline.txt` together, with the reason in the commit message, so a
downgrade cannot ride along inside an unrelated diff.

Formatting is `rustfmt.toml`: edition 2021, 100-column width, Unix newlines,
crate-granularity imports grouped std / external / crate.

> Tip: `imports_granularity` and `group_imports` are nightly-only; on the pinned
> channel rustfmt warns, skips them and `fmt --check` still passes. Expected, not
> a broken setup.

---

## 16.6 Coverage as a gate, not a dashboard

Three floors, all enforced, none decorative:

| Floor | Value | Enforced by |
|---|---|---|
| Workspace line coverage | ≥ 85% | `cargo llvm-cov report --lcov … --fail-under-lines 85` in the CI job |
| Workspace branch coverage | ≥ 75% | `xtask check-coverage` |
| Line coverage, per security-critical crate | ≥ 95% | `xtask check-coverage` |

The security-critical set is `ci/security-crates.txt`, one crate per line: the
crypto crate, the hash-chain crate, the store, the recall policy crate, the
redaction crate, the validity crate, and the two that joined when the levers
landed — the document converter, fourteen parsers reading files anybody could
have written, and file search, which decodes an index out of a directory the
agent being served can write to and applies the sensitive-path union on the way
in. Adding one is a one-line reviewable change — the point of keeping the list
in a text file rather than in code, and each line there carries the reason it is
on the list.

Two properties matter more than the numbers. **It fails closed**: a listed
crate missing from the report is a violation, and so is a report with no branch
data because the run forgot `--branch` — a gate that passes when its input is
absent is not a gate. And **it is a small Rust program, not a shell pipeline**,
shipping inside `xtask`, so the gate path carries no external tool that could
be missing elsewhere.

One file is excluded: the operating-system keychain boundary in the store
crate, pure OS I/O with nothing testable on the near side.

```bash
./skills.sh coverage
```

That cleans first (`cargo llvm-cov clean --workspace`), runs the instrumented
suite once with `--branch`, renders a JSON report from that run, and hands it to
`xtask check-coverage`.

It needs `+nightly`: `--branch` passes a nightly-only compiler flag the pinned
stable channel **refuses**, and `check-coverage` then fails closed on the missing
branch data — so on the pinned channel the gate can be neither passed nor failed.
Somebody hit that and reported the gate unrunnable; the task and its comment are
the fix.

> Tip: **always clean before re-measuring** — stale profile data produces
> numbers that look plausible and are fiction. If the instrumented run crashes
> inside the coverage tooling rather than in your code, update the nightly
> toolchain before hunting a bug; that has happened here and was wholly on the
> tool.

The local task does not apply the 85% workspace line floor; only CI does. For
the whole picture locally, add `cargo +nightly llvm-cov report --summary-only`
after the run.

---

## 16.7 Determinism in tests

A test reading the wall clock, the network, or the machine it runs on eventually
fails for a reason unrelated to the change that triggered it, so the workspace
injects those inputs.

**Identifiers.** `UlidSource` (`crates/rift-core/src/id.rs`) is a one-method
trait yielding the next strictly-increasing identifier. Production uses
`MonotonicUlidSource` — milliseconds in the high bits, a per-process counter in
the low bits, a monotonic floor so a backwards clock still yields increasing
ids. Tests use `CounterUlidSource`: a counter, no clock. Same call sites.

**Identity.** `IdentitySource` (`crates/rift-core/src/identity.rs`) supplies the
git author name, the email, a machine fingerprint and the per-install salt.
Production reads git config and the operating system; tests supply
`FixtureIdentity`. The trait has **no network method** — identity derivation is
local by construction, not by policy — and a raw machine fingerprint is read
transiently into a buffer that zeroes on drop, only its salted hash persisting.
See [04-redaction-and-secrets.md](04-redaction-and-secrets.md).

**Randomness.** Nonces are 96 bits and must be unique per key and message; reuse
is catastrophic for the cipher in use. Production draws from the operating
system's cryptographic generator; tests needing a *specific* ciphertext build the
nonce explicitly from raw bytes (the constructor documents that the caller owns
uniqueness), which is how the known-answer test compares against a published
vector.

**Time.** There is no workspace-wide clock trait: almost everywhere time enters
as an explicit `Timestamp` argument the caller supplies, and `Timestamp::now()`
appears in only a handful of production edges. The one real clock seam is the
dashboard's splash sequencer, with a real implementation, a manually-advanced one
for tests, and a deliberately slow one proving the animation degrades correctly
on a slow terminal (`crates/rift-canopy/src/splash/director.rs`). See
[12-canopy-architecture.md](12-canopy-architecture.md).

---

## 16.8 The invariant tests you must not weaken

Some tests are load-bearing: breaking them means the guarantee is gone, not that
a test is inconvenient.

**The append-only keystone** (`crates/rift-store/tests/immutability.rs`). After
a full production migration, every real table must be a registered canonical
table carrying *both* append-only guard triggers, update and delete; infra
tables are underscore-prefixed and exempt. This makes the central promise
executable — add a canonical table without its guard and CI goes red — and the
registry is the single source of truth the triggers and the assertion both
derive from.

Beside it sits a canary, `forgotten_registration_is_caught`, applying a migration
that creates a canonical-shaped table, forgetting to register it, and asserting
the keystone flags it — a guard without a calibration is a guard nobody has seen
bite. It derives its version from the shipped set, so landing a real migration
does not collide with it.

**The dependency graph.** `cargo test -p xtask` holds the workspace's shape: the
graph is acyclic (with two fixture calibrations — a back edge that must be
flagged, an acyclic graph that must pass); only the store crate may depend on a
storage backend; declared members match the directories on disk both ways; every
member opts into workspace inheritance; the toolchain channel is pinned; the
minimum version and edition are what they claim.

Its readable output is a committed snapshot,
`xtask/src/snapshots/xtask__tests__crate_graph_adjacency.snap`: one line per
crate, its in-workspace dependencies sorted, so a new edge is a visible line in a
diff. Regenerate it the usual snapshot way and *read the diff*.

**The shrink-only baselines.** `ci/rustdoc-baseline.txt` and
`ci/proptest-newline-baseline.txt` hold known-imperfect sites by name, with **two
arms** each: a site not listed is a violation ("fix it"), and a listed site that
no longer warns is *also* one ("delete the line — this list only shrinks"), which
stops a baseline becoming a parking lot. Both are empty today and exist anyway,
so a future unfixable warning has one honest home instead of a blanket allow. The
proptest baseline's header warns that adding the "dot matches newline" flag is
only the *first half* of the fix: if the property then fails, that is a finding to
report, never a reason to narrow the generator back until it passes green.

---

## 16.9 Fuzzing

`fuzz/` is a **separate workspace** with its own `[workspace]` table, so
`cargo build --workspace` on stable never tries to build it — it needs nightly
and a fuzzing runtime. Thirteen targets ship today:

`config_parse`, `run_hook`, `redact`, `redact_jsonl`, `fuzz_pack_load`, `diff`,
`glob_match`, `checkpoint_artifact`, `mcp_request`, `palette_parse`,
`doc_convert`, `shell_read`, `search_query`.

The selection rule: **an untrusted-byte parser gets a target when it lands.**
Config text, hook payloads, redaction input, pack files, diffs, glob patterns,
checkpoint artifacts, serve requests, palette definitions, document containers,
shell command lines and search queries all arrive from outside and must be
total — a value or a typed error for any input, never a panic.

The three newest are the ones a decision now runs on. `doc_convert` is by far
the largest parser surface in the tree: fourteen container formats reading files
an agent found in a repository, which is to say files anybody could have
written. `shell_read` fuzzes the strict recogniser that decides whether a
command line is a bounded read or a listing — it runs before anything else on
that path, so a panic there is a hook that dies on a command line instead of
leaving it alone. `search_query` covers both untrusted inputs of search: the
query a model composed, and the index file itself, which lives in a work tree
the agent being served can write to.

```bash
cd fuzz
cargo +nightly fuzz run config_parse -- -max_total_time=60 -timeout=25
```

`mcp_request` is a name with history, and its own file explains it: it fuzzes the
serve request decoder, whose one caller used to be a long-running server
transport since deleted. That decoder is the transport-independent seam the
decision kept, and today it decodes the recall skill CLI's arguments-turned-JSON.
The name stays because renaming discards the accumulated corpus and is wired into
both workflows.

---

## 16.10 Supply chain

Two tools, one job, configured in `deny.toml` and `.cargo/audit.toml` — kept in
lockstep, because an ignore living in one and not the other stops applying when
the other tool runs.

`cargo deny check` covers four areas:

- **Advisories.** Yanked crates are denied, and each ignore is justified in a
  comment naming the advisory, why it does not reach us, and what would make it
  relevant again. Exactly one today: an unmaintained-but-not-vulnerable decoder
  behind a syntax-highlighting dependency's required feature, which never sees
  untrusted bytes.
- **Bans.** Wildcard version requirements are denied in our own manifests.
  Multiple versions of the same crate warn. The system TLS bindings are denied
  outright — the project prefers the pure-Rust TLS stack.
- **Licences.** A short permissive allow list, with copyleft handled by
  *annotated per-crate exceptions* rather than by widening it. Three exceptions
  today, each scoped to one crate with its reason, so a future dependency under
  the same licence is a conscious addition. One is *more* permissive than the
  allow list, in an exception only because the list is never widened.
- **Sources.** Unknown registries and git sources are denied; the crates
  registry is the only allowed source.

`cargo audit --deny warnings` runs the advisory check independently, honouring
the mirrored ignore list.

> Tip: run `cargo deny check` before pushing a new dependency: seconds now
> against a CI round trip for a licence rejection.

---

## 16.11 The git hooks in your checkout

If Rift is capturing a repository you work in, **seven** git hooks are installed
in the checkout. In the order the installer declares them
(`crates/rift-shadow-hooks/src/install.rs`):

`prepare-commit-msg`, `commit-msg`, `post-commit`, `post-rewrite`, `pre-push`,
`post-merge`, `post-checkout`.

On Unix each is a direct link to the `rift` binary, routed by the name it was
invoked under; on Windows a small shim per name does the same. Beside each link
sits a sidecar with a `.rift-managed-link` suffix recording the exact target —
how Rift later tells "my link, pointing where I left it" from "somebody else's
hook with the same name". A hook that already existed is chained aside, never
overwritten.

Two of the seven are installed and routed but have no handler body yet; they
hold their slot in the dispatch table. See [03-capture.md](03-capture.md) for
what each hook with a body records and for the latency budgets — capture is on
the critical path of your `git commit`, and the design bends around not being
felt there.

**Turning wiring off and on.** `rift disable --agent <name>` and
`rift enable --agent <name>` operate on **an agent client's** wiring — the
per-client configuration files routing an assistant's events into Rift — and not
on the git hooks. There is no "disable the git hooks" verb: the hooks arrive with
repository initialisation, and the installer's health probe reports a family
missing a hook as unhealthy so the next install repairs it.

**Holding the hooks aside.** Sometimes you want them out of the way — a long
interactive rebase that would otherwise fire the rewrite hook on every step.
This is not a product feature: rename the hook files in `.git/hooks` yourself,
do your work, rename them back.

```bash
cd "$(git rev-parse --git-dir)/hooks"
for h in commit-msg post-checkout post-commit post-merge post-rewrite pre-push prepare-commit-msg; do
  mv "$h" "$h.held"     # reverse the two names to restore afterwards
done
```

Leave the `.rift-managed-link` sidecars alone — they tell the installer these
are still Rift's hooks when you put them back. Nothing in Rift knows the
`.held` suffix; it is a convention, and while the hooks are held aside nothing
is captured. `rift doctor` reports them missing, which is correct and exactly
the signal you want if you forget to restore them.

> Tip: **never** run the hook installer while the hooks are held aside. Its
> entry point is a hidden command handing its trailing arguments straight to the
> router, so it does not honour `--help`: asking for help performs a real
> install and re-links all seven. To know what it does, read `install.rs`.

---

## 16.12 Adding things: three short checklists

**Adding a canonical table.** Write the migration (additive; never edit a shipped
one — the engine is checksum-locked and a changed migration is a poisoned
projection), register the table in the store's canonical registry so it gets both
append-only triggers, and run `cargo test -p rift-store`; if you skipped the
registration, the keystone names it. Details in
[13-storage-and-schema.md](13-storage-and-schema.md).

**Adding a crate.** Create it under `crates/`, add it to `[workspace].members`,
set `lints.workspace = true`, inherit the workspace package fields, and run
`cargo test -p xtask`: the members-match-directories and inheritance checks name
what you forgot, and the adjacency snapshot changes — review that diff as
carefully as the code. Keep the direction acyclic; route heavy or pre-1.0
externals through a seam. And **measure the shipped binary in the commit that
adds the dependency**, not at the end of the change: the stripped release `rift`
is held under **30,000,000 bytes** (decimal, and it is a decimal cap so that the
figure means the same thing to everyone reading it, and it is the Linux figure
CI builds that the cap is read against). The cap was 24,000,000 while document
conversion lived in a sibling helper binary; the commit that linked the
fourteen document parsers into `rift` measured the stripped release binary on
the machine that made the change, Windows 11, at 26,319,360 bytes before and
32,177,664 bytes after — 5,858,304 bytes for the engine, beside the 5.14 MB the
original Linux measurement recorded — and raised the cap by that delta, with
rounding, rather than by a guess. Two things follow from where the engine is
now. It is **in** the binary for size accounting and **never runs in the
process a hook is**: conversion happens in `rift` re-spawned on its hidden
`hooks doc` route, so the deadline is still a kill and a hostile document still
dies in another address space, and the `gate-intercept` job holds the parsers
unreachable from any decision. And search stayed in-process, because an answer
that has to start a second process cannot be given inside a hook's budget. A
per-crate size measurement in the same commit is what makes either decision
reviewable rather than retrospective.

**Adding a check.** A check that can be opted out of per item is not a check:
deleting a test, ignoring it and hiding it behind a feature flag are all per-item
opt-outs a workspace run reports green straight through. So a durable check
either drives the shipped binary or reads a durable artifact; where it delegates
to a test it asserts that test exists **and** is not ignored; and it lands with a
calibration that restores the defect and watches the check bite. Give it a corpus
count and refuse a scan that examined nothing.

If your check runs Rift's own diagnostics, point its cache directory at a sandbox
first: `verify` and `doctor` write a verification record into whatever projection
they resolve, so a check certifying a clean-run streak can inflate it.

---

## 16.13 Machine hazards: macOS, iCloud, shell — and Windows

Not Rift bugs — environment properties that have each cost somebody real time.
The Windows ones are at the end of the section.
**A repository under an iCloud-synced directory** (`~/Documents`, or anywhere
else iCloud syncs) has four behaviours:

- `git add .` **hangs** on files iCloud has evicted ("dataless"); stage explicit
  paths instead, which is also why the deleted-paths check has the form it
  does.
- **Deleted tracked files come back**, byte-identical and untracked, when the
  sync service replays its view of the directory — that is why a file you deleted
  reappears in `git status`.
- **Directories fault in on enumeration**, so the first command to walk a large
  tree pays for downloading it. Before a bundle, an archive or a garbage
  collection, warm the tree: `brctl download .`, then a full read pass such as
  `find . -type f -not -path './target/*' -exec cat {} + > /dev/null`.
- Tests that shell out to `git` **flake** under iCloud contention. Re-run the
  single failing test alone before declaring a red; per-crate tests are the
  reliable inner loop.

> Tip: if you can, keep the checkout outside the synced tree — everything above
> stops applying. Keep `target/` off the synced volume too; a symlink to a
> non-synced location is the usual fix.

**The system `grep` is BSD `grep` and has no `-P`**, so Perl-compatible patterns
silently are not available: use `/usr/local/bin/ggrep -nP …` or `perl -ne`.

**`git grep -E` with `\b` matches nothing** on this platform's build — no error,
no hits, just a confident empty result. Use `git grep -P` for word boundaries,
or you will "prove" an absence that is not there: a sweep empty because the
pattern never compiled looks exactly like a clean tree.

**Shell portability.** `zsh` does not word-split an unquoted `$var` in a `for`
loop the way `bash` does — a variable holding a space-separated list iterates
once, as one item. The system `bash` on macOS is 3.2 and has no `mapfile`, and
there is no `timeout(1)`; the substitute is
`perl -e 'alarm N; exec @ARGV' -- your command`.

**Never wrap a measured command in a pipe.** `$?` becomes the pipe tail's
status, so `some-gate | tee log` reports `tee`'s success — and a script ending
in an `echo` exits with `echo`'s status, so a wrapper can report success for a
killed step. Read per-command exit codes, and record them.

**Establish ownership before killing a process.** Two abandoned test runs
contending for one build lock look, from the parent chain, exactly like a
colleague's active run; `lsof -a -d cwd -p <pid>` names the working directory.

**A stale binary can corrupt a projection.** If you edit a migration and a git
hook fires while `target/debug/rift` still holds the *old* build, that binary
applies the old SQL under the new checksum and poisons the projection silently.
Rebuild before any work touching the schema or the database.

**Windows.** Five properties, each met while the dashboard was brought up on one, then ten more from the stream that made every gate green there:

- **Profile junctions deny listing.** `C:\Users\<you>` holds reparse-point
  directories — `My Documents`, `Application Data`, `Local Settings`,
  `Cookies` — that resolve to a real directory yet refuse `read_dir` with
  `Access is denied. (os error 5)`; `C:\Documents and Settings` and `System
  Volume Information` are the same shape. The directory browser tags them and
  keeps the unlistable ones inert.
- **`canonicalize` answers with the `\\?\` prefix.** The registry stores that
  spelling verbatim; compare canonical paths, never strings.
- **Hard links need NTFS.** exFAT, FAT32 and network shares refuse
  `hard_link`, which is what mints a family identity; `RIFT_FAMILY_DIR`
  relocates the manifest root.
- **The console decides the render tier**, not the environment: no Windows
  host sets `COLORTERM`. The classic "Windows PowerShell" console window
  delivered no mouse records to the dashboard on the machine this was brought
  up on; Windows Terminal (1.9 and later) forwards the mouse through ConPTY.
- **Symlink creation needs a privilege**, so a test that makes one is
  `#[cfg(unix)]`; the Windows leg of the test matrix compiles without it.

Ten more, from the stream that made every gate green on a Windows box (the
whole workspace, `--all-features`, three consecutive runs, nothing skipped
beyond the symlink cases above). Each names the helper that carries the fix,
so the next Windows-shaped defect reaches for it instead of a fourth copy.

- **Git for Windows refuses a `\\?\` path in `GIT_DIR`, `--git-dir` and
  `GIT_INDEX_FILE`** (`fatal: not a git repository`; `Unable to create
  '….lock': Invalid argument`), while it accepts one in `GIT_WORK_TREE`, `-C`
  and the working directory. The family plane stores canonical paths, so
  every git call from a member silently failed: the doctor sweep deferred
  every condense and `resume` showed no branch. `rift_git::child_process_path`
  is the spelling a child gets; every git spawn site in `rift-git` goes
  through it, and nothing minted, compared or stored does. gix accepts the
  verbatim form and needs nothing.
- **Repository-relative paths are spelled with `/` on every platform**, in the
  ledger, the sidecar links and the served frames. Windows `strip_prefix`
  output carries `\`, and a read recorded that way never joined its own
  lookup. `rift_recall_substrate::worktree::repo_relative_slash` is the one
  chokepoint; a shell read's cwd is joined textually.
- **…and the rule is a property of the string, not of the host.** Fixing
  capture was half of it: the shards already frozen keep their `\`, history is
  append-only, and a rebuild reads those bytes back — so "the projection is
  rebuildable" was not the remedy it looked like. `slash_spelled` in the same
  module is the one normaliser, applied on **both** sides. Ingest: every path
  the recall projection keys a row by arrives on a `tool_calls.jsonl` record,
  so `parse_tool_calls` re-spells there, once, and past that point no `\`
  exists to reach a row (`no_backslash_reaches_a_projection_row`). Lookup:
  `decode_request` re-spells the focus set and the `target` of the four verbs
  whose target is a path, so a caller who types either spelling is answered
  about the file. It is deliberately not `#[cfg(windows)]` — a shard bundled
  from a Windows machine and rebuilt on Linux carries `\` as an ordinary byte,
  and a projection that disagreed with the one it was copied from would be
  worse than the defect. The two targets that are not paths — a handle, a
  search query — are left exactly as typed.
- **NTFS directory mtimes read from the parent listing lag.** `walkdir` on
  Windows hands back the metadata captured while listing the parent, which
  NTFS refreshes lazily, so a freshly built search index was judged stale by
  the next probe. `dir_mtime_ms` in `rift-recall-search` stats the directory
  itself at both sites.
- **The shell-read grammar refuses `\` and `?` by design**, so a fixture that
  spells a `cat`/`sed`/`grep` line from `Path::display` is never a read on
  Windows. Fixtures use `shell_spelling` (the `tests/common` module and the
  read-gate unit tests): verbatim prefix dropped, `/` throughout. The grammar
  is not widened; the fuzz target and the pinned client lists would fight it.
- **JSON and gitconfig fixtures need escaping.** A Windows path spliced into a
  raw JSON string is an invalid escape (the cache reader calls it corrupt, a
  hook defers on it); build documents with `serde_json::json!`. Git's config
  parser reads `\U` as an escape too: a path-valued key is quoted and
  `/`-spelled (`config_path` in the attribution fixture).
- **`PATH` is joined with `;`**, so a `:`-joined fixture value is never
  searched; use `std::env::join_paths` over `split_paths` of the inherited
  value.
- **Git runs hook shims through its bundled `sh`**, and a `#!/bin/sh` file is
  error 193 to `CreateProcess`. A test that runs an installed shim resolves
  `sh.exe` from `git --exec-path` (three levels up, then `usr/bin` or `bin`)
  before scrubbing the child's `PATH`; a script stand-in for a helper binary
  is a `.cmd` wrapper around the same POSIX body through that shell.
- **Claude Code's project directory folds `\` and `:` as well as `/` and
  `.`**: a working tree `c:\Users\…\x` keeps its transcripts under
  `~/.claude/projects/c--Users-…-x`, and on Windows `~` is `%USERPROFILE%`.
  The default transcript path folds the plain spelling, never the verbatim
  one, or the name is absolute and `join` discards the projects directory.
- **`-D warnings` makes a Unix-only item an error on Windows.** A helper,
  constant or binding whose only users are `#[cfg(unix)]` is dead there; gate
  it with its users, or give the Windows arm a real shape (the unreadable
  seventeenth file is a handle held with no share mode). Never
  `allow(dead_code)`. Both `clippy -D warnings` legs and the rustdoc gate pass
  on Windows now, so `./skills.sh check` is the pre-push gate there too.

Two habits from driving this through Claude Code on Windows: the Bash tool
halves backslashes in command text, so a Windows path literal goes into a
file with the editor, never through `sed`; and a running Canopy holds
`target\debug\rift.exe`, so the binary's tests build into a second target
directory (`--target-dir target-tests`).

---

## 16.14 Lessons that shaped these rules

Every rule above was cheap to write, expensive to learn:

- **Measure before you choose a remedy.** A check once reddened on a security
  crate far above its false-positive ceiling; two remedies were proposed at once,
  and only counting the hits showed both wrong — every hit was a throwaway sandbox
  directory name from the run itself.
- **Anything you add to the tracked tree is inside the checks that scan it**,
documentation and fixtures and lockfiles included. Ask both directions: whether
a check reaches your crate, and whether your crate's output reaches the check.
- **A check that has never bitten may be incapable of biting.** One swept a
  diagnostic's output inside a sandbox that never created the thing it looked
  for, so it read an empty string and passed permanently.
- **An absolute number is not a ratchet**, and **benchmarks belong in tracked,
buildable code**: timings move by roughly a factor of two with load, so ratchet
against a control measured in the same run, and a script nobody committed makes
a printed reproducibility claim false the day it is written. State a number's
limits beside it. See [15-benchmarks.md](15-benchmarks.md).
- **Prefer the smaller move.** One proposal to fix a layering problem with a
new crate would have pulled two heavy crates into the dashboard's data plane —
a *larger* edge than the one it avoided. Check the edges a fix creates.

---

## 16.15 A savings reading through the real clients

Everything above measures Rift against fixtures. One command measures it
against the clients themselves:

```bash
cargo run -p xtask -- dogfood --client all --sessions 10 --keep
```

It plays ten short, fixed sessions through each of Claude Code, Codex and
Antigravity and prints one table: per client and overall, the sessions with
reads, tokens read, tokens avoided, tokens returned, the honesty figure, the
share of [§8.7](08-cost-and-savings.md#87-the-honesty-ladder), the decisions by
kind, and — where a client's headless output exposes them — how often the model
asked again after a decline. The same facts are written as `dogfood.json` beside
the logs.

**It spends real API quota**, once per session per client, on whatever plan the
developer is signed in to. So it refuses to run under `CI`, it prints what it is
about to spend before it spends it, `cargo test` can never reach it, and nothing
in the workflow file calls it. The three parsers, the session loader, the table
and the JSON are unit-tested; the run itself is a command a person types.

**What it does to the machine, and what it does not.** Each client gets its own
clone of your checkout under `~/rift-dogfood/<utc-stamp>/`, and `rift init`
mints that clone a family of its own ([Chapter 9](09-worktrees-and-families.md)).
The harness reads `rift savings --json` in the clone and *refuses to continue*
if either projection path is the one your real repository uses — a reading that
wrote into your own evaluation window would be worth nothing. It repoints the
clone's hooks at your `target/release/rift` (building it first if it is not
there), copies two documents out of the converter's corpus into the clone so the
document lever has something to act on, and primes them by hand — ask, drain the
conversion journal, ask again — because the first read of an unconverted
document is past the gate's deadline and a client that met one would measure a
race rather than a lever. The reading each session is measured against is taken
*after* that priming — the refusal above still reads the one taken right after
`init` — so the first session's figures are the first session's own. Without
`--keep` the clone is deleted afterwards and
`rift projects prune` sweeps the registry row it created; with `--keep` both
stay, and the command says where.

**The flags, and why those.** Claude Code runs with
`--setting-sources project,local` so only the clone's own wiring is loaded, and
with `Read Edit Glob Grep` allowed rather than permissions skipped. Codex runs
with `--dangerously-bypass-hook-trust`, which lets the clone's hook file run for
one invocation and persists nothing; it is deliberately **not** given
`--dangerously-bypass-approvals-and-sandbox`, which writes a trust row into
`~/.codex/config.toml`. That client writes a trust row for a directory it has
run in anyway, and it is not this command's file to change, so the file is
copied aside with its timestamp before any client runs and put back byte for
byte at the end — the command says so when it had to. Antigravity runs with
`--new-project`, without which it continues the
previous conversation and the sessions stop being independent. Each session is
time-boxed at ten minutes by a `perl` alarm, because macOS ships no `timeout`.

**What the figure is.** It is a reading of what the levers were worth on a fixed
scripted sample, through real clients, on the day it ran. It is **not**
deterministic: the models choose their own tool calls, so two runs of the same
ten sessions read different files and produce different shares. It is **not**
the class-sized fixture that CI holds to a threshold, and it is **not** the
ordinary-session evaluation the savings window measures on a repository somebody
actually works in — that one is the number to quote. This one answers a narrower
question: given ten sessions of the same shape, do the levers fire through the
real clients at all, and does the honesty figure stay positive. Read it with its
`n` attached, and never re-denominate it.

A session that did not finish is named in the table with the reason: a client
that is missing, refuses, or has run out of quota is recorded as **UNAVAILABLE**
and the run carries on to the next one. The command's exit code says whether the
harness worked, never whether the share was good.

**`--compare`: the same sessions with the levers off.** The share above is
booked from what Rift decided; it does not know what would have been read
without it. Adding `--compare` answers that a second way, at twice the spend: every
client plays the scripted sessions twice, in the same order and under the same
time box, in two clones with two families of their own — one with `mode = "off"`
written into `[interception]`, `[search]` and `[doc]` of its serving overlay,
one exactly as the levers ship. The calibration table puts them side by side: read
tokens with the levers off, read tokens with them on, the **measured** removal
between the two, what the levers **booked** as avoided over the same sessions —
with the prevented part shown beside it where the reporting surface carries one
— and `booked ÷ measured` as a percentage, per session, per client and over the
run. Where a client's own event stream states what a session cost it, that
figure is carried in two more columns; a client whose stream states none prints
`-`, as does any other figure the harness does not have.

Read the calibration as a sanity check on the booked figure's *size*. Near 100 %
says the two agree; far above says Rift booked more than the arms removed; far
below says it booked less. None of those is a verdict, because **the two arms
are not two runs of one experiment**: the models choose their own tool calls, so
a difference carries the levers and the day's choices together. A **negative**
measured removal — the arm with the levers on read more — is an ordinary
outcome of that, and is printed as the negative it is with no ratio beside it,
never clamped to zero. The same facts are written as `compare.json` beside
`dogfood.json`, caveat included. `--keep` keeps both clones; without it both are
swept, because an arm left behind is a family and a registry row nobody meant to
keep. Without the flag the command does what it always did, byte for byte.

---

## 16.16 Where to look next

[../../CLAUDE.md](../../CLAUDE.md) (the working brief: conventions, crate map,
constraints) · [../../AGENTS.md](../../AGENTS.md) (the shorter version, for
assistants) · [../../SECURITY.md](../../SECURITY.md) (the threat model) ·
[../../SKILLS.md](../../SKILLS.md) (the task list) ·
[17-invariants.md](17-invariants.md) and [18-known-gaps.md](18-known-gaps.md)
(the standing rules, and what is open).
