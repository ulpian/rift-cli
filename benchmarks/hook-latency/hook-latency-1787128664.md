# Hook latency — git-latency sweep

- harness `0.0.1`, artifact schema 1, generated 1787128664
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
| `A_bare` | 57.51 ms | 50.00 ms | — | 1.00× | control |
| `D1_newline_50k` | 192.92 ms | 148.66 ms | 135.40 ms | 3.35× | **BREACH** |
| `D2_newline_250k` | 199.48 ms | 166.00 ms | 141.97 ms | 3.47× | **BREACH** |
| `E1_newline_64k` | 153.94 ms | 144.87 ms | 96.43 ms | 2.68× | meets |
| `E2_newline_96k` | 165.41 ms | 147.87 ms | 107.90 ms | 2.88× | **BREACH** |
| `E3_newline_128k` | 152.66 ms | 139.25 ms | 95.15 ms | 2.65× | meets |
| `E4_newline_192k` | 200.32 ms | 171.80 ms | 142.81 ms | 3.48× | **BREACH** |
| `E5_newline_250k_8f` | 177.53 ms | 156.67 ms | 120.02 ms | 3.09× | **BREACH** |

- `A_bare` — bare commit, no hooks installed (control)

- `D1_newline_50k` — live session + `.worktreeinclude` over 8 files (51200 B read, 102592 B handed on, 2.00x)

- `D2_newline_250k` — live session + `.worktreeinclude` over 40 files (256000 B read, 512960 B handed on, 2.00x)

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
| `D2_newline_250k` | 40 | 256000 | under | 512960 | 2.00× | — |
| `E1_newline_64k` | 8 | 65536 | under | 131264 | 2.00× | — |
| `E2_newline_96k` | 8 | 98304 | under | 196800 | 2.00× | — |
| `E3_newline_128k` | 8 | 131072 | under | 262336 | 2.00× | — |
| `E4_newline_192k` | 8 | 196608 | under | 393408 | 2.00× | — |
| `E5_newline_250k_8f` | 8 | 256000 | under | 512192 | 2.00× | — |

The factor here is near 1.0 because these lines are long. The 2× is the pathological end — a file that is all newlines, where the one-space prefix is the whole line — so a full budget can hand on a document just over 2 MiB. Neither end is the other, and this table says which one each arm is.

`files read (explain)` is `rift checkpoint explain`'s **own** label, not a span count: the inclusion walk's record count is on no CLI surface today, so the harness publishes the figure the product publishes rather than relabelling it as something it has not established.

## Anatomy — the curve, the units, and the phase that owns it

**BREACH: `D1_newline_50k`, `D2_newline_250k`, `E2_newline_96k`, `E4_newline_192k`, `E5_newline_250k_8f`.** The budget is 100 ms of git latency and is HARD (set by the owner). The corpus was not adjusted in response — the arms are the sizes they were declared at.

**Magnitude and attribution come from different commits.** `git latency` is the median of the timed commits minus the control's; every phase column comes from the arm's one **extra untimed** commit with the timer on. Read the residue as an anatomy of a representative commit beside the magnitude of the median, never as an exact decomposition of one commit.

`predict_id` here is **exclusive** — `redact` subtracted — so the columns are disjoint and may be added.

