# Hook latency — git-latency sweep

- harness `0.0.1`, artifact schema 1, generated 1787127988
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
| `A_bare` | 53.68 ms | 48.15 ms | — | 1.00× | control |
| `B0_idle` | 111.22 ms | 101.21 ms | 57.53 ms | 2.07× | meets |
| `B1_live` | 142.24 ms | 128.87 ms | 88.55 ms | 2.65× | meets |
| `C1_include_50k` | 141.03 ms | 125.99 ms | 87.34 ms | 2.63× | meets |
| `C2_include_250k` | 171.26 ms | 142.95 ms | 117.57 ms | 3.19× | **BREACH** |
| `C3_include_1mib` | 218.84 ms | 178.80 ms | 165.16 ms | 4.08× | **BREACH** |
| `D1_newline_50k` | 165.60 ms | 132.24 ms | 111.92 ms | 3.08× | **BREACH** |
| `D2_newline_250k` | 197.85 ms | 175.77 ms | 144.17 ms | 3.69× | **BREACH** |
| `D3_newline_1mib` | 319.13 ms | 261.41 ms | 265.44 ms | 5.94× | **BREACH** |

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

**BREACH: `C2_include_250k`, `C3_include_1mib`, `D1_newline_50k`, `D2_newline_250k`, `D3_newline_1mib`.** The budget is 100 ms of git latency and is HARD (set by the owner). The corpus was not adjusted in response — the arms are the sizes they were declared at.

**Magnitude and attribution come from different commits.** `git latency` is the median of the timed commits minus the control's; every phase column comes from the arm's one **extra untimed** commit with the timer on. Read the residue as an anatomy of a representative commit beside the magnitude of the median, never as an exact decomposition of one commit.

`predict_id` here is **exclusive** — `redact` subtracted — so the columns are disjoint and may be added.

| arm | bytes read | handed on | ×read | git latency | hook bodies | outside bodies | `redact` | `predict_id` less `redact` | verdict |
|---|---|---|---|---|---|---|---|---|---|
| `B0_idle` | — | — | — | 57.53 ms | 4.62 ms | 52.91 ms | 0.00 ms | 0.00 ms | meets |
| `B1_live` | — | — | — | 88.55 ms | 24.59 ms | 63.96 ms | 15.19 ms | 0.06 ms | meets |
| `C1_include_50k` | 51200 | 52696 | 1.03× | 87.34 ms | 29.94 ms | 57.40 ms | 16.68 ms | 0.30 ms | meets |
| `C2_include_250k` | 256000 | 263480 | 1.03× | 117.57 ms | 57.66 ms | 59.91 ms | 37.13 ms | 2.43 ms | **BREACH** |
| `C3_include_1mib` | 1048576 | 1066352 | 1.02× | 165.16 ms | 86.33 ms | 78.83 ms | 65.28 ms | 5.37 ms | **BREACH** |
| `D1_newline_50k` | 51200 | 102592 | 2.00× | 111.92 ms | 40.89 ms | 71.03 ms | 24.73 ms | 0.90 ms | **BREACH** |
| `D2_newline_250k` | 256000 | 512960 | 2.00× | 144.17 ms | 96.89 ms | 47.28 ms | 67.10 ms | 4.90 ms | **BREACH** |
| `D3_newline_1mib` | 1048576 | 2097536 | 2.00× | 265.44 ms | 187.50 ms | 77.94 ms | 138.50 ms | 13.95 ms | **BREACH** |

`outside bodies` is the floor a Rust change cannot move: three `/bin/sh` invocations and three execs that git requires, plus git's own cost of running them. If the budget ever has to halve, that column is what has to be attacked, and it is not in Rust code.

## Oracles — what each arm exercised

Reported for every arm including the control, because a check that only ever confirms is not a check: the idle arm's **absent** trailer is what makes the live arm's present one mean something.

| arm | projections | trailer | verify | events |
|---|---|---|---|---|
| `A_bare` | none | absent | — | — |
| `B0_idle` | rift/<repo-id>/projection.db 430080 B; rift/canopy/ui-state.db 90112 B | absent | 1 checkpoint(s), 0 shard(s) | 0 |
| `B1_live` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `C1_include_50k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `C2_include_250k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | — | 60 |
| `C3_include_1mib` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 12 checkpoint(s), 13 shard(s) | 60 |
| `D1_newline_50k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `D2_newline_250k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 14 shard(s) | 60 |
| `D3_newline_1mib` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 12 checkpoint(s), 12 shard(s) | 60 |

**More than one projection under a single arm's cache.** That is the shape of the defect this table exists for: the number may be about a store rift never opened.

## Phases

