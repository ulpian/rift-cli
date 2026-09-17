# rift vs the pinned capture backend

- harness `0.0.1`, report schema 9
- host: macos / x86_64 (8 logical CPUs)
- peak RSS via /usr/bin/time -l (bytes)

- machine: load1 4.78

## Legs

| leg | state | binary | version | size | built | commit |
 |---|---|---|---|---|---|---|
| `rift` | present (repo target/release) | `target/release/rift` | rift 0.0.1 | 16742 KiB | 1788195998 | — |
| `reference` | **absent** | — | — | — | — | — |
| | no pin manifest and no $RIFT_BENCH_REFERENCE_BIN: the backend leg is inactive (the harness never provisions it) | | | | | |
| `vogon` (driver) | present (repo target/release) | `target/release/vogon` | {"name":"vogon","schema":1,"script_sha256":"5770c4767dfc61db0c9066380a8a8a2ca07cc5f1a5ff91afbd2fb2840ab1dfe0","source_fnv1a":"2b531933c75718ff","version":"0.0.1"} | — | 1788195821 | — |

**Record.** No commit is recorded for this run. It is kept as a *record* — a real measurement, quotable as history, barred from setting a bar — because the history that held its subject was collapsed to a single fresh root on 2026-09-02.

Newest source in the checkout: `bins/rift/src/workflows.rs` at **1788195342** (330 files scanned). A rift                  binary older than that is refused rather than measured — a timing taken from                  a binary that predates its own sources is real and is about the wrong code.

## Fixtures

| fixture | files | bytes | commits | turns | head |
|---|---|---|---|---|---|
| `small` | 12 | 35161 | 8 | 20 | `437881472e7d` |
| `medium` | 500 | 4457612 | 200 | 10000 | `0c7d9d7a2383` |

## Headline measures

 | measure | rift p50 | rift p95 | rate | work/it | backend p50 | backend p95 |                  speedup | n |
|---|---|---|---|---|---|---|---|---|
| `hook-round-trip` | 18.40 ms | 22.48 ms | — | — | — | — | — | 30 |
| `cold-status` | 12.24 ms | 13.28 ms | — | — | — | — | — | 30 |
| `condense` | 1123.66 ms | 1176.29 ms | 24.92 events/s | 28 events | — | — | — | 3 |
| `import-throughput` | 404.85 ms | 405.21 ms | 158 shards/s | 64 shards | — | — | — | 3 |

## Other measures

 | measure | rift p50 | rift p95 | rate | work/it | backend p50 | backend p95 |                  speedup | n |
|---|---|---|---|---|---|---|---|---|
| `hook-session-start` | 54.24 ms | 57.61 ms | — | — | — | — | — | 30 |
| `hook-prompt-submit` | 42.47 ms | 44.75 ms | — | — | — | — | — | 30 |
| `hook-read-tool` | 18.86 ms | 22.15 ms | — | — | — | — | — | 30 |
| `hook-tool-result` | 16.28 ms | 18.35 ms | — | — | — | — | — | 30 |
| `hook-stop` | 49.78 ms | 55.69 ms | — | — | — | — | — | 30 |
| `hook-session-end` | 20.94 ms | 23.38 ms | — | — | — | — | — | 30 |
| `hook-replay` | 15.17 ms | 16.48 ms | — | — | — | — | — | 30 |
| `identity-attribution` | 51.52 ms | 53.76 ms | — | — | — | — | — | 30 |
| `checkpoint-list` | 11.47 ms | 12.54 ms | — | — | — | — | — | 30 |
| `resume-restore` | 124.38 ms | 155.01 ms | — | — | — | — | — | 30 |
| `blame-why` | 59.96 ms | 66.92 ms | — | — | — | — | — | 30 |
| `verify-throughput` | 18.18 ms | 20.74 ms | 54.99 checkpoints/s | 1 checkpoints | — | — | — | 3 |
| `worktree-add-adopt` | 69.26 ms | 73.59 ms | — | — | — | — | — | 30 |
| `member-switch` | 35.56 ms | 42.74 ms | — | — | — | — | — | 30 |
| `recall-known-state` | 132.04 ms | 177.74 ms | — | — | — | — | — | 30 |
| `recall-compounding` | 118.51 ms | 120.01 ms | 11670 tokens-avoided/s | 1383 tokens-avoided | — | — | — | 3 |

## Read these with the numbers

