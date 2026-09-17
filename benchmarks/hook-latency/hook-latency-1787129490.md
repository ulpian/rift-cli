# Hook latency — git-latency sweep

- harness `0.0.1`, artifact schema 1, generated 1787129490
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
| `A_bare` | 49.46 ms | 42.97 ms | — | 1.00× | control |
| `B1_live` | 173.89 ms | 140.44 ms | 124.43 ms | 3.52× | **BREACH** |
| `D1_newline_50k` | 167.33 ms | 141.86 ms | 117.87 ms | 3.38× | **BREACH** |
| `F1_newline_8k` | 163.56 ms | 140.92 ms | 114.10 ms | 3.31× | **BREACH** |
| `F2_newline_16k` | 174.61 ms | 135.87 ms | 125.15 ms | 3.53× | **BREACH** |
| `F3_newline_24k` | 176.66 ms | 143.29 ms | 127.20 ms | 3.57× | **BREACH** |
| `F4_newline_32k` | 149.37 ms | 133.74 ms | 99.91 ms | 3.02× | meets |
| `F5_newline_40k` | 150.59 ms | 136.51 ms | 101.13 ms | 3.04× | **BREACH** |

- `A_bare` — bare commit, no hooks installed (control)

- `B1_live` — hooks installed, a session live at commit time

- `D1_newline_50k` — live session + `.worktreeinclude` over 8 files (51200 B read, 102592 B handed on, 2.00x)

- `F1_newline_8k` — live session + `.worktreeinclude` over 8 files (8192 B read, 16576 B handed on, 2.02x)

- `F2_newline_16k` — live session + `.worktreeinclude` over 8 files (16384 B read, 32960 B handed on, 2.01x)

- `F3_newline_24k` — live session + `.worktreeinclude` over 8 files (24576 B read, 49344 B handed on, 2.01x)

- `F4_newline_32k` — live session + `.worktreeinclude` over 8 files (32768 B read, 65728 B handed on, 2.01x)

- `F5_newline_40k` — live session + `.worktreeinclude` over 8 files (40960 B read, 82112 B handed on, 2.00x)

## Opted-in corpus

`MAX_TOTAL_BYTES` is 1 MiB and is charged on bytes **read**; `Walk::file` prefixes every content line with one space, so N bytes contribute up to **2N + 2** to the document the redactor is handed. Both numbers are published, because an arm described as "1 MiB" without saying which 1 MiB misstates the ceiling in one direction or the other.

| arm | files | bytes read | budget | document | factor | files read (explain) |
|---|---|---|---|---|---|---|
| `D1_newline_50k` | 8 | 51200 | under | 102592 | 2.00× | — |
| `F1_newline_8k` | 8 | 8192 | under | 16576 | 2.02× | — |
| `F2_newline_16k` | 8 | 16384 | under | 32960 | 2.01× | — |
| `F3_newline_24k` | 8 | 24576 | under | 49344 | 2.01× | — |
| `F4_newline_32k` | 8 | 32768 | under | 65728 | 2.01× | — |
| `F5_newline_40k` | 8 | 40960 | under | 82112 | 2.00× | — |

The factor here is near 1.0 because these lines are long. The 2× is the pathological end — a file that is all newlines, where the one-space prefix is the whole line — so a full budget can hand on a document just over 2 MiB. Neither end is the other, and this table says which one each arm is.

`files read (explain)` is `rift checkpoint explain`'s **own** label, not a span count: the inclusion walk's record count is on no CLI surface today, so the harness publishes the figure the product publishes rather than relabelling it as something it has not established.

## Anatomy — the curve, the units, and the phase that owns it

**BREACH: `B1_live`, `D1_newline_50k`, `F1_newline_8k`, `F2_newline_16k`, `F3_newline_24k`, `F5_newline_40k`.** The budget is 100 ms of git latency and is HARD (set by the owner). The corpus was not adjusted in response — the arms are the sizes they were declared at.

**Magnitude and attribution come from different commits.** `git latency` is the median of the timed commits minus the control's; every phase column comes from the arm's one **extra untimed** commit with the timer on. Read the residue as an anatomy of a representative commit beside the magnitude of the median, never as an exact decomposition of one commit.