| arm | bytes read | handed on | ×read | git latency | hook bodies | outside bodies | `redact` | `predict_id` less `redact` | verdict |
|---|---|---|---|---|---|---|---|---|---|
| `D1_newline_50k` | 51200 | 102592 | 2.00× | 135.40 ms | 51.66 ms | 83.75 ms | 34.45 ms | 0.87 ms | **BREACH** |
| `D2_newline_250k` | 256000 | 512960 | 2.00× | 141.97 ms | 62.68 ms | 79.29 ms | 40.01 ms | 2.79 ms | **BREACH** |
| `E1_newline_64k` | 65536 | 131264 | 2.00× | 96.43 ms | 38.97 ms | 57.46 ms | 25.88 ms | 0.69 ms | meets |
| `E2_newline_96k` | 98304 | 196800 | 2.00× | 107.90 ms | 48.27 ms | 59.62 ms | 34.30 ms | 1.63 ms | **BREACH** |
| `E3_newline_128k` | 131072 | 262336 | 2.00× | 95.15 ms | 57.43 ms | 37.72 ms | 36.66 ms | 2.54 ms | meets |
| `E4_newline_192k` | 196608 | 393408 | 2.00× | 142.81 ms | 80.11 ms | 62.70 ms | 55.00 ms | 3.45 ms | **BREACH** |
| `E5_newline_250k_8f` | 256000 | 512192 | 2.00× | 120.02 ms | 62.57 ms | 57.45 ms | 41.61 ms | 3.16 ms | **BREACH** |

`outside bodies` is the floor a Rust change cannot move: three `/bin/sh` invocations and three execs that git requires, plus git's own cost of running them. If the budget ever has to halve, that column is what has to be attacked, and it is not in Rust code.

## Oracles — what each arm exercised

Reported for every arm including the control, because a check that only ever confirms is not a check: the idle arm's **absent** trailer is what makes the live arm's present one mean something.

