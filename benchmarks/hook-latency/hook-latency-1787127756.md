# Hook latency — git-latency sweep

- harness `0.0.1`, artifact schema 1, generated 1787127756
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
| `A_bare` | 62.72 ms | 51.02 ms | — | 1.00× | control |
| `B0_idle` | 123.23 ms | 113.58 ms | 60.51 ms | 1.96× | meets |
| `B1_live` | 149.19 ms | 121.21 ms | 86.48 ms | 2.38× | meets |
| `C1_include_50k` | 142.11 ms | 117.21 ms | 79.39 ms | 2.27× | meets |
| `C2_include_250k` | 189.23 ms | 143.82 ms | 126.52 ms | 3.02× | **BREACH** |
| `C3_include_1mib` | 203.47 ms | 181.51 ms | 140.76 ms | 3.24× | **BREACH** |
| `D1_newline_50k` | 147.74 ms | 135.13 ms | 85.02 ms | 2.36× | meets |
| `D2_newline_250k` | 192.55 ms | 160.72 ms | 129.84 ms | 3.07× | **BREACH** |
| `D3_newline_1mib` | 306.66 ms | 263.54 ms | 243.94 ms | 4.89× | **BREACH** |

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

**BREACH: `C2_include_250k`, `C3_include_1mib`, `D2_newline_250k`, `D3_newline_1mib`.** The budget is 100 ms of git latency and is HARD (set by the owner). The corpus was not adjusted in response — the arms are the sizes they were declared at.

**Magnitude and attribution come from different commits.** `git latency` is the median of the timed commits minus the control's; every phase column comes from the arm's one **extra untimed** commit with the timer on. Read the residue as an anatomy of a representative commit beside the magnitude of the median, never as an exact decomposition of one commit.

`predict_id` here is **exclusive** — `redact` subtracted — so the columns are disjoint and may be added.

| arm | bytes read | handed on | ×read | git latency | hook bodies | outside bodies | `redact` | `predict_id` less `redact` | verdict |
|---|---|---|---|---|---|---|---|---|---|
| `B0_idle` | — | — | — | 60.51 ms | 3.38 ms | 57.14 ms | 0.00 ms | 0.00 ms | meets |
| `B1_live` | — | — | — | 86.47 ms | 26.94 ms | 59.53 ms | 15.95 ms | 0.06 ms | meets |
| `C1_include_50k` | 51200 | 52696 | 1.03× | 79.39 ms | 29.60 ms | 49.79 ms | 19.05 ms | 0.44 ms | meets |
| `C2_include_250k` | 256000 | 263480 | 1.03× | 126.52 ms | 46.08 ms | 80.43 ms | 31.69 ms | 1.35 ms | **BREACH** |
| `C3_include_1mib` | 1048576 | 1066352 | 1.02× | 140.76 ms | 76.28 ms | 64.48 ms | 57.76 ms | 5.24 ms | **BREACH** |
| `D1_newline_50k` | 51200 | 102592 | 2.00× | 85.02 ms | 35.59 ms | 49.43 ms | 21.96 ms | 0.85 ms | meets |
| `D2_newline_250k` | 256000 | 512960 | 2.00× | 129.84 ms | 54.19 ms | 75.64 ms | 35.60 ms | 2.79 ms | **BREACH** |
| `D3_newline_1mib` | 1048576 | 2097536 | 2.00× | 243.94 ms | 211.65 ms | 32.28 ms | 154.51 ms | 16.73 ms | **BREACH** |

`outside bodies` is the floor a Rust change cannot move: three `/bin/sh` invocations and three execs that git requires, plus git's own cost of running them. If the budget ever has to halve, that column is what has to be attacked, and it is not in Rust code.

## Oracles — what each arm exercised

Reported for every arm including the control, because a check that only ever confirms is not a check: the idle arm's **absent** trailer is what makes the live arm's present one mean something.

