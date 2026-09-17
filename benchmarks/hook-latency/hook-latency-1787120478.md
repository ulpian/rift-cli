# Hook latency — git-latency sweep

- harness `0.0.1`, artifact schema 1, generated 1787120478
- host: macos / x86_64 (8 logical CPUs)
- control git: git version 2.50.1 (Apple Git-155)
- protocol: 3 warmup + 11 measured commits per arm
- budget: **100 ms** of git latency (a hard budget, set by the owner)

## Subject

`/Users/ulpian/Documents/codebase/rift/rift-lane-substrate/target/release/rift` — rift 0.0.1, resolved via repo target/release, built 1787120056

Newest source in the checkout: `crates/rift-shadow/src/lib.rs` at 1787119840 (302 files).

## Arms

`hook latency` is this arm's p50 minus the control's, in the same run; `× bare` is the ratio, and the ratio is the portable number.

| arm | p50 | min | hook latency | × bare | verdict |
|---|---|---|---|---|---|
| `A_bare` | 61.63 ms | 49.08 ms | — | 1.00× | control |
| `B0_idle` | 114.18 ms | 108.95 ms | 52.55 ms | 1.85× | meets |
| `B1_live` | 177.13 ms | 160.27 ms | 115.50 ms | 2.87× | **BREACH** |
| `C1_include_50k` | 170.44 ms | 137.21 ms | 108.81 ms | 2.77× | **BREACH** |
| `C2_include_250k` | 170.65 ms | 151.22 ms | 109.02 ms | 2.77× | **BREACH** |
| `C3_include_1mib` | 237.62 ms | 201.28 ms | 175.99 ms | 3.86× | **BREACH** |

- `A_bare` — bare commit, no hooks installed (control)

- `B0_idle` — hooks installed, nothing opted in, no live session

- `B1_live` — hooks installed, a session live at commit time

- `C1_include_50k` — live session + `.worktreeinclude` over 8 files (51200 B read, 52696 B handed on, 1.03x)

- `C2_include_250k` — live session + `.worktreeinclude` over 40 files (256000 B read, 263480 B handed on, 1.03x)

- `C3_include_1mib` — live session + `.worktreeinclude` over 16 files (1048576 B read, 1066352 B handed on, 1.02x)

## Opted-in corpus

`MAX_TOTAL_BYTES` is 1 MiB and is charged on bytes **read**; `Walk::file` prefixes every content line with one space, so N bytes contribute up to **2N + 2** to the document the redactor is handed. Both numbers are published, because an arm described as "1 MiB" without saying which 1 MiB misstates the ceiling in one direction or the other.

| arm | files | bytes read | budget | document | factor | files read (explain) |
|---|---|---|---|---|---|---|
| `C1_include_50k` | 8 | 51200 | under | 52696 | 1.03× | — |
| `C2_include_250k` | 40 | 256000 | under | 263480 | 1.03× | — |
| `C3_include_1mib` | 16 | 1048576 | **at/over the 1 MiB read ceiling** | 1066352 | 1.02× | — |

The factor here is near 1.0 because these lines are long. The 2× is the pathological end — a file that is all newlines, where the one-space prefix is the whole line — so a full budget can hand on a document just over 2 MiB. Neither end is the other, and this table says which one each arm is.

`files read (explain)` is `rift checkpoint explain`'s **own** label, not a span count: the inclusion walk's record count is on no CLI surface today, so the harness publishes the figure the product publishes rather than relabelling it as something it has not established.

## Anatomy — the curve, the units, and the phase that owns it

**BREACH: `B1_live`, `C1_include_50k`, `C2_include_250k`, `C3_include_1mib`.** The budget is 100 ms of git latency and is HARD (set by the owner). The corpus was not adjusted in response — the arms are the sizes they were declared at.

**Magnitude and attribution come from different commits.** `git latency` is the median of the timed commits minus the control's; every phase column comes from the arm's one **extra untimed** commit with the timer on. Read the residue as an anatomy of a representative commit beside the magnitude of the median, never as an exact decomposition of one commit.

`predict_id` here is **exclusive** — `redact` subtracted — so the columns are disjoint and may be added.

| arm | bytes read | handed on | ×read | git latency | hook bodies | outside bodies | `redact` | `predict_id` less `redact` | verdict |
|---|---|---|---|---|---|---|---|---|---|
| `B0_idle` | — | — | — | 52.55 ms | 3.81 ms | 48.75 ms | 0.00 ms | 0.00 ms | meets |
| `B1_live` | — | — | — | 115.50 ms | 33.33 ms | 82.17 ms | 20.69 ms | 0.08 ms | **BREACH** |
| `C1_include_50k` | 51200 | 52696 | 1.03× | 108.81 ms | 36.94 ms | 71.87 ms | 20.70 ms | 0.44 ms | **BREACH** |
| `C2_include_250k` | 256000 | 263480 | 1.03× | 109.02 ms | 49.06 ms | 59.96 ms | 33.20 ms | 2.31 ms | **BREACH** |
| `C3_include_1mib` | 1048576 | 1066352 | 1.02× | 175.99 ms | 114.20 ms | 61.79 ms | 83.50 ms | 9.75 ms | **BREACH** |

