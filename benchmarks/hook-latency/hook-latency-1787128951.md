# Hook latency — git-latency sweep

- harness `0.0.1`, artifact schema 1, generated 1787128951
- host: macos / x86_64 (8 logical CPUs)
- control git: git version 2.50.1 (Apple Git-155)
- protocol: 3 warmup + 21 measured commits per arm
- budget: **100 ms** of git latency (a hard budget, set by the owner)

## Subject

`/Users/ulpian/Documents/codebase/rift/rift-lane-substrate/target/release/rift` — rift 0.0.1, resolved via repo target/release, built 1787120056

Newest source in the checkout: `crates/rift-shadow/src/lib.rs` at 1787119840 (302 files).

## Arms

`hook latency` is this arm's p50 minus the control's, in the same run; `× bare` is the ratio, and the ratio is the portable number.

| arm | p50 | min | hook latency | × bare | verdict |
|---|---|---|---|---|---|
| `A_bare` | 47.94 ms | 44.10 ms | — | 1.00× | control |
| `D1_newline_50k` | 161.29 ms | 132.92 ms | 113.35 ms | 3.36× | **BREACH** |
| `E1_newline_64k` | 179.27 ms | 134.93 ms | 131.33 ms | 3.74× | **BREACH** |
| `E2_newline_96k` | 176.04 ms | 133.82 ms | 128.10 ms | 3.67× | **BREACH** |
| `E3_newline_128k` | 177.73 ms | 134.14 ms | 129.79 ms | 3.71× | **BREACH** |
| `E4_newline_192k` | 183.28 ms | 149.16 ms | 135.35 ms | 3.82× | **BREACH** |
| `E5_newline_250k_8f` | 194.47 ms | 158.66 ms | 146.54 ms | 4.06× | **BREACH** |

- `A_bare` — bare commit, no hooks installed (control)

- `D1_newline_50k` — live session + `.worktreeinclude` over 8 files (51200 B read, 102592 B handed on, 2.00x)

- `E1_newline_64k` — live session + `.worktreeinclude` over 8 files (65536 B read, 131264 B handed on, 2.00x)

- `E2_newline_96k` — live session + `.worktreeinclude` over 8 files (98304 B read, 196800 B handed on, 2.00x)

- `E3_newline_128k` — live session + `.worktreeinclude` over 8 files (131072 B read, 262336 B handed on, 2.00x)

- `E4_newline_192k` — live session + `.worktreeinclude` over 8 files (196608 B read, 393408 B handed on, 2.00x)

- `E5_newline_250k_8f` — live session + `.worktreeinclude` over 8 files (256000 B read, 512192 B handed on, 2.00x)

## Opted-in corpus

`MAX_TOTAL_BYTES` is 1 MiB and is charged on bytes **read**; `Walk::file` prefixes every content line with one space, so N bytes contribute up to **2N + 2** to the document the redactor is handed. Both numbers are published, because an arm described as "1 MiB" without saying which 1 MiB misstates the ceiling in one direction or the other.

| arm | files | bytes read | budget | document | factor | files read (explain) |
|---|---|---|---|---|---|---|
| `D1_newline_50k` | 8 | 51200 | under | 102592 | 2.00× | — |
| `E1_newline_64k` | 8 | 65536 | under | 131264 | 2.00× | — |
| `E2_newline_96k` | 8 | 98304 | under | 196800 | 2.00× | — |
| `E3_newline_128k` | 8 | 131072 | under | 262336 | 2.00× | — |
| `E4_newline_192k` | 8 | 196608 | under | 393408 | 2.00× | — |
| `E5_newline_250k_8f` | 8 | 256000 | under | 512192 | 2.00× | — |

The factor here is near 1.0 because these lines are long. The 2× is the pathological end — a file that is all newlines, where the one-space prefix is the whole line — so a full budget can hand on a document just over 2 MiB. Neither end is the other, and this table says which one each arm is.