| arm | projections | trailer | verify | events |
|---|---|---|---|---|
| `A_bare` | none | absent | — | — |
| `B0_idle` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 430080 B | absent | 1 checkpoint(s), 0 shard(s) | 0 |
| `B1_live` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `C1_include_50k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | — | 60 |
| `C2_include_250k` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 565248 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `C3_include_1mib` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 12 checkpoint(s), 13 shard(s) | 60 |
| `D1_newline_50k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `D2_newline_250k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 13 checkpoint(s), 14 shard(s) | 60 |
| `D3_newline_1mib` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 11 checkpoint(s), 11 shard(s) | 60 |

**More than one projection under a single arm's cache.** That is the shape of the defect this table exists for: the number may be about a store rift never opened.

## Phases

From one extra **untimed** commit per arm with `RIFT_HOOK_TIMING=1`, straight out of the shipped binary. Untimed because the timer takes clock readings the shipped default never takes.

**Read the `exclusive` column, and never sum `recorded`.** The timer records on completion, so a nested phase prints before its parent and the parent's figure already contains it: `predict_id` INCLUDES `redact`. `recorded` is what the binary printed; `exclusive` has every declared child subtracted, so those values are disjoint and may be added. Summing `recorded` would double-count the redaction — the one quantity the opted-in arms exist to separate.

| arm | hook | phase | recorded µs | exclusive µs |
|---|---|---|---|---|
| `B0_idle` | post-commit | discover | 33 | 33 |
| `B0_idle` | post-commit | enqueue | 467 | 467 |
| `B0_idle` | post-commit | gix_open | 713 | 713 |
| `B0_idle` | post-commit | head_hex | 393 | 393 |
| `B0_idle` | post-commit | live_sessions | 18 | 18 |
| `B0_idle` | post-commit | spawn_worker | 1339 | 1339 |
| `B0_idle` | post-commit | total | 3168 | 3168 |
| `B0_idle` | prepare-commit-msg | discover | 65 | 65 |
| `B0_idle` | prepare-commit-msg | live_sessions | 24 | 24 |
| `B0_idle` | prepare-commit-msg | total | 210 | 210 |
| `B1_live` | post-commit | discover | 30 | 30 |
| `B1_live` | post-commit | enqueue | 445 | 445 |
| `B1_live` | post-commit | gix_open | 683 | 683 |
| `B1_live` | post-commit | head_hex | 402 | 402 |
| `B1_live` | post-commit | live_sessions | 563 | 563 |
| `B1_live` | post-commit | spawn_worker | 1426 | 1426 |
| `B1_live` | post-commit | total | 3780 | 3780 |
| `B1_live` | prepare-commit-msg | attribution_preview | 371 | 371 |
| `B1_live` | prepare-commit-msg | discover | 31 | 31 |
| `B1_live` | prepare-commit-msg | live_sessions | 972 | 972 |
| `B1_live` | prepare-commit-msg | open_projection | 4847 | 4847 |
| `B1_live` | prepare-commit-msg | predict_id | 16003 | 58 (parent, `redact` subtracted) |
| `B1_live` | prepare-commit-msg | raw_capture | 29 | 29 |
| `B1_live` | prepare-commit-msg | read_facts | 167 | 167 |
| `B1_live` | prepare-commit-msg | redact | 15945 | 15945 |
| `B1_live` | prepare-commit-msg | rule_packs | 21 | 21 |
| `B1_live` | prepare-commit-msg | tokio_build | 35 | 35 |
| `B1_live` | prepare-commit-msg | total | 23160 | 23160 |
| `C1_include_50k` | post-commit | discover | 30 | 30 |
| `C1_include_50k` | post-commit | enqueue | 520 | 520 |
| `C1_include_50k` | post-commit | gix_open | 1169 | 1169 |
| `C1_include_50k` | post-commit | head_hex | 506 | 506 |
| `C1_include_50k` | post-commit | live_sessions | 710 | 710 |
| `C1_include_50k` | post-commit | spawn_worker | 1283 | 1283 |
| `C1_include_50k` | post-commit | total | 4500 | 4500 |
| `C1_include_50k` | prepare-commit-msg | attribution_preview | 509 | 509 |
| `C1_include_50k` | prepare-commit-msg | discover | 30 | 30 |
| `C1_include_50k` | prepare-commit-msg | live_sessions | 885 | 885 |
| `C1_include_50k` | prepare-commit-msg | open_projection | 2633 | 2633 |
| `C1_include_50k` | prepare-commit-msg | predict_id | 19490 | 438 (parent, `redact` subtracted) |
| `C1_include_50k` | prepare-commit-msg | raw_capture | 720 | 720 |
| `C1_include_50k` | prepare-commit-msg | read_facts | 121 | 121 |
| `C1_include_50k` | prepare-commit-msg | redact | 19052 | 19052 |
| `C1_include_50k` | prepare-commit-msg | rule_packs | 18 | 18 |
| `C1_include_50k` | prepare-commit-msg | tokio_build | 24 | 24 |
| `C1_include_50k` | prepare-commit-msg | total | 25102 | 25102 |
| `C2_include_250k` | post-commit | discover | 33 | 33 |
| `C2_include_250k` | post-commit | enqueue | 486 | 486 |
| `C2_include_250k` | post-commit | gix_open | 872 | 872 |
| `C2_include_250k` | post-commit | head_hex | 613 | 613 |
| `C2_include_250k` | post-commit | live_sessions | 574 | 574 |
| `C2_include_250k` | post-commit | spawn_worker | 1402 | 1402 |
| `C2_include_250k` | post-commit | total | 4207 | 4207 |
| `C2_include_250k` | prepare-commit-msg | attribution_preview | 522 | 522 |
| `C2_include_250k` | prepare-commit-msg | discover | 41 | 41 |
| `C2_include_250k` | prepare-commit-msg | live_sessions | 1165 | 1165 |
| `C2_include_250k` | prepare-commit-msg | open_projection | 2884 | 2884 |
| `C2_include_250k` | prepare-commit-msg | predict_id | 33035 | 1349 (parent, `redact` subtracted) |
| `C2_include_250k` | prepare-commit-msg | raw_capture | 2918 | 2918 |
| `C2_include_250k` | prepare-commit-msg | read_facts | 143 | 143 |
| `C2_include_250k` | prepare-commit-msg | redact | 31686 | 31686 |
| `C2_include_250k` | prepare-commit-msg | rule_packs | 26 | 26 |
| `C2_include_250k` | prepare-commit-msg | tokio_build | 24 | 24 |
| `C2_include_250k` | prepare-commit-msg | total | 41874 | 41874 |
| `C3_include_1mib` | post-commit | discover | 34 | 34 |
| `C3_include_1mib` | post-commit | enqueue | 592 | 592 |
| `C3_include_1mib` | post-commit | gix_open | 1158 | 1158 |
| `C3_include_1mib` | post-commit | head_hex | 437 | 437 |
| `C3_include_1mib` | post-commit | live_sessions | 749 | 749 |
| `C3_include_1mib` | post-commit | spawn_worker | 1750 | 1750 |
| `C3_include_1mib` | post-commit | total | 4989 | 4989 |
| `C3_include_1mib` | prepare-commit-msg | attribution_preview | 396 | 396 |
| `C3_include_1mib` | prepare-commit-msg | discover | 30 | 30 |
| `C3_include_1mib` | prepare-commit-msg | live_sessions | 760 | 760 |
| `C3_include_1mib` | prepare-commit-msg | open_projection | 2460 | 2460 |
| `C3_include_1mib` | prepare-commit-msg | predict_id | 63007 | 5244 (parent, `redact` subtracted) |
| `C3_include_1mib` | prepare-commit-msg | raw_capture | 2786 | 2786 |
| `C3_include_1mib` | prepare-commit-msg | read_facts | 122 | 122 |
| `C3_include_1mib` | prepare-commit-msg | redact | 57763 | 57763 |
| `C3_include_1mib` | prepare-commit-msg | rule_packs | 24 | 24 |
| `C3_include_1mib` | prepare-commit-msg | tokio_build | 21 | 21 |
| `C3_include_1mib` | prepare-commit-msg | total | 71290 | 71290 |
| `D1_newline_50k` | post-commit | discover | 36 | 36 |
| `D1_newline_50k` | post-commit | enqueue | 636 | 636 |
| `D1_newline_50k` | post-commit | gix_open | 1220 | 1220 |
| `D1_newline_50k` | post-commit | head_hex | 616 | 616 |
| `D1_newline_50k` | post-commit | live_sessions | 861 | 861 |
| `D1_newline_50k` | post-commit | spawn_worker | 1762 | 1762 |
| `D1_newline_50k` | post-commit | total | 5431 | 5431 |
| `D1_newline_50k` | prepare-commit-msg | attribution_preview | 609 | 609 |
| `D1_newline_50k` | prepare-commit-msg | discover | 33 | 33 |
| `D1_newline_50k` | prepare-commit-msg | live_sessions | 945 | 945 |
| `D1_newline_50k` | prepare-commit-msg | open_projection | 2999 | 2999 |
| `D1_newline_50k` | prepare-commit-msg | predict_id | 22802 | 846 (parent, `redact` subtracted) |
| `D1_newline_50k` | prepare-commit-msg | raw_capture | 1454 | 1454 |
| `D1_newline_50k` | prepare-commit-msg | read_facts | 178 | 178 |
| `D1_newline_50k` | prepare-commit-msg | redact | 21956 | 21956 |
| `D1_newline_50k` | prepare-commit-msg | rule_packs | 26 | 26 |
| `D1_newline_50k` | prepare-commit-msg | tokio_build | 23 | 23 |
| `D1_newline_50k` | prepare-commit-msg | total | 30157 | 30157 |
| `D2_newline_250k` | post-commit | discover | 33 | 33 |
| `D2_newline_250k` | post-commit | enqueue | 444 | 444 |
| `D2_newline_250k` | post-commit | gix_open | 852 | 852 |
| `D2_newline_250k` | post-commit | head_hex | 483 | 483 |
| `D2_newline_250k` | post-commit | live_sessions | 623 | 623 |
| `D2_newline_250k` | post-commit | spawn_worker | 1349 | 1349 |
| `D2_newline_250k` | post-commit | total | 4123 | 4123 |
| `D2_newline_250k` | prepare-commit-msg | attribution_preview | 366 | 366 |
| `D2_newline_250k` | prepare-commit-msg | discover | 32 | 32 |
| `D2_newline_250k` | prepare-commit-msg | live_sessions | 816 | 816 |
| `D2_newline_250k` | prepare-commit-msg | open_projection | 2855 | 2855 |
| `D2_newline_250k` | prepare-commit-msg | predict_id | 38394 | 2793 (parent, `redact` subtracted) |
| `D2_newline_250k` | prepare-commit-msg | raw_capture | 6418 | 6418 |
| `D2_newline_250k` | prepare-commit-msg | read_facts | 144 | 144 |
| `D2_newline_250k` | prepare-commit-msg | redact | 35601 | 35601 |
| `D2_newline_250k` | prepare-commit-msg | rule_packs | 20 | 20 |
| `D2_newline_250k` | prepare-commit-msg | tokio_build | 21 | 21 |
| `D2_newline_250k` | prepare-commit-msg | total | 50070 | 50070 |
| `D3_newline_1mib` | post-commit | discover | 42 | 42 |
| `D3_newline_1mib` | post-commit | enqueue | 630 | 630 |
| `D3_newline_1mib` | post-commit | gix_open | 1083 | 1083 |
| `D3_newline_1mib` | post-commit | head_hex | 552 | 552 |
| `D3_newline_1mib` | post-commit | live_sessions | 870 | 870 |
| `D3_newline_1mib` | post-commit | spawn_worker | 2790 | 2790 |
| `D3_newline_1mib` | post-commit | total | 6252 | 6252 |
| `D3_newline_1mib` | prepare-commit-msg | attribution_preview | 560 | 560 |
| `D3_newline_1mib` | prepare-commit-msg | discover | 31 | 31 |
| `D3_newline_1mib` | prepare-commit-msg | live_sessions | 696 | 696 |
| `D3_newline_1mib` | prepare-commit-msg | open_projection | 2437 | 2437 |
| `D3_newline_1mib` | prepare-commit-msg | predict_id | 171248 | 16735 (parent, `redact` subtracted) |
| `D3_newline_1mib` | prepare-commit-msg | raw_capture | 27348 | 27348 |
| `D3_newline_1mib` | prepare-commit-msg | read_facts | 134 | 134 |
| `D3_newline_1mib` | prepare-commit-msg | redact | 154513 | 154513 |
| `D3_newline_1mib` | prepare-commit-msg | rule_packs | 50 | 50 |
| `D3_newline_1mib` | prepare-commit-msg | tokio_build | 21 | 21 |
| `D3_newline_1mib` | prepare-commit-msg | total | 205402 | 205402 |

### Phase audit

`total` is measured independently of the phases, so disjoint phases can never exceed it. `over-attributed` is therefore a proof that something is nested which the harness does not declare — not a rounding remark.

| arm | hook | total µs | attributed µs | unattributed µs | verdict |
|---|---|---|---|---|---|
| `B0_idle` | post-commit | 3168 | 2963 | 205 | adds up |
| `B0_idle` | prepare-commit-msg | 210 | 89 | 121 | adds up |
| `B1_live` | post-commit | 3780 | 3549 | 231 | adds up |
| `B1_live` | prepare-commit-msg | 23160 | 22476 | 684 | adds up |
| `C1_include_50k` | post-commit | 4500 | 4218 | 282 | adds up |
| `C1_include_50k` | prepare-commit-msg | 25102 | 24430 | 672 | adds up |
| `C2_include_250k` | post-commit | 4207 | 3980 | 227 | adds up |
| `C2_include_250k` | prepare-commit-msg | 41874 | 40758 | 1116 | adds up |
| `C3_include_1mib` | post-commit | 4989 | 4720 | 269 | adds up |
| `C3_include_1mib` | prepare-commit-msg | 71290 | 69606 | 1684 | adds up |
| `D1_newline_50k` | post-commit | 5431 | 5131 | 300 | adds up |
| `D1_newline_50k` | prepare-commit-msg | 30157 | 29069 | 1088 | adds up |
| `D2_newline_250k` | post-commit | 4123 | 3784 | 339 | adds up |
| `D2_newline_250k` | prepare-commit-msg | 50070 | 49066 | 1004 | adds up |
| `D3_newline_1mib` | post-commit | 6252 | 5967 | 285 | adds up |
| `D3_newline_1mib` | prepare-commit-msg | 205402 | 202525 | 2877 | adds up |
