# Hook latency — git-latency sweep

- harness `0.0.1`, artifact schema 1, generated 1787127649
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
| `A_bare` | 42.53 ms | 37.23 ms | — | 1.00× | control |
| `B0_idle` | 100.28 ms | 93.69 ms | 57.75 ms | 2.36× | meets |
| `B1_live` | 125.66 ms | 116.57 ms | 83.13 ms | 2.95× | meets |
| `C1_include_50k` | 132.19 ms | 114.56 ms | 89.66 ms | 3.11× | meets |
| `C2_include_250k` | 138.44 ms | 125.18 ms | 95.91 ms | 3.26× | meets |
| `C3_include_1mib` | 214.68 ms | 172.84 ms | 172.15 ms | 5.05× | **BREACH** |
| `D1_newline_50k` | 149.49 ms | 121.57 ms | 106.96 ms | 3.51× | **BREACH** |
| `D2_newline_250k` | 187.06 ms | 177.53 ms | 144.53 ms | 4.40× | **BREACH** |
| `D3_newline_1mib` | 284.53 ms | 258.26 ms | 242.00 ms | 6.69× | **BREACH** |

- `A_bare` — bare commit, no hooks installed (control)

- `B0_idle` — hooks installed, nothing opted in, no live session

- `B1_live` — hooks installed, a session live at commit time

- `C1_include_50k` — live session + `.worktreeinclude` over 8 files (51200 B read, 52696 B handed on, 1.03x)

- `C2_include_250k` — live session + `.worktreeinclude` over 40 files (256000 B read, 263480 B handed on, 1.03x)

- `C3_include_1mib` — live session + `.worktreeinclude` over 16 files (1048576 B read, 1066352 B handed on, 1.02x)

- `D1_newline_50k` — live session + `.worktreeinclude` over 8 files (51200 B read, 102592 B handed on, 2.00x)

- `D2_newline_250k` — live session + `.worktreeinclude` over 40 files (256000 B read, 512960 B handed on, 2.00x)

- `D3_newline_1mib` — live session + `.worktreeinclude` over 16 files (1048576 B read, 2097536 B handed on, 2.00x)

## Opted-in corpus

`MAX_TOTAL_BYTES` is 1 MiB and is charged on bytes **read**; `Walk::file` prefixes every content line with one space, so N bytes contribute up to **2N + 2** to the document the redactor is handed. Both numbers are published, because an arm described as "1 MiB" without saying which 1 MiB misstates the ceiling in one direction or the other.

| arm | files | bytes read | budget | document | factor | files read (explain) |
|---|---|---|---|---|---|---|
| `C1_include_50k` | 8 | 51200 | under | 52696 | 1.03× | — |
| `C2_include_250k` | 40 | 256000 | under | 263480 | 1.03× | — |
| `C3_include_1mib` | 16 | 1048576 | **at/over the 1 MiB read ceiling** | 1066352 | 1.02× | — |
| `D1_newline_50k` | 8 | 51200 | under | 102592 | 2.00× | — |
| `D2_newline_250k` | 40 | 256000 | under | 512960 | 2.00× | — |
| `D3_newline_1mib` | 16 | 1048576 | **at/over the 1 MiB read ceiling** | 2097536 | 2.00× | — |

The factor here is near 1.0 because these lines are long. The 2× is the pathological end — a file that is all newlines, where the one-space prefix is the whole line — so a full budget can hand on a document just over 2 MiB. Neither end is the other, and this table says which one each arm is.

`files read (explain)` is `rift checkpoint explain`'s **own** label, not a span count: the inclusion walk's record count is on no CLI surface today, so the harness publishes the figure the product publishes rather than relabelling it as something it has not established.

## Anatomy — the curve, the units, and the phase that owns it

**BREACH: `C3_include_1mib`, `D1_newline_50k`, `D2_newline_250k`, `D3_newline_1mib`.** The budget is 100 ms of git latency and is HARD (set by the owner). The corpus was not adjusted in response — the arms are the sizes they were declared at.

**Magnitude and attribution come from different commits.** `git latency` is the median of the timed commits minus the control's; every phase column comes from the arm's one **extra untimed** commit with the timer on. Read the residue as an anatomy of a representative commit beside the magnitude of the median, never as an exact decomposition of one commit.