`files read (explain)` is `rift checkpoint explain`'s **own** label, not a span count: the inclusion walk's record count is on no CLI surface today, so the harness publishes the figure the product publishes rather than relabelling it as something it has not established.

## Anatomy — the curve, the units, and the phase that owns it

**BREACH: `D1_newline_50k`, `E1_newline_64k`, `E2_newline_96k`, `E3_newline_128k`, `E4_newline_192k`, `E5_newline_250k_8f`.** The budget is 100 ms of git latency and is HARD (set by the owner). The corpus was not adjusted in response — the arms are the sizes they were declared at.

**Magnitude and attribution come from different commits.** `git latency` is the median of the timed commits minus the control's; every phase column comes from the arm's one **extra untimed** commit with the timer on. Read the residue as an anatomy of a representative commit beside the magnitude of the median, never as an exact decomposition of one commit.

`predict_id` here is **exclusive** — `redact` subtracted — so the columns are disjoint and may be added.

| arm | bytes read | handed on | ×read | git latency | hook bodies | outside bodies | `redact` | `predict_id` less `redact` | verdict |
|---|---|---|---|---|---|---|---|---|---|
| `D1_newline_50k` | 51200 | 102592 | 2.00× | 113.35 ms | 44.33 ms | 69.02 ms | 29.24 ms | 0.91 ms | **BREACH** |
| `E1_newline_64k` | 65536 | 131264 | 2.00× | 131.33 ms | 52.62 ms | 78.71 ms | 33.21 ms | 1.22 ms | **BREACH** |
| `E2_newline_96k` | 98304 | 196800 | 2.00× | 128.10 ms | 47.37 ms | 80.73 ms | 28.43 ms | 1.11 ms | **BREACH** |
| `E3_newline_128k` | 131072 | 262336 | 2.00× | 129.79 ms | 53.20 ms | 76.59 ms | 34.06 ms | 1.52 ms | **BREACH** |
| `E4_newline_192k` | 196608 | 393408 | 2.00× | 135.35 ms | 51.89 ms | 83.46 ms | 32.12 ms | 1.96 ms | **BREACH** |
| `E5_newline_250k_8f` | 256000 | 512192 | 2.00× | 146.53 ms | 77.24 ms | 69.30 ms | 51.46 ms | 4.29 ms | **BREACH** |

`outside bodies` is the floor a Rust change cannot move: three `/bin/sh` invocations and three execs that git requires, plus git's own cost of running them. If the budget ever has to halve, that column is what has to be attacked, and it is not in Rust code.

## Oracles — what each arm exercised

Reported for every arm including the control, because a check that only ever confirms is not a check: the idle arm's **absent** trailer is what makes the live arm's present one mean something.

| arm | projections | trailer | verify | events |
|---|---|---|---|---|
| `A_bare` | none | absent | — | — |
| `D1_newline_50k` | rift/<repo-id>/projection.db 679936 B; rift/canopy/ui-state.db 90112 B | present | 22 checkpoint(s), 23 shard(s) | 100 |
| `E1_newline_64k` | rift/<repo-id>/projection.db 679936 B; rift/canopy/ui-state.db 90112 B | present | 22 checkpoint(s), 23 shard(s) | 100 |
| `E2_newline_96k` | rift/<repo-id>/projection.db 679936 B; rift/canopy/ui-state.db 90112 B | present | 22 checkpoint(s), 22 shard(s) | 100 |
| `E3_newline_128k` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 679936 B | present | 22 checkpoint(s), 23 shard(s) | 100 |
| `E4_newline_192k` | rift/<repo-id>/projection.db 671744 B; rift/canopy/ui-state.db 90112 B | present | 21 checkpoint(s), 22 shard(s) | 100 |
| `E5_newline_250k_8f` | rift/<repo-id>/projection.db 671744 B; rift/canopy/ui-state.db 90112 B | present | 21 checkpoint(s), 21 shard(s) | 100 |

**More than one projection under a single arm's cache.** That is the shape of the defect this table exists for: the number may be about a store rift never opened.

## Phases