- **`import-throughput`** — This rate is true about our own reconstruction of the pre-Rift on-disk format and silent about the format itself. No legacy artifact was ever fetched — the README beside the golden shards under `legacy-fixtures/` says so in its own words, "Nothing here was fetched" — and every spelling in these shards is cited there to a declaration that already lived in this repository, and the corpus is that frozen shard replicated under distinct ids. It measures the reader — the walk, the parse, the root re-derivation. It is never a format-compatibility claim.
- **`recall-known-state`** — This is the WHOLE serve, not the walk. The containment design budgets the ancestry walk at <= 20 ms p50 ADDED to this verb, and an added cost is a difference between two measurements: this row is one of them, and the other is the same verb on a build without the branch axis. Quote the difference a paired run reports, never this number, as the cost of containment.
- **`recall-compounding`** — The work unit is the tokens ONE serve in a linked member avoided out of the family's shared store — the member captured none of those reads itself. The published compounding ratio is that figure over the same serve's figure under the per-path baseline (one store per checkout, which is what a member had before one identity per repository family keyed the store), and a paired run prints both numerators beside it. A ratio without its two numerators is not a measurement, and this row's number is one of them, not the ratio.

## Budgets

 | measure | rift p50 | rift p95 | rate | work/it | budget | verdict | ingest proof |
|---|---|---|---|---|---|---|---|
| `hook-round-trip` | 18.40 ms | 22.48 ms | — | — | p50 ≤ 100 ms, p95 ≤ 500 ms | p50 **met** · p95 **met** | +30 events (≥ 30 required) |
| `hook-session-start` | 54.24 ms | 57.61 ms | — | — | p50 ≤ 100 ms, p95 ≤ 500 ms | p50 **met** · p95 **met** | +30 events (≥ 30 required) |
| `hook-prompt-submit` | 42.47 ms | 44.75 ms | — | — | p50 ≤ 100 ms, p95 ≤ 500 ms | p50 **met** · p95 **met** | +30 events (≥ 30 required) |
| `hook-read-tool` | 18.86 ms | 22.15 ms | — | — | p50 ≤ 100 ms, p95 ≤ 500 ms | p50 **met** · p95 **met** | +30 events (≥ 30 required) |
| `hook-tool-result` | 16.28 ms | 18.35 ms | — | — | p50 ≤ 100 ms, p95 ≤ 500 ms | p50 **met** · p95 **met** | +60 events (≥ 60 required) |
| `hook-stop` | 49.78 ms | 55.69 ms | — | — | p50 ≤ 100 ms, p95 ≤ 500 ms | p50 **met** · p95 **met** | +120 events (≥ 120 required) |
| `hook-session-end` | 20.94 ms | 23.38 ms | — | — | p50 ≤ 100 ms, p95 ≤ 500 ms | p50 **met** · p95 **met** | +150 events (≥ 150 required) |
| `hook-replay` | 15.17 ms | 16.48 ms | — | — | p50 ≤ 100 ms, p95 ≤ 500 ms | n/a — no-op path | none available |
| `identity-attribution` | 51.52 ms | 53.76 ms | — | — | p50 ≤ 100 ms, p95 ≤ 500 ms | p50 **met** · p95 **met** | +30 events (≥ 30 required) |
| `condense` | 1123.66 ms | 1176.29 ms | 24.92 events/s | 28 events | events/s — no floor ratified | OPEN — awaiting owner ratification | none available |
| `import-throughput` | 404.85 ms | 405.21 ms | 158 shards/s | 64 shards | shards/s — no floor ratified | OPEN — awaiting owner ratification | none available |
| `verify-throughput` | 18.18 ms | 20.74 ms | 54.99 checkpoints/s | 1 checkpoints | checkpoints/s — no floor ratified | OPEN — awaiting owner ratification | none available |
| `worktree-add-adopt` | 69.26 ms | 73.59 ms | — | — | p50 ≤ 100 ms, p95 ≤ 100 ms | p50 **met** · p95 **met** | none available |
| `member-switch` | 35.56 ms | 42.74 ms | — | — | p50 ≤ 100 ms, p95 ≤ 100 ms | p50 **met** · p95 **met** | none available |
| `recall-compounding` | 118.51 ms | 120.01 ms | 11670 tokens-avoided/s | 1383 tokens-avoided | tokens-avoided/s — no floor ratified | OPEN — awaiting owner ratification | none available |

The `n/a` rows (`hook-replay`) re-deliver an event whose ingest is a designed no-op. Such a row is kept because the dedup short-circuit is a real hook path worth a number, and it is held to the same ceiling — but it is **not judged** against it, because passing a budget by not doing the work is not passing it.

## Child processes

One invocation of the measured verb, counted in a separate untimed pass (the counting shims cost an exec each, so they must never sit inside a published timing).

| measure | leg | child processes |
|---|---|---|
| `identity-attribution` | `rift` | 0 (git 0, ioreg 0) |

Percentiles are nearest-rank over the measured samples: every figure is an observed run, never an interpolation.
With fewer than 20 iterations the nearest-rank p95 **is** the maximum, so it carries no tail information and no threshold may be set on it: `condense` (n=3), `import-throughput` (n=3), `verify-throughput` (n=3), `recall-compounding` (n=3).

## Not measured

- `reference`: not requested by this run (`--legs`) — 20 of 20 scenarios