`predict_id` here is **exclusive** — `redact` subtracted — so the columns are disjoint and may be added.

| arm | bytes read | handed on | ×read | git latency | hook bodies | outside bodies | `redact` | `predict_id` less `redact` | verdict |
|---|---|---|---|---|---|---|---|---|---|
| `B0_idle` | — | — | — | 57.75 ms | 2.84 ms | 54.91 ms | 0.00 ms | 0.00 ms | meets |
| `B1_live` | — | — | — | 83.12 ms | 22.00 ms | 61.13 ms | 14.13 ms | 0.05 ms | meets |
| `C1_include_50k` | 51200 | 52696 | 1.03× | 89.66 ms | 26.52 ms | 63.14 ms | 17.11 ms | 0.32 ms | meets |
| `C2_include_250k` | 256000 | 263480 | 1.03× | 95.91 ms | 38.86 ms | 57.06 ms | 25.66 ms | 1.77 ms | meets |
| `C3_include_1mib` | 1048576 | 1066352 | 1.02× | 172.15 ms | 89.92 ms | 82.22 ms | 64.50 ms | 5.75 ms | **BREACH** |
| `D1_newline_50k` | 51200 | 102592 | 2.00× | 106.96 ms | 43.30 ms | 63.65 ms | 28.48 ms | 0.82 ms | **BREACH** |
| `D2_newline_250k` | 256000 | 512960 | 2.00× | 144.53 ms | 95.22 ms | 49.32 ms | 65.99 ms | 5.30 ms | **BREACH** |
| `D3_newline_1mib` | 1048576 | 2097536 | 2.00× | 242.00 ms | 156.33 ms | 85.67 ms | 112.92 ms | 10.12 ms | **BREACH** |

`outside bodies` is the floor a Rust change cannot move: three `/bin/sh` invocations and three execs that git requires, plus git's own cost of running them. If the budget ever has to halve, that column is what has to be attacked, and it is not in Rust code.

## Oracles — what each arm exercised

Reported for every arm including the control, because a check that only ever confirms is not a check: the idle arm's **absent** trailer is what makes the live arm's present one mean something.

| arm | projections | trailer | verify | events |
|---|---|---|---|---|
| `A_bare` | none | absent | — | — |
| `B0_idle` | rift/<repo-id>/projection.db 430080 B; rift/canopy/ui-state.db 90112 B | absent | 1 checkpoint(s), 0 shard(s) | 0 |
| `B1_live` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `C1_include_50k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `C2_include_250k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `C3_include_1mib` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 12 checkpoint(s), 12 shard(s) | 60 |
| `D1_newline_50k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | — | 60 |
| `D2_newline_250k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 13 checkpoint(s), 14 shard(s) | 60 |
| `D3_newline_1mib` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 565248 B | present | 11 checkpoint(s), 11 shard(s) | 60 |

**More than one projection under a single arm's cache.** That is the shape of the defect this table exists for: the number may be about a store rift never opened.

## Phases

From one extra **untimed** commit per arm with `RIFT_HOOK_TIMING=1`, straight out of the shipped binary. Untimed because the timer takes clock readings the shipped default never takes.

**Read the `exclusive` column, and never sum `recorded`.** The timer records on completion, so a nested phase prints before its parent and the parent's figure already contains it: `predict_id` INCLUDES `redact`. `recorded` is what the binary printed; `exclusive` has every declared child subtracted, so those values are disjoint and may be added. Summing `recorded` would double-count the redaction — the one quantity the opted-in arms exist to separate.

