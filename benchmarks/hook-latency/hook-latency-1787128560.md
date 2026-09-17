# Hook latency — git-latency sweep

- harness `0.0.1`, artifact schema 1, generated 1787128560
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
| `A_bare` | 50.05 ms | 46.40 ms | — | 1.00× | control |
| `D1_newline_50k` | 151.40 ms | 141.14 ms | 101.35 ms | 3.03× | **BREACH** |
| `D2_newline_250k` | 181.03 ms | 155.60 ms | 130.98 ms | 3.62× | **BREACH** |
| `E1_newline_64k` | 168.10 ms | 150.94 ms | 118.05 ms | 3.36× | **BREACH** |
| `E2_newline_96k` | 160.73 ms | 149.47 ms | 110.68 ms | 3.21× | **BREACH** |
| `E3_newline_128k` | 169.58 ms | 155.48 ms | 119.53 ms | 3.39× | **BREACH** |
| `E4_newline_192k` | 178.72 ms | 158.65 ms | 128.67 ms | 3.57× | **BREACH** |
| `E5_newline_250k_8f` | 202.50 ms | 165.20 ms | 152.45 ms | 4.05× | **BREACH** |

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

**BREACH: `D1_newline_50k`, `D2_newline_250k`, `E1_newline_64k`, `E2_newline_96k`, `E3_newline_128k`, `E4_newline_192k`, `E5_newline_250k_8f`.** The budget is 100 ms of git latency and is HARD (set by the owner). The corpus was not adjusted in response — the arms are the sizes they were declared at.

**Magnitude and attribution come from different commits.** `git latency` is the median of the timed commits minus the control's; every phase column comes from the arm's one **extra untimed** commit with the timer on. Read the residue as an anatomy of a representative commit beside the magnitude of the median, never as an exact decomposition of one commit.

`predict_id` here is **exclusive** — `redact` subtracted — so the columns are disjoint and may be added.

| arm | bytes read | handed on | ×read | git latency | hook bodies | outside bodies | `redact` | `predict_id` less `redact` | verdict |
|---|---|---|---|---|---|---|---|---|---|
| `D1_newline_50k` | 51200 | 102592 | 2.00× | 101.35 ms | 40.81 ms | 60.54 ms | 28.73 ms | 0.69 ms | **BREACH** |
| `D2_newline_250k` | 256000 | 512960 | 2.00× | 130.98 ms | 91.56 ms | 39.42 ms | 60.92 ms | 4.46 ms | **BREACH** |
| `E1_newline_64k` | 65536 | 131264 | 2.00× | 118.05 ms | 42.48 ms | 75.57 ms | 26.37 ms | 0.98 ms | **BREACH** |
| `E2_newline_96k` | 98304 | 196800 | 2.00× | 110.68 ms | 62.01 ms | 48.67 ms | 39.60 ms | 1.79 ms | **BREACH** |
| `E3_newline_128k` | 131072 | 262336 | 2.00× | 119.53 ms | 49.14 ms | 70.39 ms | 33.52 ms | 1.36 ms | **BREACH** |
| `E4_newline_192k` | 196608 | 393408 | 2.00× | 128.67 ms | 60.31 ms | 68.36 ms | 43.13 ms | 2.22 ms | **BREACH** |
| `E5_newline_250k_8f` | 256000 | 512192 | 2.00× | 152.45 ms | 59.96 ms | 92.50 ms | 42.31 ms | 2.65 ms | **BREACH** |

`outside bodies` is the floor a Rust change cannot move: three `/bin/sh` invocations and three execs that git requires, plus git's own cost of running them. If the budget ever has to halve, that column is what has to be attacked, and it is not in Rust code.

## Oracles — what each arm exercised

Reported for every arm including the control, because a check that only ever confirms is not a check: the idle arm's **absent** trailer is what makes the live arm's present one mean something.

| arm | projections | trailer | verify | events |
|---|---|---|---|---|
| `A_bare` | none | absent | — | — |
| `D1_newline_50k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `D2_newline_250k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 13 checkpoint(s), 14 shard(s) | 60 |
| `E1_newline_64k` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 565248 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `E2_newline_96k` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 565248 B | present | — | 60 |
| `E3_newline_128k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `E4_newline_192k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `E5_newline_250k_8f` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 13 checkpoint(s), 14 shard(s) | 60 |

**More than one projection under a single arm's cache.** That is the shape of the defect this table exists for: the number may be about a store rift never opened.

## Phases

