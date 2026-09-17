# Hook latency — git-latency sweep

- harness `0.0.1`, artifact schema 1, generated 1787127877
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
| `A_bare` | 50.19 ms | 39.27 ms | — | 1.00× | control |
| `B0_idle` | 106.69 ms | 100.92 ms | 56.49 ms | 2.13× | meets |
| `B1_live` | 140.10 ms | 125.69 ms | 89.91 ms | 2.79× | meets |
| `C1_include_50k` | 131.79 ms | 119.51 ms | 81.60 ms | 2.63× | meets |
| `C2_include_250k` | 166.61 ms | 140.97 ms | 116.41 ms | 3.32× | **BREACH** |
| `C3_include_1mib` | 217.92 ms | 190.18 ms | 167.73 ms | 4.34× | **BREACH** |
| `D1_newline_50k` | 144.38 ms | 134.93 ms | 94.19 ms | 2.88× | meets |
| `D2_newline_250k` | 184.64 ms | 163.37 ms | 134.45 ms | 3.68× | **BREACH** |
| `D3_newline_1mib` | 297.81 ms | 272.64 ms | 247.62 ms | 5.93× | **BREACH** |

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
| `B0_idle` | — | — | — | 56.49 ms | 3.31 ms | 53.18 ms | 0.00 ms | 0.00 ms | meets |
| `B1_live` | — | — | — | 89.91 ms | 30.16 ms | 59.74 ms | 19.18 ms | 0.06 ms | meets |
| `C1_include_50k` | 51200 | 52696 | 1.03× | 81.60 ms | 25.19 ms | 56.40 ms | 16.23 ms | 0.29 ms | meets |
| `C2_include_250k` | 256000 | 263480 | 1.03× | 116.41 ms | 57.25 ms | 59.17 ms | 35.67 ms | 2.19 ms | **BREACH** |
| `C3_include_1mib` | 1048576 | 1066352 | 1.02× | 167.73 ms | 107.17 ms | 60.56 ms | 80.13 ms | 8.97 ms | **BREACH** |
| `D1_newline_50k` | 51200 | 102592 | 2.00× | 94.19 ms | 36.64 ms | 57.55 ms | 23.75 ms | 0.83 ms | meets |
| `D2_newline_250k` | 256000 | 512960 | 2.00× | 134.45 ms | 86.80 ms | 47.65 ms | 56.66 ms | 3.96 ms | **BREACH** |
| `D3_newline_1mib` | 1048576 | 2097536 | 2.00× | 247.62 ms | 181.78 ms | 65.84 ms | 127.59 ms | 17.87 ms | **BREACH** |

`outside bodies` is the floor a Rust change cannot move: three `/bin/sh` invocations and three execs that git requires, plus git's own cost of running them. If the budget ever has to halve, that column is what has to be attacked, and it is not in Rust code.

## Oracles — what each arm exercised

Reported for every arm including the control, because a check that only ever confirms is not a check: the idle arm's **absent** trailer is what makes the live arm's present one mean something.

