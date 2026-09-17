# `benches/native-baseline` — the benchmark harness

Every "better in Rust" claim this project makes has to become **a number a
harness produced and anybody can re-run**. This package is that harness.

It is a black-box driver over *binaries*: it links no rift crate, so what it
measures is what a developer would actually invoke. It is its own cargo workspace
(the repo-root `Cargo.toml` member list is a maintainer-only surface), with its
own committed lockfile pinned to the versions the main workspace already uses.

```bash
cd benches/native-baseline
cargo test                      # harness unit + e2e tests
cargo run -- scenarios          # the catalogue: wired vs planned
cargo run -- fixtures build small
cargo run -- doctor             # which legs resolve here, and why not
cargo run --release -- hook-latency          # the git-latency sweep, as tracked Rust
cargo run -- ratchet --baseline a.json --candidate b.json   # ratios, not absolutes
cargo run -- run --scenario cold-status   # writes into the tracked benchmarks/native-baseline/
```

## The two legs

| Leg | Binary | Resolution |
|---|---|---|
| `rift` | `rift` | `$RIFT_BENCH_RIFT_BIN`, else a release build in the repo `target/`, else `rift` on `PATH` |
| `reference` | the pinned capture backend | `$RIFT_BENCH_REFERENCE_BIN`, else the provisioned pin path, else **absent** |

**A resolved rift binary older than its own sources is refused, not measured.**
`vs-reference doctor` prints the newest source modification time in the checkout
and the file that carried it; a binary predating it is reported `ABSENT` with
that filename, because the timing would be perfectly real and about code the
binary does not contain. This project has the defect on the record: a suite stayed
green with a planted defect because it drove a `target/debug/rift` it never
relinked, and when the guard was written the release build in the side checkout
was **13 days and 168 `.rs` files** behind the tree it would have been published
against. `$RIFT_BENCH_ALLOW_STALE=1` overrides it and stamps `STALE` into the
leg's `source` field, so the report carries the admission. The pinned backend is
exempt by statement rather than by omission — its sources are not in this
checkout, so its rows read `freshness unknown`.

**The harness never downloads or provisions anything.** Provisioning the pinned
backend is an owner step (the pin machinery was deleted when capture became
native end to end); the backend leg now resolves to
*absent* and every comparison it would have produced is recorded as `skipped`
with the reason. A missing leg is never silently treated as a loss or a win, and
rift-only numbers are still collected — that is the mode this harness runs in
today. After retirement the pinned binary stays a **benchmark fixture only**,
never on a user path.

## Fixtures (generated, never cloned)

Three specs:

| Spec | Worktree | History | Scripted turns | What it stresses |
|---|---|---|---|---|
| `small` | 12 files | 8 commits | 20 | hot-path latency: hook round-trip, cold `status` |
| `medium` | 500 files | 200 commits | 10 000 | checkpoint storage growth (the "10k-checkpoint synthetic") |
| `large` | 10 000 files | 40 commits | 200 | worktree hashing, tree surgery, `status` at scale |

Checkpoints are deliberately **not** pre-generated: each leg writes its own, in
its own format, and that is part of what is being measured.

A fixture is a pure function of its spec — same seed, same file bytes, same
commit dates, therefore the same commit oids on any machine (`two_independent_
builds_agree_on_head_oid` is the guard). Fixtures are cached under `.fixtures/`
(git-ignored) keyed by a digest of the spec; changing any dimension yields a new
directory rather than a mutated one.

Generation cost, measured on the development machine (`vs-reference fixtures build`):

| Spec | Build | Reuse | On disk | `HEAD` |
|---|---|---|---|---|
| `small` | 1.8 s | 0.01 s | 35 KB | `437881472e7d…` |
| `medium` | 64 s | 0.01 s | 4.5 MB | `0c7d9d7a2383…` |
| `large` | 153 s | 0.01 s | 90 MB | `d4f1a46e3e11…` |