`outside bodies` is the floor a Rust change cannot move: three `/bin/sh` invocations and three execs that git requires, plus git's own cost of running them. If the budget ever has to halve, that column is what has to be attacked, and it is not in Rust code.

## Oracles — what each arm exercised

Reported for every arm including the control, because a check that only ever confirms is not a check: the idle arm's **absent** trailer is what makes the live arm's present one mean something.

| arm | projections | trailer | verify | events |
|---|---|---|---|---|
| `A_bare` | none | absent | — | — |
| `B0_idle` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 430080 B | absent | 1 checkpoint(s), 0 shard(s) | 0 |
| `B1_live` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 565248 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `C1_include_50k` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 565248 B | present | — | 60 |
| `C2_include_250k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `C3_include_1mib` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 565248 B | present | 12 checkpoint(s), 13 shard(s) | 60 |

**More than one projection under a single arm's cache.** That is the shape of the defect this table exists for: the number may be about a store rift never opened.

## Phases

From one extra **untimed** commit per arm with `RIFT_HOOK_TIMING=1`, straight out of the shipped binary. Untimed because the timer takes clock readings the shipped default never takes.

**Read the `exclusive` column, and never sum `recorded`.** The timer records on completion, so a nested phase prints before its parent and the parent's figure already contains it: `predict_id` INCLUDES `redact`. `recorded` is what the binary printed; `exclusive` has every declared child subtracted, so those values are disjoint and may be added. Summing `recorded` would double-count the redaction — the one quantity the opted-in arms exist to separate.

| arm | hook | phase | recorded µs | exclusive µs |
|---|---|---|---|---|
| `B0_idle` | post-commit | discover | 34 | 34 |
| `B0_idle` | post-commit | enqueue | 476 | 476 |
| `B0_idle` | post-commit | gix_open | 838 | 838 |
| `B0_idle` | post-commit | head_hex | 429 | 429 |
| `B0_idle` | post-commit | live_sessions | 24 | 24 |
| `B0_idle` | post-commit | spawn_worker | 1598 | 1598 |
| `B0_idle` | post-commit | total | 3650 | 3650 |
| `B0_idle` | prepare-commit-msg | discover | 35 | 35 |
| `B0_idle` | prepare-commit-msg | live_sessions | 23 | 23 |
| `B0_idle` | prepare-commit-msg | total | 158 | 158 |
| `B1_live` | post-commit | discover | 33 | 33 |
| `B1_live` | post-commit | enqueue | 475 | 475 |
| `B1_live` | post-commit | gix_open | 787 | 787 |
| `B1_live` | post-commit | head_hex | 449 | 449 |
| `B1_live` | post-commit | live_sessions | 612 | 612 |
| `B1_live` | post-commit | spawn_worker | 1643 | 1643 |
| `B1_live` | post-commit | total | 4280 | 4280 |
| `B1_live` | prepare-commit-msg | attribution_preview | 479 | 479 |
| `B1_live` | prepare-commit-msg | discover | 43 | 43 |
| `B1_live` | prepare-commit-msg | live_sessions | 1014 | 1014 |
| `B1_live` | prepare-commit-msg | open_projection | 5357 | 5357 |
| `B1_live` | prepare-commit-msg | predict_id | 20777 | 84 (parent, `redact` subtracted) |
| `B1_live` | prepare-commit-msg | raw_capture | 25 | 25 |
| `B1_live` | prepare-commit-msg | read_facts | 164 | 164 |
| `B1_live` | prepare-commit-msg | redact | 20693 | 20693 |
| `B1_live` | prepare-commit-msg | rule_packs | 17 | 17 |
| `B1_live` | prepare-commit-msg | tokio_build | 30 | 30 |
| `B1_live` | prepare-commit-msg | total | 29052 | 29052 |
| `C1_include_50k` | post-commit | discover | 53 | 53 |
| `C1_include_50k` | post-commit | enqueue | 552 | 552 |
| `C1_include_50k` | post-commit | gix_open | 1431 | 1431 |
| `C1_include_50k` | post-commit | head_hex | 614 | 614 |
| `C1_include_50k` | post-commit | live_sessions | 1161 | 1161 |
| `C1_include_50k` | post-commit | spawn_worker | 1874 | 1874 |
| `C1_include_50k` | post-commit | total | 5981 | 5981 |
| `C1_include_50k` | prepare-commit-msg | attribution_preview | 488 | 488 |
| `C1_include_50k` | prepare-commit-msg | discover | 66 | 66 |
| `C1_include_50k` | prepare-commit-msg | live_sessions | 1239 | 1239 |
| `C1_include_50k` | prepare-commit-msg | open_projection | 5396 | 5396 |
| `C1_include_50k` | prepare-commit-msg | predict_id | 21143 | 440 (parent, `redact` subtracted) |
| `C1_include_50k` | prepare-commit-msg | raw_capture | 1206 | 1206 |
| `C1_include_50k` | prepare-commit-msg | read_facts | 237 | 237 |
| `C1_include_50k` | prepare-commit-msg | redact | 20703 | 20703 |
| `C1_include_50k` | prepare-commit-msg | rule_packs | 40 | 40 |
| `C1_include_50k` | prepare-commit-msg | tokio_build | 45 | 45 |
| `C1_include_50k` | prepare-commit-msg | total | 30959 | 30959 |
| `C2_include_250k` | post-commit | discover | 73 | 73 |
| `C2_include_250k` | post-commit | enqueue | 442 | 442 |
| `C2_include_250k` | post-commit | gix_open | 904 | 904 |
| `C2_include_250k` | post-commit | head_hex | 440 | 440 |
| `C2_include_250k` | post-commit | live_sessions | 734 | 734 |
| `C2_include_250k` | post-commit | spawn_worker | 1452 | 1452 |
| `C2_include_250k` | post-commit | total | 4298 | 4298 |
| `C2_include_250k` | prepare-commit-msg | attribution_preview | 576 | 576 |
| `C2_include_250k` | prepare-commit-msg | discover | 32 | 32 |
| `C2_include_250k` | prepare-commit-msg | live_sessions | 706 | 706 |
| `C2_include_250k` | prepare-commit-msg | open_projection | 2995 | 2995 |
| `C2_include_250k` | prepare-commit-msg | predict_id | 35510 | 2314 (parent, `redact` subtracted) |
| `C2_include_250k` | prepare-commit-msg | raw_capture | 3447 | 3447 |
| `C2_include_250k` | prepare-commit-msg | read_facts | 127 | 127 |
| `C2_include_250k` | prepare-commit-msg | redact | 33196 | 33196 |
| `C2_include_250k` | prepare-commit-msg | rule_packs | 29 | 29 |
| `C2_include_250k` | prepare-commit-msg | tokio_build | 22 | 22 |
| `C2_include_250k` | prepare-commit-msg | total | 44765 | 44765 |
| `C3_include_1mib` | post-commit | discover | 37 | 37 |
| `C3_include_1mib` | post-commit | enqueue | 657 | 657 |
| `C3_include_1mib` | post-commit | gix_open | 839 | 839 |
| `C3_include_1mib` | post-commit | head_hex | 433 | 433 |
| `C3_include_1mib` | post-commit | live_sessions | 644 | 644 |
| `C3_include_1mib` | post-commit | spawn_worker | 2378 | 2378 |
| `C3_include_1mib` | post-commit | total | 5341 | 5341 |
| `C3_include_1mib` | prepare-commit-msg | attribution_preview | 665 | 665 |
| `C3_include_1mib` | prepare-commit-msg | discover | 53 | 53 |
| `C3_include_1mib` | prepare-commit-msg | live_sessions | 1281 | 1281 |
| `C3_include_1mib` | prepare-commit-msg | open_projection | 4974 | 4974 |
| `C3_include_1mib` | prepare-commit-msg | predict_id | 93250 | 9751 (parent, `redact` subtracted) |
| `C3_include_1mib` | prepare-commit-msg | raw_capture | 5679 | 5679 |
| `C3_include_1mib` | prepare-commit-msg | read_facts | 210 | 210 |
| `C3_include_1mib` | prepare-commit-msg | redact | 83499 | 83499 |
| `C3_include_1mib` | prepare-commit-msg | rule_packs | 43 | 43 |
| `C3_include_1mib` | prepare-commit-msg | tokio_build | 35 | 35 |
| `C3_include_1mib` | prepare-commit-msg | total | 108863 | 108863 |

### Phase audit

`total` is measured independently of the phases, so disjoint phases can never exceed it. `over-attributed` is therefore a proof that something is nested which the harness does not declare — not a rounding remark.

| arm | hook | total µs | attributed µs | unattributed µs | verdict |
|---|---|---|---|---|---|
| `B0_idle` | post-commit | 3650 | 3399 | 251 | adds up |
| `B0_idle` | prepare-commit-msg | 158 | 58 | 100 | adds up |
| `B1_live` | post-commit | 4280 | 3999 | 281 | adds up |
| `B1_live` | prepare-commit-msg | 29052 | 27906 | 1146 | adds up |
| `C1_include_50k` | post-commit | 5981 | 5685 | 296 | adds up |
| `C1_include_50k` | prepare-commit-msg | 30959 | 29860 | 1099 | adds up |
| `C2_include_250k` | post-commit | 4298 | 4045 | 253 | adds up |
| `C2_include_250k` | prepare-commit-msg | 44765 | 43444 | 1321 | adds up |
| `C3_include_1mib` | post-commit | 5341 | 4988 | 353 | adds up |
| `C3_include_1mib` | prepare-commit-msg | 108863 | 106190 | 2673 | adds up |