| arm | projections | trailer | verify | events |
|---|---|---|---|---|
| `A_bare` | none | absent | — | — |
| `B0_idle` | rift/<repo-id>/projection.db 430080 B; rift/canopy/ui-state.db 90112 B | absent | 1 checkpoint(s), 0 shard(s) | 0 |
| `B1_live` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 565248 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `C1_include_50k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `C2_include_250k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
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
| `B0_idle` | post-commit | discover | 31 | 31 |
| `B0_idle` | post-commit | enqueue | 443 | 443 |
| `B0_idle` | post-commit | gix_open | 750 | 750 |
| `B0_idle` | post-commit | head_hex | 399 | 399 |
| `B0_idle` | post-commit | live_sessions | 21 | 21 |
| `B0_idle` | post-commit | spawn_worker | 1261 | 1261 |
| `B0_idle` | post-commit | total | 3131 | 3131 |
| `B0_idle` | prepare-commit-msg | discover | 41 | 41 |
| `B0_idle` | prepare-commit-msg | live_sessions | 22 | 22 |
| `B0_idle` | prepare-commit-msg | total | 181 | 181 |
| `B1_live` | post-commit | discover | 41 | 41 |
| `B1_live` | post-commit | enqueue | 502 | 502 |
| `B1_live` | post-commit | gix_open | 796 | 796 |
| `B1_live` | post-commit | head_hex | 422 | 422 |
| `B1_live` | post-commit | live_sessions | 680 | 680 |
| `B1_live` | post-commit | spawn_worker | 1305 | 1305 |
| `B1_live` | post-commit | total | 3963 | 3963 |
| `B1_live` | prepare-commit-msg | attribution_preview | 400 | 400 |
| `B1_live` | prepare-commit-msg | discover | 47 | 47 |
| `B1_live` | prepare-commit-msg | live_sessions | 1014 | 1014 |
| `B1_live` | prepare-commit-msg | open_projection | 4432 | 4432 |
| `B1_live` | prepare-commit-msg | predict_id | 19244 | 62 (parent, `redact` subtracted) |
| `B1_live` | prepare-commit-msg | raw_capture | 30 | 30 |
| `B1_live` | prepare-commit-msg | read_facts | 199 | 199 |
| `B1_live` | prepare-commit-msg | redact | 19182 | 19182 |
| `B1_live` | prepare-commit-msg | rule_packs | 17 | 17 |
| `B1_live` | prepare-commit-msg | tokio_build | 30 | 30 |
| `B1_live` | prepare-commit-msg | total | 26198 | 26198 |
| `C1_include_50k` | post-commit | discover | 29 | 29 |
| `C1_include_50k` | post-commit | enqueue | 429 | 429 |
| `C1_include_50k` | post-commit | gix_open | 828 | 828 |
| `C1_include_50k` | post-commit | head_hex | 468 | 468 |
| `C1_include_50k` | post-commit | live_sessions | 559 | 559 |
| `C1_include_50k` | post-commit | spawn_worker | 1238 | 1238 |
| `C1_include_50k` | post-commit | total | 3756 | 3756 |
| `C1_include_50k` | prepare-commit-msg | attribution_preview | 351 | 351 |
| `C1_include_50k` | prepare-commit-msg | discover | 34 | 34 |
| `C1_include_50k` | prepare-commit-msg | live_sessions | 600 | 600 |
| `C1_include_50k` | prepare-commit-msg | open_projection | 2493 | 2493 |
| `C1_include_50k` | prepare-commit-msg | predict_id | 16519 | 294 (parent, `redact` subtracted) |
| `C1_include_50k` | prepare-commit-msg | raw_capture | 502 | 502 |
| `C1_include_50k` | prepare-commit-msg | read_facts | 115 | 115 |
| `C1_include_50k` | prepare-commit-msg | redact | 16225 | 16225 |
| `C1_include_50k` | prepare-commit-msg | rule_packs | 16 | 16 |
| `C1_include_50k` | prepare-commit-msg | tokio_build | 20 | 20 |
| `C1_include_50k` | prepare-commit-msg | total | 21438 | 21438 |
| `C2_include_250k` | post-commit | discover | 56 | 56 |
| `C2_include_250k` | post-commit | enqueue | 685 | 685 |
| `C2_include_250k` | post-commit | gix_open | 1192 | 1192 |
| `C2_include_250k` | post-commit | head_hex | 680 | 680 |
| `C2_include_250k` | post-commit | live_sessions | 1040 | 1040 |
| `C2_include_250k` | post-commit | spawn_worker | 1474 | 1474 |
| `C2_include_250k` | post-commit | total | 5449 | 5449 |
| `C2_include_250k` | prepare-commit-msg | attribution_preview | 620 | 620 |
| `C2_include_250k` | prepare-commit-msg | discover | 48 | 48 |
| `C2_include_250k` | prepare-commit-msg | live_sessions | 1533 | 1533 |
| `C2_include_250k` | prepare-commit-msg | open_projection | 4730 | 4730 |
| `C2_include_250k` | prepare-commit-msg | predict_id | 37857 | 2188 (parent, `redact` subtracted) |
| `C2_include_250k` | prepare-commit-msg | raw_capture | 5356 | 5356 |
| `C2_include_250k` | prepare-commit-msg | read_facts | 219 | 219 |
| `C2_include_250k` | prepare-commit-msg | redact | 35669 | 35669 |
| `C2_include_250k` | prepare-commit-msg | rule_packs | 41 | 41 |
| `C2_include_250k` | prepare-commit-msg | tokio_build | 31 | 31 |
| `C2_include_250k` | prepare-commit-msg | total | 51797 | 51797 |
| `C3_include_1mib` | post-commit | discover | 32 | 32 |
| `C3_include_1mib` | post-commit | enqueue | 488 | 488 |
| `C3_include_1mib` | post-commit | gix_open | 825 | 825 |
| `C3_include_1mib` | post-commit | head_hex | 385 | 385 |
| `C3_include_1mib` | post-commit | live_sessions | 595 | 595 |
| `C3_include_1mib` | post-commit | spawn_worker | 1393 | 1393 |
| `C3_include_1mib` | post-commit | total | 3969 | 3969 |
| `C3_include_1mib` | prepare-commit-msg | attribution_preview | 548 | 548 |
| `C3_include_1mib` | prepare-commit-msg | discover | 54 | 54 |
| `C3_include_1mib` | prepare-commit-msg | live_sessions | 1470 | 1470 |
| `C3_include_1mib` | prepare-commit-msg | open_projection | 4311 | 4311 |
| `C3_include_1mib` | prepare-commit-msg | predict_id | 89109 | 8975 (parent, `redact` subtracted) |
| `C3_include_1mib` | prepare-commit-msg | raw_capture | 5414 | 5414 |
| `C3_include_1mib` | prepare-commit-msg | read_facts | 191 | 191 |
| `C3_include_1mib` | prepare-commit-msg | redact | 80134 | 80134 |
| `C3_include_1mib` | prepare-commit-msg | rule_packs | 70 | 70 |
| `C3_include_1mib` | prepare-commit-msg | tokio_build | 29 | 29 |
| `C3_include_1mib` | prepare-commit-msg | total | 103199 | 103199 |
| `D1_newline_50k` | post-commit | discover | 29 | 29 |
| `D1_newline_50k` | post-commit | enqueue | 441 | 441 |
| `D1_newline_50k` | post-commit | gix_open | 725 | 725 |
| `D1_newline_50k` | post-commit | head_hex | 363 | 363 |
| `D1_newline_50k` | post-commit | live_sessions | 600 | 600 |
| `D1_newline_50k` | post-commit | spawn_worker | 1326 | 1326 |
| `D1_newline_50k` | post-commit | total | 3701 | 3701 |
| `D1_newline_50k` | prepare-commit-msg | attribution_preview | 428 | 428 |
| `D1_newline_50k` | prepare-commit-msg | discover | 32 | 32 |
| `D1_newline_50k` | prepare-commit-msg | live_sessions | 1603 | 1603 |
| `D1_newline_50k` | prepare-commit-msg | open_projection | 3830 | 3830 |
| `D1_newline_50k` | prepare-commit-msg | predict_id | 24581 | 826 (parent, `redact` subtracted) |
| `D1_newline_50k` | prepare-commit-msg | raw_capture | 1487 | 1487 |
| `D1_newline_50k` | prepare-commit-msg | read_facts | 181 | 181 |
| `D1_newline_50k` | prepare-commit-msg | redact | 23755 | 23755 |
| `D1_newline_50k` | prepare-commit-msg | rule_packs | 26 | 26 |
| `D1_newline_50k` | prepare-commit-msg | tokio_build | 31 | 31 |
| `D1_newline_50k` | prepare-commit-msg | total | 32941 | 32941 |
| `D2_newline_250k` | post-commit | discover | 47 | 47 |
| `D2_newline_250k` | post-commit | enqueue | 631 | 631 |
| `D2_newline_250k` | post-commit | gix_open | 1238 | 1238 |
| `D2_newline_250k` | post-commit | head_hex | 677 | 677 |
| `D2_newline_250k` | post-commit | live_sessions | 974 | 974 |
| `D2_newline_250k` | post-commit | spawn_worker | 1971 | 1971 |
| `D2_newline_250k` | post-commit | total | 5855 | 5855 |
| `D2_newline_250k` | prepare-commit-msg | attribution_preview | 391 | 391 |
| `D2_newline_250k` | prepare-commit-msg | discover | 58 | 58 |
| `D2_newline_250k` | prepare-commit-msg | live_sessions | 1492 | 1492 |
| `D2_newline_250k` | prepare-commit-msg | open_projection | 4187 | 4187 |
| `D2_newline_250k` | prepare-commit-msg | predict_id | 60615 | 3959 (parent, `redact` subtracted) |
| `D2_newline_250k` | prepare-commit-msg | raw_capture | 11607 | 11607 |
| `D2_newline_250k` | prepare-commit-msg | read_facts | 175 | 175 |
| `D2_newline_250k` | prepare-commit-msg | redact | 56656 | 56656 |
| `D2_newline_250k` | prepare-commit-msg | rule_packs | 42 | 42 |
| `D2_newline_250k` | prepare-commit-msg | tokio_build | 24 | 24 |
| `D2_newline_250k` | prepare-commit-msg | total | 80944 | 80944 |
| `D3_newline_1mib` | post-commit | discover | 31 | 31 |
| `D3_newline_1mib` | post-commit | enqueue | 710 | 710 |
| `D3_newline_1mib` | post-commit | gix_open | 845 | 845 |
| `D3_newline_1mib` | post-commit | head_hex | 415 | 415 |
| `D3_newline_1mib` | post-commit | live_sessions | 729 | 729 |
| `D3_newline_1mib` | post-commit | spawn_worker | 1695 | 1695 |
| `D3_newline_1mib` | post-commit | total | 4743 | 4743 |
| `D3_newline_1mib` | prepare-commit-msg | attribution_preview | 540 | 540 |
| `D3_newline_1mib` | prepare-commit-msg | discover | 60 | 60 |
| `D3_newline_1mib` | prepare-commit-msg | live_sessions | 1074 | 1074 |
| `D3_newline_1mib` | prepare-commit-msg | open_projection | 4320 | 4320 |
| `D3_newline_1mib` | prepare-commit-msg | predict_id | 145455 | 17866 (parent, `redact` subtracted) |
| `D3_newline_1mib` | prepare-commit-msg | raw_capture | 22491 | 22491 |
| `D3_newline_1mib` | prepare-commit-msg | read_facts | 171 | 171 |
| `D3_newline_1mib` | prepare-commit-msg | redact | 127589 | 127589 |
| `D3_newline_1mib` | prepare-commit-msg | rule_packs | 25 | 25 |
| `D3_newline_1mib` | prepare-commit-msg | tokio_build | 28 | 28 |
| `D3_newline_1mib` | prepare-commit-msg | total | 177034 | 177034 |