`predict_id` here is **exclusive** — `redact` subtracted — so the columns are disjoint and may be added.

| arm | bytes read | handed on | ×read | git latency | hook bodies | outside bodies | `redact` | `predict_id` less `redact` | verdict |
|---|---|---|---|---|---|---|---|---|---|
| `B1_live` | — | — | — | 124.42 ms | 27.20 ms | 97.23 ms | 16.17 ms | 0.06 ms | **BREACH** |
| `D1_newline_50k` | 51200 | 102592 | 2.00× | 117.86 ms | 39.54 ms | 78.32 ms | 24.46 ms | 0.53 ms | **BREACH** |
| `F1_newline_8k` | 8192 | 16576 | 2.02× | 114.10 ms | 43.53 ms | 70.57 ms | 27.73 ms | 0.23 ms | **BREACH** |
| `F2_newline_16k` | 16384 | 32960 | 2.01× | 125.15 ms | 48.84 ms | 76.31 ms | 29.07 ms | 0.36 ms | **BREACH** |
| `F3_newline_24k` | 24576 | 49344 | 2.01× | 127.19 ms | 47.06 ms | 80.14 ms | 29.03 ms | 0.32 ms | **BREACH** |
| `F4_newline_32k` | 32768 | 65728 | 2.01× | 99.91 ms | 33.64 ms | 66.26 ms | 18.62 ms | 0.39 ms | meets |
| `F5_newline_40k` | 40960 | 82112 | 2.00× | 101.13 ms | 40.09 ms | 61.04 ms | 23.17 ms | 0.83 ms | **BREACH** |

`outside bodies` is the floor a Rust change cannot move: three `/bin/sh` invocations and three execs that git requires, plus git's own cost of running them. If the budget ever has to halve, that column is what has to be attacked, and it is not in Rust code.

## Oracles — what each arm exercised

Reported for every arm including the control, because a check that only ever confirms is not a check: the idle arm's **absent** trailer is what makes the live arm's present one mean something.

| arm | projections | trailer | verify | events |
|---|---|---|---|---|
| `A_bare` | none | absent | — | — |
| `B1_live` | rift/<repo-id>/projection.db 704512 B; rift/canopy/ui-state.db 90112 B | present | 24 checkpoint(s), 25 shard(s) | 100 |
| `D1_newline_50k` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 688128 B | present | 23 checkpoint(s), 23 shard(s) | 100 |
| `F1_newline_8k` | rift/<repo-id>/projection.db 688128 B; rift/canopy/ui-state.db 90112 B | present | 23 checkpoint(s), 24 shard(s) | 100 |
| `F2_newline_16k` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 696320 B | present | 24 checkpoint(s), 24 shard(s) | 100 |
| `F3_newline_24k` | rift/<repo-id>/projection.db 696320 B; rift/canopy/ui-state.db 90112 B | present | — | 100 |
| `F4_newline_32k` | rift/<repo-id>/projection.db 696320 B; rift/canopy/ui-state.db 90112 B | present | — | 100 |
| `F5_newline_40k` | rift/<repo-id>/projection.db 688128 B; rift/canopy/ui-state.db 90112 B | present | 23 checkpoint(s), 24 shard(s) | 100 |

**More than one projection under a single arm's cache.** That is the shape of the defect this table exists for: the number may be about a store rift never opened.

## Phases

From one extra **untimed** commit per arm with `RIFT_HOOK_TIMING=1`, straight out of the shipped binary. Untimed because the timer takes clock readings the shipped default never takes.

**Read the `exclusive` column, and never sum `recorded`.** The timer records on completion, so a nested phase prints before its parent and the parent's figure already contains it: `predict_id` INCLUDES `redact`. `recorded` is what the binary printed; `exclusive` has every declared child subtracted, so those values are disjoint and may be added. Summing `recorded` would double-count the redaction — the one quantity the opted-in arms exist to separate.