Those oids are the cross-machine check: a different value for the same spec
means the environment leaked into the fixture. A run only builds the fixtures it
can actually measure — with no leg available, nothing is generated at all.

Set `$RIFT_BENCH_FIXTURE_DIR` to move the cache; point it outside any synced
folder (iCloud/Dropbox eviction turns a fixture into stalled I/O mid-measure).

## The hook rows

Nine rows — one per Claude Code hook verb, plus the replay of one of them and
the child-process count of another — because their costs are not alike: a
session's *first* hook pays identity binding, a turn boundary pays a worktree
snapshot, a read-shaped tool call pays worktree hashing plus tokenization, and
`Stop` closes a turn and **leaves the transcript for the next hook** — a
turn's spend is settled off the Stop, from a recorded transcript offset, by
the next hook for the session (the deferred-settle contract; the
`hook-prompt-submit` row is where that settle is paid). One blended "hook"
figure would hide all four. Every hook payload names the transcript, because
every hook may be the one that settles the turn before it.

| row | timed verb | session | untimed prelude |
|---|---|---|---|
| `hook-round-trip` | `PreToolUse` (Bash) | established | — |
| `hook-session-start` | `SessionStart` | new each iteration | — |
| `hook-prompt-submit` | `UserPromptSubmit` | established | — |
| `hook-read-tool` | `PreToolUse` (Read): the worktree hash; the BPE count is the detached worker's | established | — |
| `hook-tool-result` | `PostToolUse` | established | its `PreToolUse` |
| `hook-stop` | `Stop` | established | prompt → call → result |
| `hook-session-end` | `SessionEnd`: journals the condense; the archive is the detached worker's | new each iteration | a whole session |
| `hook-replay` | `PreToolUse` (Bash), *unchanged* | established | — |
| `identity-attribution` | `SessionStart` | new each iteration | — |

Every row but the last delivers a **new** event each iteration, in the field a
live session varies (the prompt text, the command, the file being read). That is
not cosmetic: a canonical event's identity is
`sha256(agent ‖ session ‖ verb ‖ turn ‖ ordinal ‖ payload)`, deliberately
clock-free, so re-sending byte-identical bytes in the same turn is a *replay* and
ingest is a designed no-op. `hook-replay` sends exactly that, on purpose, and
measures the dedup short-circuit as its own number — the difference between the
two rows is what ingest actually costs.

A verb costs what it costs *in the session state it really meets*: a `Stop` at a
session that never opened a turn closes nothing and snapshots no boundary, so
timing it would produce a flattering number for work that never happened. The
prelude puts the session where the timed verb finds it in life, outside the
timed region. Files it needs (a transcript) are written **after** the prelude
runs, so they are as fresh at the timed verb as a live agent's would be — the
bytes a settling hook reads were written moments ago in life, and a Stop must
not be measured against a transcript that has been quiet for seconds.

Every hook row is judged against the agent-hook budget — p50 < 100 ms, p95 < 500 ms,
*including* ingest — and the report prints the bar beside the number. With one
exception: `hook-replay` is *held to* the budget (a regression in the dedup path
is worth seeing) but its verdict is **n/a — no-op path**, because passing a
budget by not doing the work is not passing it.

### Detached work

Two hook verbs hand work to a detached one-shot worker rather than doing it
inline (an owner decision taken 2026-08-30): `SessionEnd` journals its condense, and a
read-shaped call journals its BPE token count — the tokenizer's initialisation
alone (234 ms, release) is over the budget, so no inline cap could keep it. The
timed region ends when the hook process exits, exactly as in life. The harness
then **waits for the journal to drain, untimed** — before the next iteration's
prelude, and once more after the window before the probes — so a worker still
condensing the previous session is never measured as contention, and a journal
that will not drain within 30 s fails the row instead of publishing the latency
of a hook whose work never happened.

### Proof of ingest

