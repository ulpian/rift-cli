# Hook latency — git-latency sweep

- harness `0.0.1`, artifact schema 1, generated 1787128771
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
| `A_bare` | 52.22 ms | 49.53 ms | — | 1.00× | control |
| `D1_newline_50k` | 169.94 ms | 149.88 ms | 117.72 ms | 3.25× | **BREACH** |
| `D2_newline_250k` | 177.54 ms | 154.59 ms | 125.32 ms | 3.40× | **BREACH** |
| `E1_newline_64k` | 174.41 ms | 162.74 ms | 122.18 ms | 3.34× | **BREACH** |
| `E2_newline_96k` | 159.98 ms | 138.80 ms | 107.76 ms | 3.06× | **BREACH** |
| `E3_newline_128k` | 193.16 ms | 158.63 ms | 140.94 ms | 3.70× | **BREACH** |
| `E4_newline_192k` | 185.83 ms | 168.32 ms | 133.61 ms | 3.56× | **BREACH** |
| `E5_newline_250k_8f` | 179.26 ms | 159.25 ms | 127.04 ms | 3.43× | **BREACH** |

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
| `D1_newline_50k` | 51200 | 102592 | 2.00× | 117.72 ms | 32.62 ms | 85.10 ms | 20.82 ms | 0.57 ms | **BREACH** |
| `D2_newline_250k` | 256000 | 512960 | 2.00× | 125.32 ms | 88.23 ms | 37.08 ms | 61.40 ms | 4.11 ms | **BREACH** |
| `E1_newline_64k` | 65536 | 131264 | 2.00× | 122.18 ms | 51.15 ms | 71.03 ms | 32.61 ms | 1.60 ms | **BREACH** |
| `E2_newline_96k` | 98304 | 196800 | 2.00× | 107.76 ms | 45.82 ms | 61.94 ms | 31.11 ms | 1.11 ms | **BREACH** |
| `E3_newline_128k` | 131072 | 262336 | 2.00× | 140.94 ms | 44.84 ms | 96.10 ms | 30.68 ms | 1.92 ms | **BREACH** |
| `E4_newline_192k` | 196608 | 393408 | 2.00× | 133.61 ms | 59.28 ms | 74.33 ms | 40.63 ms | 2.16 ms | **BREACH** |
| `E5_newline_250k_8f` | 256000 | 512192 | 2.00× | 127.04 ms | 63.09 ms | 63.96 ms | 44.52 ms | 2.60 ms | **BREACH** |

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
| `E3_newline_128k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `E4_newline_192k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 14 shard(s) | 60 |
| `E5_newline_250k_8f` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 14 shard(s) | 60 |

**More than one projection under a single arm's cache.** That is the shape of the defect this table exists for: the number may be about a store rift never opened.

## Phases

From one extra **untimed** commit per arm with `RIFT_HOOK_TIMING=1`, straight out of the shipped binary. Untimed because the timer takes clock readings the shipped default never takes.

**Read the `exclusive` column, and never sum `recorded`.** The timer records on completion, so a nested phase prints before its parent and the parent's figure already contains it: `predict_id` INCLUDES `redact`. `recorded` is what the binary printed; `exclusive` has every declared child subtracted, so those values are disjoint and may be added. Summing `recorded` would double-count the redaction — the one quantity the opted-in arms exist to separate.