| arm | hook | phase | recorded µs | exclusive µs |
|---|---|---|---|---|
| `B1_live` | post-commit | discover | 41 | 41 |
| `B1_live` | post-commit | enqueue | 643 | 643 |
| `B1_live` | post-commit | gix_open | 1073 | 1073 |
| `B1_live` | post-commit | head_hex | 606 | 606 |
| `B1_live` | post-commit | live_sessions | 1757 | 1757 |
| `B1_live` | post-commit | spawn_worker | 1436 | 1436 |
| `B1_live` | post-commit | total | 5858 | 5858 |
| `B1_live` | prepare-commit-msg | attribution_preview | 403 | 403 |
| `B1_live` | prepare-commit-msg | discover | 39 | 39 |
| `B1_live` | prepare-commit-msg | live_sessions | 1159 | 1159 |
| `B1_live` | prepare-commit-msg | open_projection | 2599 | 2599 |
| `B1_live` | prepare-commit-msg | predict_id | 16230 | 58 (parent, `redact` subtracted) |
| `B1_live` | prepare-commit-msg | raw_capture | 28 | 28 |
| `B1_live` | prepare-commit-msg | read_facts | 128 | 128 |
| `B1_live` | prepare-commit-msg | redact | 16172 | 16172 |
| `B1_live` | prepare-commit-msg | rule_packs | 24 | 24 |
| `B1_live` | prepare-commit-msg | tokio_build | 23 | 23 |
| `B1_live` | prepare-commit-msg | total | 21340 | 21340 |
| `D1_newline_50k` | post-commit | discover | 42 | 42 |
| `D1_newline_50k` | post-commit | enqueue | 681 | 681 |
| `D1_newline_50k` | post-commit | gix_open | 1169 | 1169 |
| `D1_newline_50k` | post-commit | head_hex | 640 | 640 |
| `D1_newline_50k` | post-commit | live_sessions | 1597 | 1597 |
| `D1_newline_50k` | post-commit | spawn_worker | 2774 | 2774 |
| `D1_newline_50k` | post-commit | total | 7210 | 7210 |
| `D1_newline_50k` | prepare-commit-msg | attribution_preview | 446 | 446 |
| `D1_newline_50k` | prepare-commit-msg | discover | 31 | 31 |
| `D1_newline_50k` | prepare-commit-msg | live_sessions | 1058 | 1058 |
| `D1_newline_50k` | prepare-commit-msg | open_projection | 2460 | 2460 |
| `D1_newline_50k` | prepare-commit-msg | predict_id | 24993 | 533 (parent, `redact` subtracted) |
| `D1_newline_50k` | prepare-commit-msg | raw_capture | 2129 | 2129 |
| `D1_newline_50k` | prepare-commit-msg | read_facts | 157 | 157 |
| `D1_newline_50k` | prepare-commit-msg | redact | 24460 | 24460 |
| `D1_newline_50k` | prepare-commit-msg | rule_packs | 40 | 40 |
| `D1_newline_50k` | prepare-commit-msg | tokio_build | 22 | 22 |
| `D1_newline_50k` | prepare-commit-msg | total | 32332 | 32332 |
| `F1_newline_8k` | post-commit | discover | 60 | 60 |
| `F1_newline_8k` | post-commit | enqueue | 530 | 530 |
| `F1_newline_8k` | post-commit | gix_open | 1332 | 1332 |
| `F1_newline_8k` | post-commit | head_hex | 568 | 568 |
| `F1_newline_8k` | post-commit | live_sessions | 1887 | 1887 |
| `F1_newline_8k` | post-commit | spawn_worker | 1665 | 1665 |
| `F1_newline_8k` | post-commit | total | 6343 | 6343 |
| `F1_newline_8k` | prepare-commit-msg | attribution_preview | 654 | 654 |
| `F1_newline_8k` | prepare-commit-msg | discover | 76 | 76 |
| `F1_newline_8k` | prepare-commit-msg | live_sessions | 1665 | 1665 |
| `F1_newline_8k` | prepare-commit-msg | open_projection | 4438 | 4438 |
| `F1_newline_8k` | prepare-commit-msg | predict_id | 27961 | 234 (parent, `redact` subtracted) |
| `F1_newline_8k` | prepare-commit-msg | raw_capture | 1014 | 1014 |
| `F1_newline_8k` | prepare-commit-msg | read_facts | 214 | 214 |
| `F1_newline_8k` | prepare-commit-msg | redact | 27727 | 27727 |
| `F1_newline_8k` | prepare-commit-msg | rule_packs | 29 | 29 |
| `F1_newline_8k` | prepare-commit-msg | tokio_build | 31 | 31 |
| `F1_newline_8k` | prepare-commit-msg | total | 37183 | 37183 |
| `F2_newline_16k` | post-commit | discover | 58 | 58 |
| `F2_newline_16k` | post-commit | enqueue | 792 | 792 |
| `F2_newline_16k` | post-commit | gix_open | 1435 | 1435 |
| `F2_newline_16k` | post-commit | head_hex | 739 | 739 |
| `F2_newline_16k` | post-commit | live_sessions | 1787 | 1787 |
| `F2_newline_16k` | post-commit | spawn_worker | 3187 | 3187 |
| `F2_newline_16k` | post-commit | total | 8387 | 8387 |
| `F2_newline_16k` | prepare-commit-msg | attribution_preview | 616 | 616 |
| `F2_newline_16k` | prepare-commit-msg | discover | 49 | 49 |
| `F2_newline_16k` | prepare-commit-msg | live_sessions | 2911 | 2911 |
| `F2_newline_16k` | prepare-commit-msg | open_projection | 4428 | 4428 |
| `F2_newline_16k` | prepare-commit-msg | predict_id | 29437 | 365 (parent, `redact` subtracted) |
| `F2_newline_16k` | prepare-commit-msg | raw_capture | 1526 | 1526 |
| `F2_newline_16k` | prepare-commit-msg | read_facts | 212 | 212 |
| `F2_newline_16k` | prepare-commit-msg | redact | 29072 | 29072 |
| `F2_newline_16k` | prepare-commit-msg | rule_packs | 42 | 42 |
| `F2_newline_16k` | prepare-commit-msg | tokio_build | 31 | 31 |
| `F2_newline_16k` | prepare-commit-msg | total | 40448 | 40448 |
| `F3_newline_24k` | post-commit | discover | 61 | 61 |
| `F3_newline_24k` | post-commit | enqueue | 809 | 809 |
| `F3_newline_24k` | post-commit | gix_open | 1298 | 1298 |
| `F3_newline_24k` | post-commit | head_hex | 726 | 726 |
| `F3_newline_24k` | post-commit | live_sessions | 1739 | 1739 |
| `F3_newline_24k` | post-commit | spawn_worker | 1850 | 1850 |
| `F3_newline_24k` | post-commit | total | 6857 | 6857 |
| `F3_newline_24k` | prepare-commit-msg | attribution_preview | 445 | 445 |
| `F3_newline_24k` | prepare-commit-msg | discover | 164 | 164 |
| `F3_newline_24k` | prepare-commit-msg | live_sessions | 2368 | 2368 |
| `F3_newline_24k` | prepare-commit-msg | open_projection | 4805 | 4805 |
| `F3_newline_24k` | prepare-commit-msg | predict_id | 29352 | 320 (parent, `redact` subtracted) |
| `F3_newline_24k` | prepare-commit-msg | raw_capture | 1711 | 1711 |
| `F3_newline_24k` | prepare-commit-msg | read_facts | 201 | 201 |
| `F3_newline_24k` | prepare-commit-msg | redact | 29032 | 29032 |
| `F3_newline_24k` | prepare-commit-msg | rule_packs | 39 | 39 |
| `F3_newline_24k` | prepare-commit-msg | tokio_build | 32 | 32 |
| `F3_newline_24k` | prepare-commit-msg | total | 40203 | 40203 |
| `F4_newline_32k` | post-commit | discover | 48 | 48 |
| `F4_newline_32k` | post-commit | enqueue | 737 | 737 |
| `F4_newline_32k` | post-commit | gix_open | 1292 | 1292 |
| `F4_newline_32k` | post-commit | head_hex | 898 | 898 |
| `F4_newline_32k` | post-commit | live_sessions | 1632 | 1632 |
| `F4_newline_32k` | post-commit | spawn_worker | 3053 | 3053 |
| `F4_newline_32k` | post-commit | total | 8019 | 8019 |
| `F4_newline_32k` | prepare-commit-msg | attribution_preview | 445 | 445 |
| `F4_newline_32k` | prepare-commit-msg | discover | 33 | 33 |
| `F4_newline_32k` | prepare-commit-msg | live_sessions | 1162 | 1162 |
| `F4_newline_32k` | prepare-commit-msg | open_projection | 2863 | 2863 |
| `F4_newline_32k` | prepare-commit-msg | predict_id | 19011 | 392 (parent, `redact` subtracted) |
| `F4_newline_32k` | prepare-commit-msg | raw_capture | 1154 | 1154 |
| `F4_newline_32k` | prepare-commit-msg | read_facts | 141 | 141 |
| `F4_newline_32k` | prepare-commit-msg | redact | 18619 | 18619 |
| `F4_newline_32k` | prepare-commit-msg | rule_packs | 28 | 28 |
| `F4_newline_32k` | prepare-commit-msg | tokio_build | 24 | 24 |
| `F4_newline_32k` | prepare-commit-msg | total | 25625 | 25625 |
| `F5_newline_40k` | post-commit | discover | 53 | 53 |
| `F5_newline_40k` | post-commit | enqueue | 524 | 524 |
| `F5_newline_40k` | post-commit | gix_open | 1160 | 1160 |
| `F5_newline_40k` | post-commit | head_hex | 693 | 693 |
| `F5_newline_40k` | post-commit | live_sessions | 1564 | 1564 |
| `F5_newline_40k` | post-commit | spawn_worker | 1424 | 1424 |
| `F5_newline_40k` | post-commit | total | 5711 | 5711 |
| `F5_newline_40k` | prepare-commit-msg | attribution_preview | 598 | 598 |
| `F5_newline_40k` | prepare-commit-msg | discover | 74 | 74 |
| `F5_newline_40k` | prepare-commit-msg | live_sessions | 1834 | 1834 |
| `F5_newline_40k` | prepare-commit-msg | open_projection | 3994 | 3994 |
| `F5_newline_40k` | prepare-commit-msg | predict_id | 24004 | 830 (parent, `redact` subtracted) |
| `F5_newline_40k` | prepare-commit-msg | raw_capture | 1342 | 1342 |
| `F5_newline_40k` | prepare-commit-msg | read_facts | 141 | 141 |
| `F5_newline_40k` | prepare-commit-msg | redact | 23174 | 23174 |
| `F5_newline_40k` | prepare-commit-msg | rule_packs | 39 | 39 |
| `F5_newline_40k` | prepare-commit-msg | tokio_build | 23 | 23 |
| `F5_newline_40k` | prepare-commit-msg | total | 34380 | 34380 |