From one extra **untimed** commit per arm with `RIFT_HOOK_TIMING=1`, straight out of the shipped binary. Untimed because the timer takes clock readings the shipped default never takes.

**Read the `exclusive` column, and never sum `recorded`.** The timer records on completion, so a nested phase prints before its parent and the parent's figure already contains it: `predict_id` INCLUDES `redact`. `recorded` is what the binary printed; `exclusive` has every declared child subtracted, so those values are disjoint and may be added. Summing `recorded` would double-count the redaction — the one quantity the opted-in arms exist to separate.

| arm | hook | phase | recorded µs | exclusive µs |
|---|---|---|---|---|
| `D1_newline_50k` | post-commit | discover | 33 | 33 |
| `D1_newline_50k` | post-commit | enqueue | 584 | 584 |
| `D1_newline_50k` | post-commit | gix_open | 797 | 797 |
| `D1_newline_50k` | post-commit | head_hex | 546 | 546 |
| `D1_newline_50k` | post-commit | live_sessions | 703 | 703 |
| `D1_newline_50k` | post-commit | spawn_worker | 1488 | 1488 |
| `D1_newline_50k` | post-commit | total | 4417 | 4417 |
| `D1_newline_50k` | prepare-commit-msg | attribution_preview | 401 | 401 |
| `D1_newline_50k` | prepare-commit-msg | discover | 44 | 44 |
| `D1_newline_50k` | prepare-commit-msg | live_sessions | 963 | 963 |
| `D1_newline_50k` | prepare-commit-msg | open_projection | 2884 | 2884 |
| `D1_newline_50k` | prepare-commit-msg | predict_id | 29417 | 686 (parent, `redact` subtracted) |
| `D1_newline_50k` | prepare-commit-msg | raw_capture | 1662 | 1662 |
| `D1_newline_50k` | prepare-commit-msg | read_facts | 146 | 146 |
| `D1_newline_50k` | prepare-commit-msg | redact | 28731 | 28731 |
| `D1_newline_50k` | prepare-commit-msg | rule_packs | 28 | 28 |
| `D1_newline_50k` | prepare-commit-msg | tokio_build | 25 | 25 |
| `D1_newline_50k` | prepare-commit-msg | total | 36390 | 36390 |
| `D2_newline_250k` | post-commit | discover | 45 | 45 |
| `D2_newline_250k` | post-commit | enqueue | 729 | 729 |
| `D2_newline_250k` | post-commit | gix_open | 1152 | 1152 |
| `D2_newline_250k` | post-commit | head_hex | 588 | 588 |
| `D2_newline_250k` | post-commit | live_sessions | 984 | 984 |
| `D2_newline_250k` | post-commit | spawn_worker | 3065 | 3065 |
| `D2_newline_250k` | post-commit | total | 6908 | 6908 |
| `D2_newline_250k` | prepare-commit-msg | attribution_preview | 570 | 570 |
| `D2_newline_250k` | prepare-commit-msg | discover | 75 | 75 |
| `D2_newline_250k` | prepare-commit-msg | live_sessions | 1246 | 1246 |
| `D2_newline_250k` | prepare-commit-msg | open_projection | 4299 | 4299 |
| `D2_newline_250k` | prepare-commit-msg | predict_id | 65382 | 4457 (parent, `redact` subtracted) |
| `D2_newline_250k` | prepare-commit-msg | raw_capture | 10727 | 10727 |
| `D2_newline_250k` | prepare-commit-msg | read_facts | 148 | 148 |
| `D2_newline_250k` | prepare-commit-msg | redact | 60925 | 60925 |
| `D2_newline_250k` | prepare-commit-msg | rule_packs | 43 | 43 |
| `D2_newline_250k` | prepare-commit-msg | tokio_build | 23 | 23 |
| `D2_newline_250k` | prepare-commit-msg | total | 84653 | 84653 |
| `E1_newline_64k` | post-commit | discover | 37 | 37 |
| `E1_newline_64k` | post-commit | enqueue | 436 | 436 |
| `E1_newline_64k` | post-commit | gix_open | 903 | 903 |
| `E1_newline_64k` | post-commit | head_hex | 459 | 459 |
| `E1_newline_64k` | post-commit | live_sessions | 609 | 609 |
| `E1_newline_64k` | post-commit | spawn_worker | 1328 | 1328 |
| `E1_newline_64k` | post-commit | total | 4090 | 4090 |
| `E1_newline_64k` | prepare-commit-msg | attribution_preview | 1675 | 1675 |
| `E1_newline_64k` | prepare-commit-msg | discover | 58 | 58 |
| `E1_newline_64k` | prepare-commit-msg | live_sessions | 1157 | 1157 |
| `E1_newline_64k` | prepare-commit-msg | open_projection | 4478 | 4478 |
| `E1_newline_64k` | prepare-commit-msg | predict_id | 27346 | 976 (parent, `redact` subtracted) |
| `E1_newline_64k` | prepare-commit-msg | raw_capture | 2319 | 2319 |
| `E1_newline_64k` | prepare-commit-msg | read_facts | 203 | 203 |
| `E1_newline_64k` | prepare-commit-msg | redact | 26370 | 26370 |
| `E1_newline_64k` | prepare-commit-msg | rule_packs | 29 | 29 |
| `E1_newline_64k` | prepare-commit-msg | tokio_build | 30 | 30 |
| `E1_newline_64k` | prepare-commit-msg | total | 38385 | 38385 |
| `E2_newline_96k` | post-commit | discover | 48 | 48 |
| `E2_newline_96k` | post-commit | enqueue | 771 | 771 |
| `E2_newline_96k` | post-commit | gix_open | 1190 | 1190 |
| `E2_newline_96k` | post-commit | head_hex | 730 | 730 |
| `E2_newline_96k` | post-commit | live_sessions | 1179 | 1179 |
| `E2_newline_96k` | post-commit | spawn_worker | 3052 | 3052 |
| `E2_newline_96k` | post-commit | total | 7313 | 7313 |
| `E2_newline_96k` | prepare-commit-msg | attribution_preview | 664 | 664 |
| `E2_newline_96k` | prepare-commit-msg | discover | 62 | 62 |
| `E2_newline_96k` | prepare-commit-msg | live_sessions | 1635 | 1635 |
| `E2_newline_96k` | prepare-commit-msg | open_projection | 4673 | 4673 |
| `E2_newline_96k` | prepare-commit-msg | predict_id | 41386 | 1788 (parent, `redact` subtracted) |
| `E2_newline_96k` | prepare-commit-msg | raw_capture | 4164 | 4164 |
| `E2_newline_96k` | prepare-commit-msg | read_facts | 208 | 208 |
| `E2_newline_96k` | prepare-commit-msg | redact | 39598 | 39598 |
| `E2_newline_96k` | prepare-commit-msg | rule_packs | 47 | 47 |
| `E2_newline_96k` | prepare-commit-msg | tokio_build | 31 | 31 |
| `E2_newline_96k` | prepare-commit-msg | total | 54692 | 54692 |
| `E3_newline_128k` | post-commit | discover | 31 | 31 |
| `E3_newline_128k` | post-commit | enqueue | 465 | 465 |
| `E3_newline_128k` | post-commit | gix_open | 714 | 714 |
| `E3_newline_128k` | post-commit | head_hex | 386 | 386 |
| `E3_newline_128k` | post-commit | live_sessions | 544 | 544 |
| `E3_newline_128k` | post-commit | spawn_worker | 1352 | 1352 |
| `E3_newline_128k` | post-commit | total | 3703 | 3703 |
| `E3_newline_128k` | prepare-commit-msg | attribution_preview | 377 | 377 |
| `E3_newline_128k` | prepare-commit-msg | discover | 42 | 42 |
| `E3_newline_128k` | prepare-commit-msg | live_sessions | 696 | 696 |
| `E3_newline_128k` | prepare-commit-msg | open_projection | 4404 | 4404 |
| `E3_newline_128k` | prepare-commit-msg | predict_id | 34882 | 1360 (parent, `redact` subtracted) |
| `E3_newline_128k` | prepare-commit-msg | raw_capture | 3921 | 3921 |
| `E3_newline_128k` | prepare-commit-msg | read_facts | 177 | 177 |
| `E3_newline_128k` | prepare-commit-msg | redact | 33522 | 33522 |
| `E3_newline_128k` | prepare-commit-msg | rule_packs | 40 | 40 |
| `E3_newline_128k` | prepare-commit-msg | tokio_build | 28 | 28 |
| `E3_newline_128k` | prepare-commit-msg | total | 45441 | 45441 |
| `E4_newline_192k` | post-commit | discover | 35 | 35 |
| `E4_newline_192k` | post-commit | enqueue | 546 | 546 |
| `E4_newline_192k` | post-commit | gix_open | 860 | 860 |
| `E4_newline_192k` | post-commit | head_hex | 466 | 466 |
| `E4_newline_192k` | post-commit | live_sessions | 775 | 775 |
| `E4_newline_192k` | post-commit | spawn_worker | 1483 | 1483 |
| `E4_newline_192k` | post-commit | total | 4411 | 4411 |
| `E4_newline_192k` | prepare-commit-msg | attribution_preview | 431 | 431 |
| `E4_newline_192k` | prepare-commit-msg | discover | 49 | 49 |
| `E4_newline_192k` | prepare-commit-msg | live_sessions | 1097 | 1097 |
| `E4_newline_192k` | prepare-commit-msg | open_projection | 2959 | 2959 |
| `E4_newline_192k` | prepare-commit-msg | predict_id | 45343 | 2217 (parent, `redact` subtracted) |
| `E4_newline_192k` | prepare-commit-msg | raw_capture | 4751 | 4751 |
| `E4_newline_192k` | prepare-commit-msg | read_facts | 142 | 142 |
| `E4_newline_192k` | prepare-commit-msg | redact | 43126 | 43126 |
| `E4_newline_192k` | prepare-commit-msg | rule_packs | 31 | 31 |
| `E4_newline_192k` | prepare-commit-msg | tokio_build | 25 | 25 |
| `E4_newline_192k` | prepare-commit-msg | total | 55900 | 55900 |
| `E5_newline_250k_8f` | post-commit | discover | 37 | 37 |
| `E5_newline_250k_8f` | post-commit | enqueue | 526 | 526 |
| `E5_newline_250k_8f` | post-commit | gix_open | 722 | 722 |
| `E5_newline_250k_8f` | post-commit | head_hex | 479 | 479 |
| `E5_newline_250k_8f` | post-commit | live_sessions | 1076 | 1076 |
| `E5_newline_250k_8f` | post-commit | spawn_worker | 1403 | 1403 |
| `E5_newline_250k_8f` | post-commit | total | 4554 | 4554 |
| `E5_newline_250k_8f` | prepare-commit-msg | attribution_preview | 365 | 365 |
| `E5_newline_250k_8f` | prepare-commit-msg | discover | 35 | 35 |
| `E5_newline_250k_8f` | prepare-commit-msg | live_sessions | 901 | 901 |
| `E5_newline_250k_8f` | prepare-commit-msg | open_projection | 2773 | 2773 |
| `E5_newline_250k_8f` | prepare-commit-msg | predict_id | 44961 | 2648 (parent, `redact` subtracted) |
| `E5_newline_250k_8f` | prepare-commit-msg | raw_capture | 5075 | 5075 |
| `E5_newline_250k_8f` | prepare-commit-msg | read_facts | 126 | 126 |
| `E5_newline_250k_8f` | prepare-commit-msg | redact | 42313 | 42313 |
| `E5_newline_250k_8f` | prepare-commit-msg | rule_packs | 37 | 37 |
| `E5_newline_250k_8f` | prepare-commit-msg | tokio_build | 23 | 23 |
| `E5_newline_250k_8f` | prepare-commit-msg | total | 55402 | 55402 |