| arm | projections | trailer | verify | events |
|---|---|---|---|---|
| `A_bare` | none | absent | — | — |
| `D1_newline_50k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `D2_newline_250k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 13 checkpoint(s), 14 shard(s) | 60 |
| `E1_newline_64k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `E2_newline_96k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `E3_newline_128k` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 565248 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `E4_newline_192k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `E5_newline_250k_8f` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 14 shard(s) | 60 |

**More than one projection under a single arm's cache.** That is the shape of the defect this table exists for: the number may be about a store rift never opened.

## Phases

From one extra **untimed** commit per arm with `RIFT_HOOK_TIMING=1`, straight out of the shipped binary. Untimed because the timer takes clock readings the shipped default never takes.

**Read the `exclusive` column, and never sum `recorded`.** The timer records on completion, so a nested phase prints before its parent and the parent's figure already contains it: `predict_id` INCLUDES `redact`. `recorded` is what the binary printed; `exclusive` has every declared child subtracted, so those values are disjoint and may be added. Summing `recorded` would double-count the redaction — the one quantity the opted-in arms exist to separate.

| arm | hook | phase | recorded µs | exclusive µs |
|---|---|---|---|---|
| `D1_newline_50k` | post-commit | discover | 58 | 58 |
| `D1_newline_50k` | post-commit | enqueue | 738 | 738 |
| `D1_newline_50k` | post-commit | gix_open | 1313 | 1313 |
| `D1_newline_50k` | post-commit | head_hex | 573 | 573 |
| `D1_newline_50k` | post-commit | live_sessions | 1082 | 1082 |
| `D1_newline_50k` | post-commit | spawn_worker | 1683 | 1683 |
| `D1_newline_50k` | post-commit | total | 5762 | 5762 |
| `D1_newline_50k` | prepare-commit-msg | attribution_preview | 864 | 864 |
| `D1_newline_50k` | prepare-commit-msg | discover | 61 | 61 |
| `D1_newline_50k` | prepare-commit-msg | live_sessions | 911 | 911 |
| `D1_newline_50k` | prepare-commit-msg | open_projection | 4572 | 4572 |
| `D1_newline_50k` | prepare-commit-msg | predict_id | 35322 | 870 (parent, `redact` subtracted) |
| `D1_newline_50k` | prepare-commit-msg | raw_capture | 2479 | 2479 |
| `D1_newline_50k` | prepare-commit-msg | read_facts | 214 | 214 |
| `D1_newline_50k` | prepare-commit-msg | redact | 34452 | 34452 |
| `D1_newline_50k` | prepare-commit-msg | rule_packs | 30 | 30 |
| `D1_newline_50k` | prepare-commit-msg | tokio_build | 33 | 33 |
| `D1_newline_50k` | prepare-commit-msg | total | 45894 | 45894 |
| `D2_newline_250k` | post-commit | discover | 59 | 59 |
| `D2_newline_250k` | post-commit | enqueue | 706 | 706 |
| `D2_newline_250k` | post-commit | gix_open | 1199 | 1199 |
| `D2_newline_250k` | post-commit | head_hex | 701 | 701 |
| `D2_newline_250k` | post-commit | live_sessions | 1000 | 1000 |
| `D2_newline_250k` | post-commit | spawn_worker | 1580 | 1580 |
| `D2_newline_250k` | post-commit | total | 5559 | 5559 |
| `D2_newline_250k` | prepare-commit-msg | attribution_preview | 413 | 413 |
| `D2_newline_250k` | prepare-commit-msg | discover | 34 | 34 |
| `D2_newline_250k` | prepare-commit-msg | live_sessions | 876 | 876 |
| `D2_newline_250k` | prepare-commit-msg | open_projection | 3289 | 3289 |
| `D2_newline_250k` | prepare-commit-msg | predict_id | 42800 | 2788 (parent, `redact` subtracted) |
| `D2_newline_250k` | prepare-commit-msg | raw_capture | 8409 | 8409 |
| `D2_newline_250k` | prepare-commit-msg | read_facts | 161 | 161 |
| `D2_newline_250k` | prepare-commit-msg | redact | 40012 | 40012 |
| `D2_newline_250k` | prepare-commit-msg | rule_packs | 22 | 22 |
| `D2_newline_250k` | prepare-commit-msg | tokio_build | 24 | 24 |
| `D2_newline_250k` | prepare-commit-msg | total | 57123 | 57123 |
| `E1_newline_64k` | post-commit | discover | 40 | 40 |
| `E1_newline_64k` | post-commit | enqueue | 452 | 452 |
| `E1_newline_64k` | post-commit | gix_open | 710 | 710 |
| `E1_newline_64k` | post-commit | head_hex | 369 | 369 |
| `E1_newline_64k` | post-commit | live_sessions | 533 | 533 |
| `E1_newline_64k` | post-commit | spawn_worker | 1315 | 1315 |
| `E1_newline_64k` | post-commit | total | 3640 | 3640 |
| `E1_newline_64k` | prepare-commit-msg | attribution_preview | 395 | 395 |
| `E1_newline_64k` | prepare-commit-msg | discover | 30 | 30 |
| `E1_newline_64k` | prepare-commit-msg | live_sessions | 877 | 877 |
| `E1_newline_64k` | prepare-commit-msg | open_projection | 3858 | 3858 |
| `E1_newline_64k` | prepare-commit-msg | predict_id | 26566 | 685 (parent, `redact` subtracted) |
| `E1_newline_64k` | prepare-commit-msg | raw_capture | 2631 | 2631 |
| `E1_newline_64k` | prepare-commit-msg | read_facts | 189 | 189 |
| `E1_newline_64k` | prepare-commit-msg | redact | 25881 | 25881 |
| `E1_newline_64k` | prepare-commit-msg | rule_packs | 19 | 19 |
| `E1_newline_64k` | prepare-commit-msg | tokio_build | 29 | 29 |
| `E1_newline_64k` | prepare-commit-msg | total | 35325 | 35325 |
| `E2_newline_96k` | post-commit | discover | 35 | 35 |
| `E2_newline_96k` | post-commit | enqueue | 471 | 471 |
| `E2_newline_96k` | post-commit | gix_open | 751 | 751 |
| `E2_newline_96k` | post-commit | head_hex | 390 | 390 |
| `E2_newline_96k` | post-commit | live_sessions | 584 | 584 |
| `E2_newline_96k` | post-commit | spawn_worker | 1436 | 1436 |
| `E2_newline_96k` | post-commit | total | 3931 | 3931 |
| `E2_newline_96k` | prepare-commit-msg | attribution_preview | 601 | 601 |
| `E2_newline_96k` | prepare-commit-msg | discover | 73 | 73 |
| `E2_newline_96k` | prepare-commit-msg | live_sessions | 809 | 809 |
| `E2_newline_96k` | prepare-commit-msg | open_projection | 2846 | 2846 |
| `E2_newline_96k` | prepare-commit-msg | predict_id | 35924 | 1629 (parent, `redact` subtracted) |
| `E2_newline_96k` | prepare-commit-msg | raw_capture | 2601 | 2601 |
| `E2_newline_96k` | prepare-commit-msg | read_facts | 144 | 144 |
| `E2_newline_96k` | prepare-commit-msg | redact | 34295 | 34295 |
| `E2_newline_96k` | prepare-commit-msg | rule_packs | 36 | 36 |
| `E2_newline_96k` | prepare-commit-msg | tokio_build | 25 | 25 |
| `E2_newline_96k` | prepare-commit-msg | total | 44343 | 44343 |
| `E3_newline_128k` | post-commit | discover | 45 | 45 |
| `E3_newline_128k` | post-commit | enqueue | 705 | 705 |
| `E3_newline_128k` | post-commit | gix_open | 1116 | 1116 |
| `E3_newline_128k` | post-commit | head_hex | 532 | 532 |
| `E3_newline_128k` | post-commit | live_sessions | 891 | 891 |
| `E3_newline_128k` | post-commit | spawn_worker | 1892 | 1892 |
| `E3_newline_128k` | post-commit | total | 5480 | 5480 |
| `E3_newline_128k` | prepare-commit-msg | attribution_preview | 611 | 611 |
| `E3_newline_128k` | prepare-commit-msg | discover | 61 | 61 |
| `E3_newline_128k` | prepare-commit-msg | live_sessions | 1211 | 1211 |
| `E3_newline_128k` | prepare-commit-msg | open_projection | 4124 | 4124 |
| `E3_newline_128k` | prepare-commit-msg | predict_id | 39208 | 2545 (parent, `redact` subtracted) |
| `E3_newline_128k` | prepare-commit-msg | raw_capture | 5014 | 5014 |
| `E3_newline_128k` | prepare-commit-msg | read_facts | 229 | 229 |
| `E3_newline_128k` | prepare-commit-msg | redact | 36663 | 36663 |
| `E3_newline_128k` | prepare-commit-msg | rule_packs | 43 | 43 |
| `E3_newline_128k` | prepare-commit-msg | tokio_build | 28 | 28 |
| `E3_newline_128k` | prepare-commit-msg | total | 51948 | 51948 |
| `E4_newline_192k` | post-commit | discover | 48 | 48 |
| `E4_newline_192k` | post-commit | enqueue | 691 | 691 |
| `E4_newline_192k` | post-commit | gix_open | 1187 | 1187 |
| `E4_newline_192k` | post-commit | head_hex | 752 | 752 |
| `E4_newline_192k` | post-commit | live_sessions | 1076 | 1076 |
| `E4_newline_192k` | post-commit | spawn_worker | 2091 | 2091 |
| `E4_newline_192k` | post-commit | total | 6198 | 6198 |
| `E4_newline_192k` | prepare-commit-msg | attribution_preview | 630 | 630 |
| `E4_newline_192k` | prepare-commit-msg | discover | 62 | 62 |
| `E4_newline_192k` | prepare-commit-msg | live_sessions | 1115 | 1115 |
| `E4_newline_192k` | prepare-commit-msg | open_projection | 4715 | 4715 |
| `E4_newline_192k` | prepare-commit-msg | predict_id | 58450 | 3446 (parent, `redact` subtracted) |
| `E4_newline_192k` | prepare-commit-msg | raw_capture | 6903 | 6903 |
| `E4_newline_192k` | prepare-commit-msg | read_facts | 207 | 207 |
| `E4_newline_192k` | prepare-commit-msg | redact | 55004 | 55004 |
| `E4_newline_192k` | prepare-commit-msg | rule_packs | 50 | 50 |
| `E4_newline_192k` | prepare-commit-msg | tokio_build | 31 | 31 |
| `E4_newline_192k` | prepare-commit-msg | total | 73913 | 73913 |
| `E5_newline_250k_8f` | post-commit | discover | 32 | 32 |
| `E5_newline_250k_8f` | post-commit | enqueue | 691 | 691 |
| `E5_newline_250k_8f` | post-commit | gix_open | 1075 | 1075 |
| `E5_newline_250k_8f` | post-commit | head_hex | 713 | 713 |
| `E5_newline_250k_8f` | post-commit | live_sessions | 1236 | 1236 |
| `E5_newline_250k_8f` | post-commit | spawn_worker | 2643 | 2643 |
| `E5_newline_250k_8f` | post-commit | total | 6688 | 6688 |
| `E5_newline_250k_8f` | prepare-commit-msg | attribution_preview | 398 | 398 |
| `E5_newline_250k_8f` | prepare-commit-msg | discover | 32 | 32 |
| `E5_newline_250k_8f` | prepare-commit-msg | live_sessions | 700 | 700 |
| `E5_newline_250k_8f` | prepare-commit-msg | open_projection | 3343 | 3343 |
| `E5_newline_250k_8f` | prepare-commit-msg | predict_id | 44774 | 3164 (parent, `redact` subtracted) |
| `E5_newline_250k_8f` | prepare-commit-msg | raw_capture | 5358 | 5358 |
| `E5_newline_250k_8f` | prepare-commit-msg | read_facts | 140 | 140 |
| `E5_newline_250k_8f` | prepare-commit-msg | redact | 41610 | 41610 |
| `E5_newline_250k_8f` | prepare-commit-msg | rule_packs | 45 | 45 |
| `E5_newline_250k_8f` | prepare-commit-msg | tokio_build | 24 | 24 |
| `E5_newline_250k_8f` | prepare-commit-msg | total | 55881 | 55881 |

### Phase audit

`total` is measured independently of the phases, so disjoint phases can never exceed it. `over-attributed` is therefore a proof that something is nested which the harness does not declare — not a rounding remark.

| arm | hook | total µs | attributed µs | unattributed µs | verdict |
|---|---|---|---|---|---|
| `D1_newline_50k` | post-commit | 5762 | 5447 | 315 | adds up |
| `D1_newline_50k` | prepare-commit-msg | 45894 | 44486 | 1408 | adds up |
| `D2_newline_250k` | post-commit | 5559 | 5245 | 314 | adds up |
| `D2_newline_250k` | prepare-commit-msg | 57123 | 56028 | 1095 | adds up |
| `E1_newline_64k` | post-commit | 3640 | 3419 | 221 | adds up |
| `E1_newline_64k` | prepare-commit-msg | 35325 | 34594 | 731 | adds up |
| `E2_newline_96k` | post-commit | 3931 | 3667 | 264 | adds up |
| `E2_newline_96k` | prepare-commit-msg | 44343 | 43059 | 1284 | adds up |
| `E3_newline_128k` | post-commit | 5480 | 5181 | 299 | adds up |
| `E3_newline_128k` | prepare-commit-msg | 51948 | 50529 | 1419 | adds up |
| `E4_newline_192k` | post-commit | 6198 | 5845 | 353 | adds up |
| `E4_newline_192k` | prepare-commit-msg | 73913 | 72163 | 1750 | adds up |
| `E5_newline_250k_8f` | post-commit | 6688 | 6390 | 298 | adds up |
| `E5_newline_250k_8f` | prepare-commit-msg | 55881 | 54814 | 1067 | adds up |