### Phase audit

`total` is measured independently of the phases, so disjoint phases can never exceed it. `over-attributed` is therefore a proof that something is nested which the harness does not declare — not a rounding remark.

| arm | hook | total µs | attributed µs | unattributed µs | verdict |
|---|---|---|---|---|---|
| `B1_live` | post-commit | 5858 | 5556 | 302 | adds up |
| `B1_live` | prepare-commit-msg | 21340 | 20633 | 707 | adds up |
| `D1_newline_50k` | post-commit | 7210 | 6903 | 307 | adds up |
| `D1_newline_50k` | prepare-commit-msg | 32332 | 31336 | 996 | adds up |
| `F1_newline_8k` | post-commit | 6343 | 6042 | 301 | adds up |
| `F1_newline_8k` | prepare-commit-msg | 37183 | 36082 | 1101 | adds up |
| `F2_newline_16k` | post-commit | 8387 | 7998 | 389 | adds up |
| `F2_newline_16k` | prepare-commit-msg | 40448 | 39252 | 1196 | adds up |
| `F3_newline_24k` | post-commit | 6857 | 6483 | 374 | adds up |
| `F3_newline_24k` | prepare-commit-msg | 40203 | 39117 | 1086 | adds up |
| `F4_newline_32k` | post-commit | 8019 | 7660 | 359 | adds up |
| `F4_newline_32k` | prepare-commit-msg | 25625 | 24861 | 764 | adds up |
| `F5_newline_40k` | post-commit | 5711 | 5418 | 293 | adds up |
| `F5_newline_40k` | prepare-commit-msg | 34380 | 32049 | 2331 | adds up |