From one extra **untimed** commit per arm with `RIFT_HOOK_TIMING=1`, straight out of the shipped binary. Untimed because the timer takes clock readings the shipped default never takes.

**Read the `exclusive` column, and never sum `recorded`.** The timer records on completion, so a nested phase prints before its parent and the parent's figure already contains it: `predict_id` INCLUDES `redact`. `recorded` is what the binary printed; `exclusive` has every declared child subtracted, so those values are disjoint and may be added. Summing `recorded` would double-count the redaction — the one quantity the opted-in arms exist to separate.

| arm | hook | phase | recorded µs | exclusive µs |
|---|---|---|---|---|
| `D1_newline_50k` | post-commit | discover | 39 | 39 |
| `D1_newline_50k` | post-commit | enqueue | 649 | 649 |
| `D1_newline_50k` | post-commit | gix_open | 1092 | 1092 |
| `D1_newline_50k` | post-commit | head_hex | 544 | 544 |
| `D1_newline_50k` | post-commit | live_sessions | 1425 | 1425 |
| `D1_newline_50k` | post-commit | spawn_worker | 1816 | 1816 |
| `D1_newline_50k` | post-commit | total | 5848 | 5848 |
| `D1_newline_50k` | prepare-commit-msg | attribution_preview | 606 | 606 |
| `D1_newline_50k` | prepare-commit-msg | discover | 50 | 50 |
| `D1_newline_50k` | prepare-commit-msg | live_sessions | 1426 | 1426 |
| `D1_newline_50k` | prepare-commit-msg | open_projection | 3445 | 3445 |
| `D1_newline_50k` | prepare-commit-msg | predict_id | 30151 | 913 (parent, `redact` subtracted) |
| `D1_newline_50k` | prepare-commit-msg | raw_capture | 1459 | 1459 |
| `D1_newline_50k` | prepare-commit-msg | read_facts | 155 | 155 |
| `D1_newline_50k` | prepare-commit-msg | redact | 29238 | 29238 |
| `D1_newline_50k` | prepare-commit-msg | rule_packs | 18 | 18 |
| `D1_newline_50k` | prepare-commit-msg | tokio_build | 23 | 23 |
| `D1_newline_50k` | prepare-commit-msg | total | 38479 | 38479 |
| `E1_newline_64k` | post-commit | discover | 43 | 43 |
| `E1_newline_64k` | post-commit | enqueue | 684 | 684 |
| `E1_newline_64k` | post-commit | gix_open | 1367 | 1367 |
| `E1_newline_64k` | post-commit | head_hex | 640 | 640 |
| `E1_newline_64k` | post-commit | live_sessions | 1699 | 1699 |
| `E1_newline_64k` | post-commit | spawn_worker | 1832 | 1832 |
| `E1_newline_64k` | post-commit | total | 6581 | 6581 |
| `E1_newline_64k` | prepare-commit-msg | attribution_preview | 608 | 608 |
| `E1_newline_64k` | prepare-commit-msg | discover | 78 | 78 |
| `E1_newline_64k` | prepare-commit-msg | live_sessions | 2203 | 2203 |
| `E1_newline_64k` | prepare-commit-msg | open_projection | 4409 | 4409 |
| `E1_newline_64k` | prepare-commit-msg | predict_id | 34428 | 1220 (parent, `redact` subtracted) |
| `E1_newline_64k` | prepare-commit-msg | raw_capture | 2853 | 2853 |
| `E1_newline_64k` | prepare-commit-msg | read_facts | 201 | 201 |
| `E1_newline_64k` | prepare-commit-msg | redact | 33208 | 33208 |
| `E1_newline_64k` | prepare-commit-msg | rule_packs | 44 | 44 |
| `E1_newline_64k` | prepare-commit-msg | tokio_build | 30 | 30 |
| `E1_newline_64k` | prepare-commit-msg | total | 46036 | 46036 |
| `E2_newline_96k` | post-commit | discover | 45 | 45 |
| `E2_newline_96k` | post-commit | enqueue | 803 | 803 |
| `E2_newline_96k` | post-commit | gix_open | 1126 | 1126 |
| `E2_newline_96k` | post-commit | head_hex | 573 | 573 |
| `E2_newline_96k` | post-commit | live_sessions | 1825 | 1825 |
| `E2_newline_96k` | post-commit | spawn_worker | 3018 | 3018 |
| `E2_newline_96k` | post-commit | total | 7707 | 7707 |
| `E2_newline_96k` | prepare-commit-msg | attribution_preview | 366 | 366 |
| `E2_newline_96k` | prepare-commit-msg | discover | 34 | 34 |
| `E2_newline_96k` | prepare-commit-msg | live_sessions | 1152 | 1152 |
| `E2_newline_96k` | prepare-commit-msg | open_projection | 3973 | 3973 |
| `E2_newline_96k` | prepare-commit-msg | predict_id | 29539 | 1108 (parent, `redact` subtracted) |
| `E2_newline_96k` | prepare-commit-msg | raw_capture | 3523 | 3523 |
| `E2_newline_96k` | prepare-commit-msg | read_facts | 182 | 182 |
| `E2_newline_96k` | prepare-commit-msg | redact | 28431 | 28431 |
| `E2_newline_96k` | prepare-commit-msg | rule_packs | 40 | 40 |
| `E2_newline_96k` | prepare-commit-msg | tokio_build | 29 | 29 |
| `E2_newline_96k` | prepare-commit-msg | total | 39664 | 39664 |
| `E3_newline_128k` | post-commit | discover | 31 | 31 |
| `E3_newline_128k` | post-commit | enqueue | 652 | 652 |
| `E3_newline_128k` | post-commit | gix_open | 1049 | 1049 |
| `E3_newline_128k` | post-commit | head_hex | 570 | 570 |
| `E3_newline_128k` | post-commit | live_sessions | 2014 | 2014 |
| `E3_newline_128k` | post-commit | spawn_worker | 1802 | 1802 |
| `E3_newline_128k` | post-commit | total | 6413 | 6413 |
| `E3_newline_128k` | prepare-commit-msg | attribution_preview | 490 | 490 |
| `E3_newline_128k` | prepare-commit-msg | discover | 40 | 40 |
| `E3_newline_128k` | prepare-commit-msg | live_sessions | 1168 | 1168 |
| `E3_newline_128k` | prepare-commit-msg | open_projection | 3878 | 3878 |
| `E3_newline_128k` | prepare-commit-msg | predict_id | 35583 | 1519 (parent, `redact` subtracted) |
| `E3_newline_128k` | prepare-commit-msg | raw_capture | 4496 | 4496 |
| `E3_newline_128k` | prepare-commit-msg | read_facts | 196 | 196 |
| `E3_newline_128k` | prepare-commit-msg | redact | 34064 | 34064 |
| `E3_newline_128k` | prepare-commit-msg | rule_packs | 43 | 43 |
| `E3_newline_128k` | prepare-commit-msg | tokio_build | 27 | 27 |
| `E3_newline_128k` | prepare-commit-msg | total | 46792 | 46792 |
| `E4_newline_192k` | post-commit | discover | 31 | 31 |
| `E4_newline_192k` | post-commit | enqueue | 469 | 469 |
| `E4_newline_192k` | post-commit | gix_open | 799 | 799 |
| `E4_newline_192k` | post-commit | head_hex | 361 | 361 |
| `E4_newline_192k` | post-commit | live_sessions | 931 | 931 |
| `E4_newline_192k` | post-commit | spawn_worker | 1994 | 1994 |
| `E4_newline_192k` | post-commit | total | 4842 | 4842 |
| `E4_newline_192k` | prepare-commit-msg | attribution_preview | 370 | 370 |
| `E4_newline_192k` | prepare-commit-msg | discover | 31 | 31 |
| `E4_newline_192k` | prepare-commit-msg | live_sessions | 963 | 963 |
| `E4_newline_192k` | prepare-commit-msg | open_projection | 3578 | 3578 |
| `E4_newline_192k` | prepare-commit-msg | predict_id | 34075 | 1956 (parent, `redact` subtracted) |
| `E4_newline_192k` | prepare-commit-msg | raw_capture | 6849 | 6849 |
| `E4_newline_192k` | prepare-commit-msg | read_facts | 190 | 190 |
| `E4_newline_192k` | prepare-commit-msg | redact | 32119 | 32119 |
| `E4_newline_192k` | prepare-commit-msg | rule_packs | 30 | 30 |
| `E4_newline_192k` | prepare-commit-msg | tokio_build | 27 | 27 |
| `E4_newline_192k` | prepare-commit-msg | total | 47048 | 47048 |
| `E5_newline_250k_8f` | post-commit | discover | 59 | 59 |
| `E5_newline_250k_8f` | post-commit | enqueue | 653 | 653 |
| `E5_newline_250k_8f` | post-commit | gix_open | 1297 | 1297 |
| `E5_newline_250k_8f` | post-commit | head_hex | 617 | 617 |
| `E5_newline_250k_8f` | post-commit | live_sessions | 1475 | 1475 |
| `E5_newline_250k_8f` | post-commit | spawn_worker | 2143 | 2143 |
| `E5_newline_250k_8f` | post-commit | total | 6572 | 6572 |
| `E5_newline_250k_8f` | prepare-commit-msg | attribution_preview | 430 | 430 |
| `E5_newline_250k_8f` | prepare-commit-msg | discover | 39 | 39 |
| `E5_newline_250k_8f` | prepare-commit-msg | live_sessions | 1399 | 1399 |
| `E5_newline_250k_8f` | prepare-commit-msg | open_projection | 4434 | 4434 |
| `E5_newline_250k_8f` | prepare-commit-msg | predict_id | 55757 | 4293 (parent, `redact` subtracted) |
| `E5_newline_250k_8f` | prepare-commit-msg | raw_capture | 6805 | 6805 |
| `E5_newline_250k_8f` | prepare-commit-msg | read_facts | 180 | 180 |
| `E5_newline_250k_8f` | prepare-commit-msg | redact | 51464 | 51464 |
| `E5_newline_250k_8f` | prepare-commit-msg | rule_packs | 45 | 45 |
| `E5_newline_250k_8f` | prepare-commit-msg | tokio_build | 28 | 28 |
| `E5_newline_250k_8f` | prepare-commit-msg | total | 70665 | 70665 |