From one extra **untimed** commit per arm with `RIFT_HOOK_TIMING=1`, straight out of the shipped binary. Untimed because the timer takes clock readings the shipped default never takes.

**Read the `exclusive` column, and never sum `recorded`.** The timer records on completion, so a nested phase prints before its parent and the parent's figure already contains it: `predict_id` INCLUDES `redact`. `recorded` is what the binary printed; `exclusive` has every declared child subtracted, so those values are disjoint and may be added. Summing `recorded` would double-count the redaction — the one quantity the opted-in arms exist to separate.

| arm | hook | phase | recorded µs | exclusive µs |
|---|---|---|---|---|
| `B0_idle` | post-commit | discover | 40 | 40 |
| `B0_idle` | post-commit | enqueue | 1723 | 1723 |
| `B0_idle` | post-commit | gix_open | 725 | 725 |
| `B0_idle` | post-commit | head_hex | 414 | 414 |
| `B0_idle` | post-commit | live_sessions | 26 | 26 |
| `B0_idle` | post-commit | spawn_worker | 1309 | 1309 |
| `B0_idle` | post-commit | total | 4481 | 4481 |
| `B0_idle` | prepare-commit-msg | discover | 31 | 31 |
| `B0_idle` | prepare-commit-msg | live_sessions | 15 | 15 |
| `B0_idle` | prepare-commit-msg | total | 139 | 139 |
| `B1_live` | post-commit | discover | 34 | 34 |
| `B1_live` | post-commit | enqueue | 545 | 545 |
| `B1_live` | post-commit | gix_open | 827 | 827 |
| `B1_live` | post-commit | head_hex | 435 | 435 |
| `B1_live` | post-commit | live_sessions | 1045 | 1045 |
| `B1_live` | post-commit | spawn_worker | 1480 | 1480 |
| `B1_live` | post-commit | total | 4604 | 4604 |
| `B1_live` | prepare-commit-msg | attribution_preview | 359 | 359 |
| `B1_live` | prepare-commit-msg | discover | 41 | 41 |
| `B1_live` | prepare-commit-msg | live_sessions | 834 | 834 |
| `B1_live` | prepare-commit-msg | open_projection | 2622 | 2622 |
| `B1_live` | prepare-commit-msg | predict_id | 15249 | 64 (parent, `redact` subtracted) |
| `B1_live` | prepare-commit-msg | raw_capture | 25 | 25 |
| `B1_live` | prepare-commit-msg | read_facts | 129 | 129 |
| `B1_live` | prepare-commit-msg | redact | 15185 | 15185 |
| `B1_live` | prepare-commit-msg | rule_packs | 18 | 18 |
| `B1_live` | prepare-commit-msg | tokio_build | 23 | 23 |
| `B1_live` | prepare-commit-msg | total | 19987 | 19987 |
| `C1_include_50k` | post-commit | discover | 31 | 31 |
| `C1_include_50k` | post-commit | enqueue | 551 | 551 |
| `C1_include_50k` | post-commit | gix_open | 750 | 750 |
| `C1_include_50k` | post-commit | head_hex | 424 | 424 |
| `C1_include_50k` | post-commit | live_sessions | 542 | 542 |
| `C1_include_50k` | post-commit | spawn_worker | 2562 | 2562 |
| `C1_include_50k` | post-commit | total | 5121 | 5121 |
| `C1_include_50k` | prepare-commit-msg | attribution_preview | 354 | 354 |
| `C1_include_50k` | prepare-commit-msg | discover | 42 | 42 |
| `C1_include_50k` | prepare-commit-msg | live_sessions | 1056 | 1056 |
| `C1_include_50k` | prepare-commit-msg | open_projection | 4647 | 4647 |
| `C1_include_50k` | prepare-commit-msg | predict_id | 16977 | 302 (parent, `redact` subtracted) |
| `C1_include_50k` | prepare-commit-msg | raw_capture | 781 | 781 |
| `C1_include_50k` | prepare-commit-msg | read_facts | 174 | 174 |
| `C1_include_50k` | prepare-commit-msg | redact | 16675 | 16675 |
| `C1_include_50k` | prepare-commit-msg | rule_packs | 68 | 68 |
| `C1_include_50k` | prepare-commit-msg | tokio_build | 24 | 24 |
| `C1_include_50k` | prepare-commit-msg | total | 24821 | 24821 |
| `C2_include_250k` | post-commit | discover | 33 | 33 |
| `C2_include_250k` | post-commit | enqueue | 869 | 869 |
| `C2_include_250k` | post-commit | gix_open | 1528 | 1528 |
| `C2_include_250k` | post-commit | head_hex | 855 | 855 |
| `C2_include_250k` | post-commit | live_sessions | 1239 | 1239 |
| `C2_include_250k` | post-commit | spawn_worker | 2117 | 2117 |
| `C2_include_250k` | post-commit | total | 6987 | 6987 |
| `C2_include_250k` | prepare-commit-msg | attribution_preview | 500 | 500 |
| `C2_include_250k` | prepare-commit-msg | discover | 66 | 66 |
| `C2_include_250k` | prepare-commit-msg | live_sessions | 1105 | 1105 |
| `C2_include_250k` | prepare-commit-msg | open_projection | 4093 | 4093 |
| `C2_include_250k` | prepare-commit-msg | predict_id | 39564 | 2430 (parent, `redact` subtracted) |
| `C2_include_250k` | prepare-commit-msg | raw_capture | 4049 | 4049 |
| `C2_include_250k` | prepare-commit-msg | read_facts | 205 | 205 |
| `C2_include_250k` | prepare-commit-msg | redact | 37134 | 37134 |
| `C2_include_250k` | prepare-commit-msg | rule_packs | 57 | 57 |
| `C2_include_250k` | prepare-commit-msg | tokio_build | 33 | 33 |
| `C2_include_250k` | prepare-commit-msg | total | 50677 | 50677 |
| `C3_include_1mib` | post-commit | discover | 32 | 32 |
| `C3_include_1mib` | post-commit | enqueue | 693 | 693 |
| `C3_include_1mib` | post-commit | gix_open | 769 | 769 |
| `C3_include_1mib` | post-commit | head_hex | 374 | 374 |
| `C3_include_1mib` | post-commit | live_sessions | 893 | 893 |
| `C3_include_1mib` | post-commit | spawn_worker | 1956 | 1956 |
| `C3_include_1mib` | post-commit | total | 5013 | 5013 |
| `C3_include_1mib` | prepare-commit-msg | attribution_preview | 367 | 367 |
| `C3_include_1mib` | prepare-commit-msg | discover | 44 | 44 |
| `C3_include_1mib` | prepare-commit-msg | live_sessions | 1162 | 1162 |
| `C3_include_1mib` | prepare-commit-msg | open_projection | 4367 | 4367 |
| `C3_include_1mib` | prepare-commit-msg | predict_id | 70650 | 5365 (parent, `redact` subtracted) |
| `C3_include_1mib` | prepare-commit-msg | raw_capture | 2993 | 2993 |
| `C3_include_1mib` | prepare-commit-msg | read_facts | 193 | 193 |
| `C3_include_1mib` | prepare-commit-msg | redact | 65285 | 65285 |
| `C3_include_1mib` | prepare-commit-msg | rule_packs | 33 | 33 |
| `C3_include_1mib` | prepare-commit-msg | tokio_build | 29 | 29 |
| `C3_include_1mib` | prepare-commit-msg | total | 81315 | 81315 |
| `D1_newline_50k` | post-commit | discover | 62 | 62 |
| `D1_newline_50k` | post-commit | enqueue | 449 | 449 |
| `D1_newline_50k` | post-commit | gix_open | 1193 | 1193 |
| `D1_newline_50k` | post-commit | head_hex | 646 | 646 |
| `D1_newline_50k` | post-commit | live_sessions | 637 | 637 |
| `D1_newline_50k` | post-commit | spawn_worker | 1495 | 1495 |
| `D1_newline_50k` | post-commit | total | 4714 | 4714 |
| `D1_newline_50k` | prepare-commit-msg | attribution_preview | 586 | 586 |
| `D1_newline_50k` | prepare-commit-msg | discover | 55 | 55 |
| `D1_newline_50k` | prepare-commit-msg | live_sessions | 1652 | 1652 |
| `D1_newline_50k` | prepare-commit-msg | open_projection | 4347 | 4347 |
| `D1_newline_50k` | prepare-commit-msg | predict_id | 25634 | 901 (parent, `redact` subtracted) |
| `D1_newline_50k` | prepare-commit-msg | raw_capture | 2544 | 2544 |
| `D1_newline_50k` | prepare-commit-msg | read_facts | 184 | 184 |
| `D1_newline_50k` | prepare-commit-msg | redact | 24733 | 24733 |
| `D1_newline_50k` | prepare-commit-msg | rule_packs | 58 | 58 |
| `D1_newline_50k` | prepare-commit-msg | tokio_build | 28 | 28 |
| `D1_newline_50k` | prepare-commit-msg | total | 36177 | 36177 |
| `D2_newline_250k` | post-commit | discover | 92 | 92 |
| `D2_newline_250k` | post-commit | enqueue | 671 | 671 |
| `D2_newline_250k` | post-commit | gix_open | 1561 | 1561 |
| `D2_newline_250k` | post-commit | head_hex | 1136 | 1136 |
| `D2_newline_250k` | post-commit | live_sessions | 1222 | 1222 |
| `D2_newline_250k` | post-commit | spawn_worker | 3647 | 3647 |
| `D2_newline_250k` | post-commit | total | 8680 | 8680 |
| `D2_newline_250k` | prepare-commit-msg | attribution_preview | 819 | 819 |
| `D2_newline_250k` | prepare-commit-msg | discover | 57 | 57 |
| `D2_newline_250k` | prepare-commit-msg | live_sessions | 1156 | 1156 |
| `D2_newline_250k` | prepare-commit-msg | open_projection | 3588 | 3588 |
| `D2_newline_250k` | prepare-commit-msg | predict_id | 72003 | 4901 (parent, `redact` subtracted) |
| `D2_newline_250k` | prepare-commit-msg | raw_capture | 6616 | 6616 |
| `D2_newline_250k` | prepare-commit-msg | read_facts | 142 | 142 |
| `D2_newline_250k` | prepare-commit-msg | redact | 67102 | 67102 |
| `D2_newline_250k` | prepare-commit-msg | rule_packs | 31 | 31 |
| `D2_newline_250k` | prepare-commit-msg | tokio_build | 23 | 23 |
| `D2_newline_250k` | prepare-commit-msg | total | 88211 | 88211 |
| `D3_newline_1mib` | post-commit | discover | 57 | 57 |
| `D3_newline_1mib` | post-commit | enqueue | 1917 | 1917 |
| `D3_newline_1mib` | post-commit | gix_open | 1467 | 1467 |
| `D3_newline_1mib` | post-commit | head_hex | 491 | 491 |
| `D3_newline_1mib` | post-commit | live_sessions | 924 | 924 |
| `D3_newline_1mib` | post-commit | spawn_worker | 1420 | 1420 |
| `D3_newline_1mib` | post-commit | total | 6558 | 6558 |
| `D3_newline_1mib` | prepare-commit-msg | attribution_preview | 549 | 549 |
| `D3_newline_1mib` | prepare-commit-msg | discover | 31 | 31 |
| `D3_newline_1mib` | prepare-commit-msg | live_sessions | 804 | 804 |
| `D3_newline_1mib` | prepare-commit-msg | open_projection | 2730 | 2730 |
| `D3_newline_1mib` | prepare-commit-msg | predict_id | 152451 | 13948 (parent, `redact` subtracted) |
| `D3_newline_1mib` | prepare-commit-msg | raw_capture | 21014 | 21014 |
| `D3_newline_1mib` | prepare-commit-msg | read_facts | 122 | 122 |
| `D3_newline_1mib` | prepare-commit-msg | redact | 138503 | 138503 |
| `D3_newline_1mib` | prepare-commit-msg | rule_packs | 25 | 25 |
| `D3_newline_1mib` | prepare-commit-msg | tokio_build | 23 | 23 |
| `D3_newline_1mib` | prepare-commit-msg | total | 180944 | 180944 |