| arm | hook | phase | recorded µs | exclusive µs |
|---|---|---|---|---|
| `D1_newline_50k` | post-commit | discover | 39 | 39 |
| `D1_newline_50k` | post-commit | enqueue | 521 | 521 |
| `D1_newline_50k` | post-commit | gix_open | 779 | 779 |
| `D1_newline_50k` | post-commit | head_hex | 425 | 425 |
| `D1_newline_50k` | post-commit | live_sessions | 628 | 628 |
| `D1_newline_50k` | post-commit | spawn_worker | 1481 | 1481 |
| `D1_newline_50k` | post-commit | total | 4140 | 4140 |
| `D1_newline_50k` | prepare-commit-msg | attribution_preview | 466 | 466 |
| `D1_newline_50k` | prepare-commit-msg | discover | 31 | 31 |
| `D1_newline_50k` | prepare-commit-msg | live_sessions | 897 | 897 |
| `D1_newline_50k` | prepare-commit-msg | open_projection | 2923 | 2923 |
| `D1_newline_50k` | prepare-commit-msg | predict_id | 21393 | 570 (parent, `redact` subtracted) |
| `D1_newline_50k` | prepare-commit-msg | raw_capture | 1782 | 1782 |
| `D1_newline_50k` | prepare-commit-msg | read_facts | 194 | 194 |
| `D1_newline_50k` | prepare-commit-msg | redact | 20823 | 20823 |
| `D1_newline_50k` | prepare-commit-msg | rule_packs | 31 | 31 |
| `D1_newline_50k` | prepare-commit-msg | tokio_build | 29 | 29 |
| `D1_newline_50k` | prepare-commit-msg | total | 28476 | 28476 |
| `D2_newline_250k` | post-commit | discover | 34 | 34 |
| `D2_newline_250k` | post-commit | enqueue | 509 | 509 |
| `D2_newline_250k` | post-commit | gix_open | 782 | 782 |
| `D2_newline_250k` | post-commit | head_hex | 469 | 469 |
| `D2_newline_250k` | post-commit | live_sessions | 865 | 865 |
| `D2_newline_250k` | post-commit | spawn_worker | 1502 | 1502 |
| `D2_newline_250k` | post-commit | total | 4440 | 4440 |
| `D2_newline_250k` | prepare-commit-msg | attribution_preview | 612 | 612 |
| `D2_newline_250k` | prepare-commit-msg | discover | 45 | 45 |
| `D2_newline_250k` | prepare-commit-msg | live_sessions | 1018 | 1018 |
| `D2_newline_250k` | prepare-commit-msg | open_projection | 4238 | 4238 |
| `D2_newline_250k` | prepare-commit-msg | predict_id | 65504 | 4107 (parent, `redact` subtracted) |
| `D2_newline_250k` | prepare-commit-msg | raw_capture | 10762 | 10762 |
| `D2_newline_250k` | prepare-commit-msg | read_facts | 178 | 178 |
| `D2_newline_250k` | prepare-commit-msg | redact | 61397 | 61397 |
| `D2_newline_250k` | prepare-commit-msg | rule_packs | 47 | 47 |
| `D2_newline_250k` | prepare-commit-msg | tokio_build | 30 | 30 |
| `D2_newline_250k` | prepare-commit-msg | total | 83794 | 83794 |
| `E1_newline_64k` | post-commit | discover | 56 | 56 |
| `E1_newline_64k` | post-commit | enqueue | 766 | 766 |
| `E1_newline_64k` | post-commit | gix_open | 1164 | 1164 |
| `E1_newline_64k` | post-commit | head_hex | 751 | 751 |
| `E1_newline_64k` | post-commit | live_sessions | 960 | 960 |
| `E1_newline_64k` | post-commit | spawn_worker | 1980 | 1980 |
| `E1_newline_64k` | post-commit | total | 5990 | 5990 |
| `E1_newline_64k` | prepare-commit-msg | attribution_preview | 762 | 762 |
| `E1_newline_64k` | prepare-commit-msg | discover | 44 | 44 |
| `E1_newline_64k` | prepare-commit-msg | live_sessions | 968 | 968 |
| `E1_newline_64k` | prepare-commit-msg | open_projection | 4023 | 4023 |
| `E1_newline_64k` | prepare-commit-msg | predict_id | 34213 | 1603 (parent, `redact` subtracted) |
| `E1_newline_64k` | prepare-commit-msg | raw_capture | 2684 | 2684 |
| `E1_newline_64k` | prepare-commit-msg | read_facts | 204 | 204 |
| `E1_newline_64k` | prepare-commit-msg | redact | 32610 | 32610 |
| `E1_newline_64k` | prepare-commit-msg | rule_packs | 38 | 38 |
| `E1_newline_64k` | prepare-commit-msg | tokio_build | 29 | 29 |
| `E1_newline_64k` | prepare-commit-msg | total | 45159 | 45159 |
| `E2_newline_96k` | post-commit | discover | 34 | 34 |
| `E2_newline_96k` | post-commit | enqueue | 662 | 662 |
| `E2_newline_96k` | post-commit | gix_open | 1160 | 1160 |
| `E2_newline_96k` | post-commit | head_hex | 744 | 744 |
| `E2_newline_96k` | post-commit | live_sessions | 807 | 807 |
| `E2_newline_96k` | post-commit | spawn_worker | 2248 | 2248 |
| `E2_newline_96k` | post-commit | total | 5952 | 5952 |
| `E2_newline_96k` | prepare-commit-msg | attribution_preview | 411 | 411 |
| `E2_newline_96k` | prepare-commit-msg | discover | 34 | 34 |
| `E2_newline_96k` | prepare-commit-msg | live_sessions | 656 | 656 |
| `E2_newline_96k` | prepare-commit-msg | open_projection | 2924 | 2924 |
| `E2_newline_96k` | prepare-commit-msg | predict_id | 32223 | 1109 (parent, `redact` subtracted) |
| `E2_newline_96k` | prepare-commit-msg | raw_capture | 2562 | 2562 |
| `E2_newline_96k` | prepare-commit-msg | read_facts | 149 | 149 |
| `E2_newline_96k` | prepare-commit-msg | redact | 31114 | 31114 |
| `E2_newline_96k` | prepare-commit-msg | rule_packs | 29 | 29 |
| `E2_newline_96k` | prepare-commit-msg | tokio_build | 24 | 24 |
| `E2_newline_96k` | prepare-commit-msg | total | 39869 | 39869 |
| `E3_newline_128k` | post-commit | discover | 39 | 39 |
| `E3_newline_128k` | post-commit | enqueue | 526 | 526 |
| `E3_newline_128k` | post-commit | gix_open | 821 | 821 |
| `E3_newline_128k` | post-commit | head_hex | 488 | 488 |
| `E3_newline_128k` | post-commit | live_sessions | 789 | 789 |
| `E3_newline_128k` | post-commit | spawn_worker | 1536 | 1536 |
| `E3_newline_128k` | post-commit | total | 4445 | 4445 |
| `E3_newline_128k` | prepare-commit-msg | attribution_preview | 597 | 597 |
| `E3_newline_128k` | prepare-commit-msg | discover | 33 | 33 |
| `E3_newline_128k` | prepare-commit-msg | live_sessions | 599 | 599 |
| `E3_newline_128k` | prepare-commit-msg | open_projection | 2602 | 2602 |
| `E3_newline_128k` | prepare-commit-msg | predict_id | 32597 | 1919 (parent, `redact` subtracted) |
| `E3_newline_128k` | prepare-commit-msg | raw_capture | 2666 | 2666 |
| `E3_newline_128k` | prepare-commit-msg | read_facts | 140 | 140 |
| `E3_newline_128k` | prepare-commit-msg | redact | 30678 | 30678 |
| `E3_newline_128k` | prepare-commit-msg | rule_packs | 29 | 29 |
| `E3_newline_128k` | prepare-commit-msg | tokio_build | 23 | 23 |
| `E3_newline_128k` | prepare-commit-msg | total | 40400 | 40400 |
| `E4_newline_192k` | post-commit | discover | 33 | 33 |
| `E4_newline_192k` | post-commit | enqueue | 587 | 587 |
| `E4_newline_192k` | post-commit | gix_open | 776 | 776 |
| `E4_newline_192k` | post-commit | head_hex | 845 | 845 |
| `E4_newline_192k` | post-commit | live_sessions | 1067 | 1067 |
| `E4_newline_192k` | post-commit | spawn_worker | 1426 | 1426 |
| `E4_newline_192k` | post-commit | total | 5045 | 5045 |
| `E4_newline_192k` | prepare-commit-msg | attribution_preview | 441 | 441 |
| `E4_newline_192k` | prepare-commit-msg | discover | 49 | 49 |
| `E4_newline_192k` | prepare-commit-msg | live_sessions | 1196 | 1196 |
| `E4_newline_192k` | prepare-commit-msg | open_projection | 2979 | 2979 |
| `E4_newline_192k` | prepare-commit-msg | predict_id | 42792 | 2159 (parent, `redact` subtracted) |
| `E4_newline_192k` | prepare-commit-msg | raw_capture | 5449 | 5449 |
| `E4_newline_192k` | prepare-commit-msg | read_facts | 205 | 205 |
| `E4_newline_192k` | prepare-commit-msg | redact | 40633 | 40633 |
| `E4_newline_192k` | prepare-commit-msg | rule_packs | 46 | 46 |
| `E4_newline_192k` | prepare-commit-msg | tokio_build | 32 | 32 |
| `E4_newline_192k` | prepare-commit-msg | total | 54236 | 54236 |
| `E5_newline_250k_8f` | post-commit | discover | 32 | 32 |
| `E5_newline_250k_8f` | post-commit | enqueue | 796 | 796 |
| `E5_newline_250k_8f` | post-commit | gix_open | 849 | 849 |
| `E5_newline_250k_8f` | post-commit | head_hex | 438 | 438 |
| `E5_newline_250k_8f` | post-commit | live_sessions | 724 | 724 |
| `E5_newline_250k_8f` | post-commit | spawn_worker | 1718 | 1718 |
| `E5_newline_250k_8f` | post-commit | total | 4815 | 4815 |
| `E5_newline_250k_8f` | prepare-commit-msg | attribution_preview | 386 | 386 |
| `E5_newline_250k_8f` | prepare-commit-msg | discover | 58 | 58 |
| `E5_newline_250k_8f` | prepare-commit-msg | live_sessions | 837 | 837 |
| `E5_newline_250k_8f` | prepare-commit-msg | open_projection | 2864 | 2864 |
| `E5_newline_250k_8f` | prepare-commit-msg | predict_id | 47113 | 2597 (parent, `redact` subtracted) |
| `E5_newline_250k_8f` | prepare-commit-msg | raw_capture | 5683 | 5683 |
| `E5_newline_250k_8f` | prepare-commit-msg | read_facts | 152 | 152 |
| `E5_newline_250k_8f` | prepare-commit-msg | redact | 44516 | 44516 |
| `E5_newline_250k_8f` | prepare-commit-msg | rule_packs | 37 | 37 |
| `E5_newline_250k_8f` | prepare-commit-msg | tokio_build | 25 | 25 |
| `E5_newline_250k_8f` | prepare-commit-msg | total | 58271 | 58271 |