### Phase audit

`total` is measured independently of the phases, so disjoint phases can never exceed it. `over-attributed` is therefore a proof that something is nested which the harness does not declare — not a rounding remark.

| arm | hook | total µs | attributed µs | unattributed µs | verdict |
|---|---|---|---|---|---|
| `B0_idle` | post-commit | 3131 | 2905 | 226 | adds up |
| `B0_idle` | prepare-commit-msg | 181 | 63 | 118 | adds up |
| `B1_live` | post-commit | 3963 | 3746 | 217 | adds up |
| `B1_live` | prepare-commit-msg | 26198 | 25413 | 785 | adds up |
| `C1_include_50k` | post-commit | 3756 | 3551 | 205 | adds up |
| `C1_include_50k` | prepare-commit-msg | 21438 | 20650 | 788 | adds up |
| `C2_include_250k` | post-commit | 5449 | 5127 | 322 | adds up |
| `C2_include_250k` | prepare-commit-msg | 51797 | 50435 | 1362 | adds up |
| `C3_include_1mib` | post-commit | 3969 | 3718 | 251 | adds up |
| `C3_include_1mib` | prepare-commit-msg | 103199 | 101196 | 2003 | adds up |
| `D1_newline_50k` | post-commit | 3701 | 3484 | 217 | adds up |
| `D1_newline_50k` | prepare-commit-msg | 32941 | 32199 | 742 | adds up |
| `D2_newline_250k` | post-commit | 5855 | 5538 | 317 | adds up |
| `D2_newline_250k` | prepare-commit-msg | 80944 | 78591 | 2353 | adds up |
| `D3_newline_1mib` | post-commit | 4743 | 4425 | 318 | adds up |
| `D3_newline_1mib` | prepare-commit-msg | 177034 | 174164 | 2870 | adds up |