"Including ingest" is only a claim unless the run can show it. Before the first
measured iteration and after the last, the harness reads the leg's own durable
counter (`rift status` → `captured events`) outside the timed region, and
requires every event the iteration owes: **one for the timed verb, plus one for
each invocation in its untimed prelude** (so `hook-tool-result` owes 2 per
iteration, `hook-stop` 4, `hook-session-end` 5). The prelude's share is not
optional slack — a floor of "at least one" is cleared by a row whose prelude
ingests while the timed verb writes nothing, which is the failure this check
exists to catch. A hook that ingests nothing exits *faster*, so without this a
broken capture path would publish as a record-breaking result; instead the
measure is reported **failed**, with its tempting timings inside the failure. The
backend leg publishes no equivalent counter, so its hook rows carry no proof and
the report says so rather than assuming one.

### Child processes

`identity-attribution` times the same verb as `hook-session-start` and publishes
something else: **how many child processes that hook launches**. A wall time says
how long the first hook of a session took; it does not say that almost all of it
was process creation, which is what the 488.54 ms baseline turned out to be —
eight `git config`/`ioreg` spawns for three facts.

The count is taken the way a black-box harness has to take it: a directory of
counting shims goes first on the child's `PATH`, one script per observed program
(`git`, `ioreg`), each recording its own name and then `exec`ing the real binary
by absolute path. Three rules keep it honest:

* it runs in its **own untimed pass** — a shim costs an extra `exec` per spawn,
  so inside a timed region it would measure the instrument;
* the counted invocation is **one iteration past the measured window**, with its
  own prelude, because re-running the last measured hook would re-deliver an
  event the session has already seen — and a session whose identity is already
  bound spawns nothing, which would publish a beautiful, meaningless zero;
* a host that cannot host the shims reports the count as *unavailable*, never as
  zero. Zero spawns is a real and excellent result, so it must not also be what a
  failed measurement looks like.

## Measurement protocol