### Phase audit

`total` is measured independently of the phases, so disjoint phases can never exceed it. `over-attributed` is therefore a proof that something is nested which the harness does not declare — not a rounding remark.

| arm | hook | total µs | attributed µs | unattributed µs | verdict |
|---|---|---|---|---|---|
| `D1_newline_50k` | post-commit | 5848 | 5565 | 283 | adds up |
| `D1_newline_50k` | prepare-commit-msg | 38479 | 37333 | 1146 | adds up |
| `E1_newline_64k` | post-commit | 6581 | 6265 | 316 | adds up |
| `E1_newline_64k` | prepare-commit-msg | 46036 | 44854 | 1182 | adds up |
| `E2_newline_96k` | post-commit | 7707 | 7390 | 317 | adds up |
| `E2_newline_96k` | prepare-commit-msg | 39664 | 38838 | 826 | adds up |
| `E3_newline_128k` | post-commit | 6413 | 6118 | 295 | adds up |
| `E3_newline_128k` | prepare-commit-msg | 46792 | 45921 | 871 | adds up |
| `E4_newline_192k` | post-commit | 4842 | 4585 | 257 | adds up |
| `E4_newline_192k` | prepare-commit-msg | 47048 | 46113 | 935 | adds up |
| `E5_newline_250k_8f` | post-commit | 6572 | 6244 | 328 | adds up |
| `E5_newline_250k_8f` | prepare-commit-msg | 70665 | 69117 | 1548 | adds up |