### Phase audit

`total` is measured independently of the phases, so disjoint phases can never exceed it. `over-attributed` is therefore a proof that something is nested which the harness does not declare — not a rounding remark.

| arm | hook | total µs | attributed µs | unattributed µs | verdict |
|---|---|---|---|---|---|
| `B0_idle` | post-commit | 4481 | 4237 | 244 | adds up |
| `B0_idle` | prepare-commit-msg | 139 | 46 | 93 | adds up |
| `B1_live` | post-commit | 4604 | 4366 | 238 | adds up |
| `B1_live` | prepare-commit-msg | 19987 | 19300 | 687 | adds up |
| `C1_include_50k` | post-commit | 5121 | 4860 | 261 | adds up |
| `C1_include_50k` | prepare-commit-msg | 24821 | 24123 | 698 | adds up |
| `C2_include_250k` | post-commit | 6987 | 6641 | 346 | adds up |
| `C2_include_250k` | prepare-commit-msg | 50677 | 49672 | 1005 | adds up |
| `C3_include_1mib` | post-commit | 5013 | 4717 | 296 | adds up |
| `C3_include_1mib` | prepare-commit-msg | 81315 | 79838 | 1477 | adds up |
| `D1_newline_50k` | post-commit | 4714 | 4482 | 232 | adds up |
| `D1_newline_50k` | prepare-commit-msg | 36177 | 35088 | 1089 | adds up |
| `D2_newline_250k` | post-commit | 8680 | 8329 | 351 | adds up |
| `D2_newline_250k` | prepare-commit-msg | 88211 | 84435 | 3776 | adds up |
| `D3_newline_1mib` | post-commit | 6558 | 6276 | 282 | adds up |
| `D3_newline_1mib` | prepare-commit-msg | 180944 | 177749 | 3195 | adds up |