| arm | hook | phase | recorded µs | exclusive µs |
|---|---|---|---|---|
| `B0_idle` | post-commit | discover | 27 | 27 |
| `B0_idle` | post-commit | enqueue | 385 | 385 |
| `B0_idle` | post-commit | gix_open | 597 | 597 |
| `B0_idle` | post-commit | head_hex | 300 | 300 |
| `B0_idle` | post-commit | live_sessions | 12 | 12 |
| `B0_idle` | post-commit | spawn_worker | 1228 | 1228 |
| `B0_idle` | post-commit | total | 2732 | 2732 |
| `B0_idle` | prepare-commit-msg | discover | 25 | 25 |
| `B0_idle` | prepare-commit-msg | live_sessions | 12 | 12 |
| `B0_idle` | prepare-commit-msg | total | 111 | 111 |
| `B1_live` | post-commit | discover | 29 | 29 |
| `B1_live` | post-commit | enqueue | 382 | 382 |
| `B1_live` | post-commit | gix_open | 623 | 623 |
| `B1_live` | post-commit | head_hex | 333 | 333 |
| `B1_live` | post-commit | live_sessions | 487 | 487 |
| `B1_live` | post-commit | spawn_worker | 1152 | 1152 |
| `B1_live` | post-commit | total | 3200 | 3200 |
| `B1_live` | prepare-commit-msg | attribution_preview | 342 | 342 |
| `B1_live` | prepare-commit-msg | discover | 30 | 30 |
| `B1_live` | prepare-commit-msg | live_sessions | 690 | 690 |
| `B1_live` | prepare-commit-msg | open_projection | 2702 | 2702 |
| `B1_live` | prepare-commit-msg | predict_id | 14180 | 52 (parent, `redact` subtracted) |
| `B1_live` | prepare-commit-msg | raw_capture | 35 | 35 |
| `B1_live` | prepare-commit-msg | read_facts | 152 | 152 |
| `B1_live` | prepare-commit-msg | redact | 14128 | 14128 |
| `B1_live` | prepare-commit-msg | rule_packs | 20 | 20 |
| `B1_live` | prepare-commit-msg | tokio_build | 24 | 24 |
| `B1_live` | prepare-commit-msg | total | 18799 | 18799 |
| `C1_include_50k` | post-commit | discover | 34 | 34 |
| `C1_include_50k` | post-commit | enqueue | 411 | 411 |
| `C1_include_50k` | post-commit | gix_open | 681 | 681 |
| `C1_include_50k` | post-commit | head_hex | 400 | 400 |
| `C1_include_50k` | post-commit | live_sessions | 505 | 505 |
| `C1_include_50k` | post-commit | spawn_worker | 1162 | 1162 |
| `C1_include_50k` | post-commit | total | 3409 | 3409 |
| `C1_include_50k` | prepare-commit-msg | attribution_preview | 342 | 342 |
| `C1_include_50k` | prepare-commit-msg | discover | 28 | 28 |
| `C1_include_50k` | prepare-commit-msg | live_sessions | 815 | 815 |
| `C1_include_50k` | prepare-commit-msg | open_projection | 2680 | 2680 |
| `C1_include_50k` | prepare-commit-msg | predict_id | 17429 | 319 (parent, `redact` subtracted) |
| `C1_include_50k` | prepare-commit-msg | raw_capture | 856 | 856 |
| `C1_include_50k` | prepare-commit-msg | read_facts | 197 | 197 |
| `C1_include_50k` | prepare-commit-msg | redact | 17110 | 17110 |
| `C1_include_50k` | prepare-commit-msg | rule_packs | 29 | 29 |
| `C1_include_50k` | prepare-commit-msg | tokio_build | 22 | 22 |
| `C1_include_50k` | prepare-commit-msg | total | 23115 | 23115 |
| `C2_include_250k` | post-commit | discover | 28 | 28 |
| `C2_include_250k` | post-commit | enqueue | 409 | 409 |
| `C2_include_250k` | post-commit | gix_open | 660 | 660 |
| `C2_include_250k` | post-commit | head_hex | 363 | 363 |
| `C2_include_250k` | post-commit | live_sessions | 587 | 587 |
| `C2_include_250k` | post-commit | spawn_worker | 1289 | 1289 |
| `C2_include_250k` | post-commit | total | 3526 | 3526 |
| `C2_include_250k` | prepare-commit-msg | attribution_preview | 343 | 343 |
| `C2_include_250k` | prepare-commit-msg | discover | 28 | 28 |
| `C2_include_250k` | prepare-commit-msg | live_sessions | 551 | 551 |
| `C2_include_250k` | prepare-commit-msg | open_projection | 2586 | 2586 |
| `C2_include_250k` | prepare-commit-msg | predict_id | 27436 | 1771 (parent, `redact` subtracted) |
| `C2_include_250k` | prepare-commit-msg | raw_capture | 3185 | 3185 |
| `C2_include_250k` | prepare-commit-msg | read_facts | 185 | 185 |
| `C2_include_250k` | prepare-commit-msg | redact | 25665 | 25665 |
| `C2_include_250k` | prepare-commit-msg | rule_packs | 25 | 25 |
| `C2_include_250k` | prepare-commit-msg | tokio_build | 27 | 27 |
| `C2_include_250k` | prepare-commit-msg | total | 35330 | 35330 |
| `C3_include_1mib` | post-commit | discover | 42 | 42 |
| `C3_include_1mib` | post-commit | enqueue | 542 | 542 |
| `C3_include_1mib` | post-commit | gix_open | 912 | 912 |
| `C3_include_1mib` | post-commit | head_hex | 550 | 550 |
| `C3_include_1mib` | post-commit | live_sessions | 718 | 718 |
| `C3_include_1mib` | post-commit | spawn_worker | 1708 | 1708 |
| `C3_include_1mib` | post-commit | total | 4707 | 4707 |
| `C3_include_1mib` | prepare-commit-msg | attribution_preview | 413 | 413 |
| `C3_include_1mib` | prepare-commit-msg | discover | 331 | 331 |
| `C3_include_1mib` | prepare-commit-msg | live_sessions | 1845 | 1845 |
| `C3_include_1mib` | prepare-commit-msg | open_projection | 5636 | 5636 |
| `C3_include_1mib` | prepare-commit-msg | predict_id | 70244 | 5745 (parent, `redact` subtracted) |
| `C3_include_1mib` | prepare-commit-msg | raw_capture | 4844 | 4844 |
| `C3_include_1mib` | prepare-commit-msg | read_facts | 191 | 191 |
| `C3_include_1mib` | prepare-commit-msg | redact | 64499 | 64499 |
| `C3_include_1mib` | prepare-commit-msg | rule_packs | 34 | 34 |
| `C3_include_1mib` | prepare-commit-msg | tokio_build | 31 | 31 |
| `C3_include_1mib` | prepare-commit-msg | total | 85217 | 85217 |
| `D1_newline_50k` | post-commit | discover | 43 | 43 |
| `D1_newline_50k` | post-commit | enqueue | 487 | 487 |
| `D1_newline_50k` | post-commit | gix_open | 980 | 980 |
| `D1_newline_50k` | post-commit | head_hex | 439 | 439 |
| `D1_newline_50k` | post-commit | live_sessions | 545 | 545 |
| `D1_newline_50k` | post-commit | spawn_worker | 1542 | 1542 |
| `D1_newline_50k` | post-commit | total | 4336 | 4336 |
| `D1_newline_50k` | prepare-commit-msg | attribution_preview | 516 | 516 |
| `D1_newline_50k` | prepare-commit-msg | discover | 42 | 42 |
| `D1_newline_50k` | prepare-commit-msg | live_sessions | 1403 | 1403 |
| `D1_newline_50k` | prepare-commit-msg | open_projection | 3953 | 3953 |
| `D1_newline_50k` | prepare-commit-msg | predict_id | 29296 | 821 (parent, `redact` subtracted) |
| `D1_newline_50k` | prepare-commit-msg | raw_capture | 2445 | 2445 |
| `D1_newline_50k` | prepare-commit-msg | read_facts | 181 | 181 |
| `D1_newline_50k` | prepare-commit-msg | redact | 28475 | 28475 |
| `D1_newline_50k` | prepare-commit-msg | rule_packs | 38 | 38 |
| `D1_newline_50k` | prepare-commit-msg | tokio_build | 29 | 29 |
| `D1_newline_50k` | prepare-commit-msg | total | 38967 | 38967 |
| `D2_newline_250k` | post-commit | discover | 53 | 53 |
| `D2_newline_250k` | post-commit | enqueue | 651 | 651 |
| `D2_newline_250k` | post-commit | gix_open | 1261 | 1261 |
| `D2_newline_250k` | post-commit | head_hex | 620 | 620 |
| `D2_newline_250k` | post-commit | live_sessions | 839 | 839 |
| `D2_newline_250k` | post-commit | spawn_worker | 2007 | 2007 |
| `D2_newline_250k` | post-commit | total | 5747 | 5747 |
| `D2_newline_250k` | prepare-commit-msg | attribution_preview | 690 | 690 |
| `D2_newline_250k` | prepare-commit-msg | discover | 58 | 58 |
| `D2_newline_250k` | prepare-commit-msg | live_sessions | 965 | 965 |
| `D2_newline_250k` | prepare-commit-msg | open_projection | 3914 | 3914 |
| `D2_newline_250k` | prepare-commit-msg | predict_id | 71290 | 5301 (parent, `redact` subtracted) |
| `D2_newline_250k` | prepare-commit-msg | raw_capture | 10613 | 10613 |
| `D2_newline_250k` | prepare-commit-msg | read_facts | 173 | 173 |
| `D2_newline_250k` | prepare-commit-msg | redact | 65989 | 65989 |
| `D2_newline_250k` | prepare-commit-msg | rule_packs | 41 | 41 |
| `D2_newline_250k` | prepare-commit-msg | tokio_build | 26 | 26 |
| `D2_newline_250k` | prepare-commit-msg | total | 89470 | 89470 |
| `D3_newline_1mib` | post-commit | discover | 29 | 29 |
| `D3_newline_1mib` | post-commit | enqueue | 450 | 450 |
| `D3_newline_1mib` | post-commit | gix_open | 759 | 759 |
| `D3_newline_1mib` | post-commit | head_hex | 457 | 457 |
| `D3_newline_1mib` | post-commit | live_sessions | 539 | 539 |
| `D3_newline_1mib` | post-commit | spawn_worker | 1252 | 1252 |
| `D3_newline_1mib` | post-commit | total | 3697 | 3697 |
| `D3_newline_1mib` | prepare-commit-msg | attribution_preview | 362 | 362 |
| `D3_newline_1mib` | prepare-commit-msg | discover | 30 | 30 |
| `D3_newline_1mib` | prepare-commit-msg | live_sessions | 576 | 576 |
| `D3_newline_1mib` | prepare-commit-msg | open_projection | 2399 | 2399 |
| `D3_newline_1mib` | prepare-commit-msg | predict_id | 123040 | 10121 (parent, `redact` subtracted) |
| `D3_newline_1mib` | prepare-commit-msg | raw_capture | 23955 | 23955 |
| `D3_newline_1mib` | prepare-commit-msg | read_facts | 116 | 116 |
| `D3_newline_1mib` | prepare-commit-msg | redact | 112919 | 112919 |
| `D3_newline_1mib` | prepare-commit-msg | rule_packs | 50 | 50 |
| `D3_newline_1mib` | prepare-commit-msg | tokio_build | 21 | 21 |
| `D3_newline_1mib` | prepare-commit-msg | total | 152636 | 152636 |