1. **Hermetic environment.** Every child process is launched through `Hermetic`:
   git config comes from stub files the harness writes (never `~/.gitconfig` or
   `/etc/gitconfig`), the `GIT_*` variables that re-target a repository are
   scrubbed, `protocol.allow = never` forbids any transport, `TZ=UTC`/`LC_ALL=C`
   pin formatting, and identity/commit dates are fixed. Git hooks are disabled
   while generating fixtures and enabled while measuring (condensation-at-commit
   rides the repository's own hooks).
2. **Isolation.** A scenario declares whether it runs against a shared fixture or
   a fresh per-iteration copy. Copy cost is never inside the timed region.
3. **Warmup then measure.** Warmup iterations are executed and discarded, then
   `n` measured iterations are timed end to end (process spawn included — spawn
   cost *is* the hook comparison).
4. **Statistics.** Reported as min / p50 / p95 / max, nearest-rank on the sorted
   sample vector — no interpolation, no averaging away a tail. Sample counts are
   published with every number. Below 20 iterations the nearest-rank p95 *is*
   the maximum; the report says so for every row it applies to, and no threshold
   may be set on such a p95.
5. **Non-time measures.** Peak RSS (from the platform `time` utility when it is
   available, otherwise recorded as unavailable — never estimated), binary size,
   and files written per checkpoint.
6. **Honesty rules.** A leg that is absent, a verb a leg does not have, and a
   measure that is not wired yet are three *distinct*, explicitly recorded
   outcomes. The report never fabricates a comparison, and a rift-only measure
   (e.g. `rift verify` throughput — Reference has no integrity check at all) is
   labelled as such rather than counted as a win.

## The other rows

Everything the measurement plan asks for that is not a hook verb. All wired (the `Planned`
state is representable and tested, and empty since 2026-08-29); the two
resolver rows carry an **answer probe** — one untimed re-run after the
measured window, over the same repository state — because a `session resume`
that restored nothing and a `why` that resolved no checkpoint both exit 0
quickly, and timing a refusal would publish it as a latency.

| row | timed verb | fixture | untimed prelude | answered when |
|---|---|---|---|---|
| `cold-status` | `status` | `small`, fresh copy per iteration | — | — |
| `checkpoint-list` | `checkpoint list` | `small` | — | — |
| `condense` | `vogon drive … --commit` (3 turns, 25 hooks, then a commit) | `medium`, fresh copy | — | — |
| `resume-restore` | `--json session resume main --force` | `small` | a session on its own id (transcript, start, prompt, tool call, Stop), a file, a commit through the installed hooks with the archive inline | `sessions[0].restored == "restored"` |
| `import-throughput` | `import --from-legacy` | `small`, fresh copy | 64 reconstructed shards planted by `fast-import` | — |
| `blame-why` | `--json why bench/why_<i>.txt:1` | `small` | the same session, whose turn **writes** the file before its Stop, then the commit | `selection.checkpoint_id` is a string |
| `verify-throughput` | `verify` | `medium` | — | — |

The resume and why rows are rift-only: the retired reference implementation had
no verb that restored a transcript from a checkpoint into place, and no
line → checkpoint → prompt resolver. Neither row has a ratified budget; they
publish p50/p95 and the report says "not measured" beside the bar until an owner
sets one.

## Reports

A run writes a versioned JSON report (`schema_version`) plus a Markdown rendering
into the tracked `benchmarks/native-baseline/` by default (`--out` overrides;
`docs/` is git-ignored in this repository, so a report written there would be
invisible to anyone who did not run it). The report records both legs' resolved
binary paths and version strings, the fixture manifests, the host description,
and every sample count, so a number can always be traced back to the exact
repository and binary that produced it. `benchmarks/README.md` aggregates every
family's artifacts into one generated ```` ```bench-report ```` fence
(`vs-reference facts --kind report`), which the artifact-consistency guard
recomputes.

## Where the priced savings figures live (and why not here)

This harness links no rift crate, on purpose — it drives *binaries*. That also
means it cannot tokenize, so it cannot price what a call would have cost. Its
`recall-intercept-savings` row says so in its own caveat: it proves the
mechanism ran and the report answered, and it can never be quoted as a saving.

The priced figures are the product's own fixtures, which run inside the
workspace with the real tokenizer and publish pool / received / removed:

| class | fixture | where |
|---|---|---|
| repeat reads | `the_intercept_fixture_removes_at_least_ninety_percent_of_its_pool` | `bins/rift/tests/read_gate_e2e.rs` |
| documents | `the_doc_fixture_removes_at_least_ninety_percent_of_its_pool` | `bins/rift/tests/document_savings.rs` |
| searches | `the_search_fixture_removes_at_least_ninety_percent_of_its_pool` | `bins/rift/tests/search_savings.rs` |

The document fixture generates its own corpus in Rust, drives the shipped
binary over it on both channels — the substituted read and the verb — and
cross-checks every price against the figure the product booked for the same
document. Quote those numbers, never a row from this harness.

Its bar is **≥ 90 % on the priced formats**. A PDF is not presented to a model
as text at all — its honest cost is pages times a vision-token rate, and no
artifact in this tree pins one — so the fixture declares nothing for the three
PDFs and they enter neither the numerator nor the denominator; what they cost
is published beside the share rather than divided into it. Quote the bar with
that scope attached, and the wider figure over every document read beside it.

The search fixture works the same way and lives in the same place, for the
same reason: it generates a five-thousand-path repository in Rust, commits it,
and plays one scripted session over it twice — once with the levers off, so
the tool runs and the model receives the listing, and once with them on, so it
receives a frame. Its pool is the listing the model would have received, cut
at the ceiling the client the call was made on documents: a hundred files for
a `Glob`, thirty thousand characters for a shell result, and the smaller of
the two wherever no artifact in this tree states one. Its scope is **name
searches only**. A content grep has no declared native cost anywhere here and
is never substituted by a name search, so it is in no fraction at all and
publishes its own honesty figure instead — how often a bounded search was
asked again in the turn it was bounded in. Recursive directory listings are
outside it from the other side: Rift declines to answer them, and nothing here
prices one.