### Phase audit

`total` is measured independently of the phases, so disjoint phases can never exceed it. `over-attributed` is therefore a proof that something is nested which the harness does not declare — not a rounding remark.

| arm | hook | total µs | attributed µs | unattributed µs | verdict |
|---|---|---|---|---|---|
| `D1_newline_50k` | post-commit | 4140 | 3873 | 267 | adds up |
| `D1_newline_50k` | prepare-commit-msg | 28476 | 27746 | 730 | adds up |
| `D2_newline_250k` | post-commit | 4440 | 4161 | 279 | adds up |
| `D2_newline_250k` | prepare-commit-msg | 83794 | 82434 | 1360 | adds up |
| `E1_newline_64k` | post-commit | 5990 | 5677 | 313 | adds up |
| `E1_newline_64k` | prepare-commit-msg | 45159 | 42965 | 2194 | adds up |
| `E2_newline_96k` | post-commit | 5952 | 5655 | 297 | adds up |
| `E2_newline_96k` | prepare-commit-msg | 39869 | 39012 | 857 | adds up |
| `E3_newline_128k` | post-commit | 4445 | 4199 | 246 | adds up |
| `E3_newline_128k` | prepare-commit-msg | 40400 | 39286 | 1114 | adds up |
| `E4_newline_192k` | post-commit | 5045 | 4734 | 311 | adds up |
| `E4_newline_192k` | prepare-commit-msg | 54236 | 53189 | 1047 | adds up |
| `E5_newline_250k_8f` | post-commit | 4815 | 4557 | 258 | adds up |
| `E5_newline_250k_8f` | prepare-commit-msg | 58271 | 57155 | 1116 | adds up |