### Phase audit

`total` is measured independently of the phases, so disjoint phases can never exceed it. `over-attributed` is therefore a proof that something is nested which the harness does not declare — not a rounding remark.

| arm | hook | total µs | attributed µs | unattributed µs | verdict |
|---|---|---|---|---|---|
| `B0_idle` | post-commit | 2732 | 2549 | 183 | adds up |
| `B0_idle` | prepare-commit-msg | 111 | 37 | 74 | adds up |
| `B1_live` | post-commit | 3200 | 3006 | 194 | adds up |
| `B1_live` | prepare-commit-msg | 18799 | 18175 | 624 | adds up |
| `C1_include_50k` | post-commit | 3409 | 3193 | 216 | adds up |
| `C1_include_50k` | prepare-commit-msg | 23115 | 22398 | 717 | adds up |
| `C2_include_250k` | post-commit | 3526 | 3336 | 190 | adds up |
| `C2_include_250k` | prepare-commit-msg | 35330 | 34366 | 964 | adds up |
| `C3_include_1mib` | post-commit | 4707 | 4472 | 235 | adds up |
| `C3_include_1mib` | prepare-commit-msg | 85217 | 83569 | 1648 | adds up |
| `D1_newline_50k` | post-commit | 4336 | 4036 | 300 | adds up |
| `D1_newline_50k` | prepare-commit-msg | 38967 | 37903 | 1064 | adds up |
| `D2_newline_250k` | post-commit | 5747 | 5431 | 316 | adds up |
| `D2_newline_250k` | prepare-commit-msg | 89470 | 87770 | 1700 | adds up |
| `D3_newline_1mib` | post-commit | 3697 | 3486 | 211 | adds up |
| `D3_newline_1mib` | prepare-commit-msg | 152636 | 150549 | 2087 | adds up |