### Phase audit

`total` is measured independently of the phases, so disjoint phases can never exceed it. `over-attributed` is therefore a proof that something is nested which the harness does not declare — not a rounding remark.

| arm | hook | total µs | attributed µs | unattributed µs | verdict |
|---|---|---|---|---|---|
| `D1_newline_50k` | post-commit | 4417 | 4151 | 266 | adds up |
| `D1_newline_50k` | prepare-commit-msg | 36390 | 35570 | 820 | adds up |
| `D2_newline_250k` | post-commit | 6908 | 6563 | 345 | adds up |
| `D2_newline_250k` | prepare-commit-msg | 84653 | 82513 | 2140 | adds up |
| `E1_newline_64k` | post-commit | 4090 | 3772 | 318 | adds up |
| `E1_newline_64k` | prepare-commit-msg | 38385 | 37295 | 1090 | adds up |
| `E2_newline_96k` | post-commit | 7313 | 6970 | 343 | adds up |
| `E2_newline_96k` | prepare-commit-msg | 54692 | 52870 | 1822 | adds up |
| `E3_newline_128k` | post-commit | 3703 | 3492 | 211 | adds up |
| `E3_newline_128k` | prepare-commit-msg | 45441 | 44567 | 874 | adds up |
| `E4_newline_192k` | post-commit | 4411 | 4165 | 246 | adds up |
| `E4_newline_192k` | prepare-commit-msg | 55900 | 54828 | 1072 | adds up |
| `E5_newline_250k_8f` | post-commit | 4554 | 4243 | 311 | adds up |
| `E5_newline_250k_8f` | prepare-commit-msg | 55402 | 54296 | 1106 | adds up |
