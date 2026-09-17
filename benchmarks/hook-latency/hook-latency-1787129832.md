# Hook latency — git-latency sweep

- harness `0.0.1`, artifact schema 1, generated 1787129832
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
| `A_bare` | 61.23 ms | 51.09 ms | — | 1.00× | control |
| `B1_live` | 159.77 ms | 137.73 ms | 98.53 ms | 2.61× | meets |
| `D1_newline_50k` | 165.74 ms | 144.48 ms | 104.50 ms | 2.71× | **BREACH** |
| `F1_newline_8k` | 157.57 ms | 125.61 ms | 96.34 ms | 2.57× | meets |
| `F2_newline_16k` | 166.96 ms | 138.33 ms | 105.73 ms | 2.73× | **BREACH** |
| `F3_newline_24k` | 167.96 ms | 129.62 ms | 106.73 ms | 2.74× | **BREACH** |
| `F4_newline_32k` | 172.89 ms | 125.35 ms | 111.65 ms | 2.82× | **BREACH** |
| `F5_newline_40k` | 172.09 ms | 133.76 ms | 110.85 ms | 2.81× | **BREACH** |

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

**BREACH: `D1_newline_50k`, `F2_newline_16k`, `F3_newline_24k`, `F4_newline_32k`, `F5_newline_40k`.** The budget is 100 ms of git latency and is HARD (set by the owner). The corpus was not adjusted in response — the arms are the sizes they were declared at.

**Magnitude and attribution come from different commits.** `git latency` is the median of the timed commits minus the control's; every phase column comes from the arm's one **extra untimed** commit with the timer on. Read the residue as an anatomy of a representative commit beside the magnitude of the median, never as an exact decomposition of one commit.

`predict_id` here is **exclusive** — `redact` subtracted — so the columns are disjoint and may be added.

| arm | bytes read | handed on | ×read | git latency | hook bodies | outside bodies | `redact` | `predict_id` less `redact` | verdict |
|---|---|---|---|---|---|---|---|---|---|
| `B1_live` | — | — | — | 98.53 ms | 44.56 ms | 53.97 ms | 30.80 ms | 0.08 ms | meets |
| `D1_newline_50k` | 51200 | 102592 | 2.00× | 104.50 ms | 52.06 ms | 52.44 ms | 31.05 ms | 0.87 ms | **BREACH** |
| `F1_newline_8k` | 8192 | 16576 | 2.02× | 96.34 ms | 39.03 ms | 57.30 ms | 24.27 ms | 0.22 ms | meets |
| `F2_newline_16k` | 16384 | 32960 | 2.01× | 105.73 ms | 39.87 ms | 65.86 ms | 24.43 ms | 0.22 ms | **BREACH** |
| `F3_newline_24k` | 24576 | 49344 | 2.01× | 106.72 ms | 45.66 ms | 61.07 ms | 28.09 ms | 0.42 ms | **BREACH** |
| `F4_newline_32k` | 32768 | 65728 | 2.01× | 111.65 ms | 32.83 ms | 78.82 ms | 21.61 ms | 0.41 ms | **BREACH** |
| `F5_newline_40k` | 40960 | 82112 | 2.00× | 110.85 ms | 37.63 ms | 73.23 ms | 20.63 ms | 0.47 ms | **BREACH** |

`outside bodies` is the floor a Rust change cannot move: three `/bin/sh` invocations and three execs that git requires, plus git's own cost of running them. If the budget ever has to halve, that column is what has to be attacked, and it is not in Rust code.

## Oracles — what each arm exercised

Reported for every arm including the control, because a check that only ever confirms is not a check: the idle arm's **absent** trailer is what makes the live arm's present one mean something.

| arm | projections | trailer | verify | events |
|---|---|---|---|---|
| `A_bare` | none | absent | — | — |
| `B1_live` | rift/<repo-id>/projection.db 696320 B; rift/canopy/ui-state.db 90112 B | present | — | 100 |
| `D1_newline_50k` | rift/<repo-id>/projection.db 679936 B; rift/canopy/ui-state.db 90112 B | present | 22 checkpoint(s), 23 shard(s) | 100 |
| `F1_newline_8k` | rift/<repo-id>/projection.db 696320 B; rift/canopy/ui-state.db 90112 B | present | 23 checkpoint(s), 24 shard(s) | 100 |
| `F2_newline_16k` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 704512 B | present | — | 100 |
| `F3_newline_24k` | rift/<repo-id>/projection.db 696320 B; rift/canopy/ui-state.db 90112 B | present | — | 100 |
| `F4_newline_32k` | rift/<repo-id>/projection.db 679936 B; rift/canopy/ui-state.db 90112 B | present | 22 checkpoint(s), 23 shard(s) | 100 |
| `F5_newline_40k` | rift/<repo-id>/projection.db 688128 B; rift/canopy/ui-state.db 90112 B | present | 23 checkpoint(s), 23 shard(s) | 100 |

**More than one projection under a single arm's cache.** That is the shape of the defect this table exists for: the number may be about a store rift never opened.

## Phases

From one extra **untimed** commit per arm with `RIFT_HOOK_TIMING=1`, straight out of the shipped binary. Untimed because the timer takes clock readings the shipped default never takes.

**Read the `exclusive` column, and never sum `recorded`.** The timer records on completion, so a nested phase prints before its parent and the parent's figure already contains it: `predict_id` INCLUDES `redact`. `recorded` is what the binary printed; `exclusive` has every declared child subtracted, so those values are disjoint and may be added. Summing `recorded` would double-count the redaction — the one quantity the opted-in arms exist to separate.

| arm | hook | phase | recorded µs | exclusive µs |
|---|---|---|---|---|
| `B1_live` | post-commit | discover | 55 | 55 |
| `B1_live` | post-commit | enqueue | 703 | 703 |
| `B1_live` | post-commit | gix_open | 1201 | 1201 |
| `B1_live` | post-commit | head_hex | 683 | 683 |
| `B1_live` | post-commit | live_sessions | 1723 | 1723 |
| `B1_live` | post-commit | spawn_worker | 1468 | 1468 |
| `B1_live` | post-commit | total | 6112 | 6112 |
| `B1_live` | prepare-commit-msg | attribution_preview | 1454 | 1454 |
| `B1_live` | prepare-commit-msg | discover | 37 | 37 |
| `B1_live` | prepare-commit-msg | live_sessions | 1428 | 1428 |
| `B1_live` | prepare-commit-msg | open_projection | 3401 | 3401 |
| `B1_live` | prepare-commit-msg | predict_id | 30878 | 78 (parent, `redact` subtracted) |
| `B1_live` | prepare-commit-msg | raw_capture | 30 | 30 |
| `B1_live` | prepare-commit-msg | read_facts | 188 | 188 |
| `B1_live` | prepare-commit-msg | redact | 30800 | 30800 |
| `B1_live` | prepare-commit-msg | rule_packs | 25 | 25 |
| `B1_live` | prepare-commit-msg | tokio_build | 27 | 27 |
| `B1_live` | prepare-commit-msg | total | 38450 | 38450 |
| `D1_newline_50k` | post-commit | discover | 64 | 64 |
| `D1_newline_50k` | post-commit | enqueue | 811 | 811 |
| `D1_newline_50k` | post-commit | gix_open | 1336 | 1336 |
| `D1_newline_50k` | post-commit | head_hex | 619 | 619 |
| `D1_newline_50k` | post-commit | live_sessions | 1705 | 1705 |
| `D1_newline_50k` | post-commit | spawn_worker | 3024 | 3024 |
| `D1_newline_50k` | post-commit | total | 7985 | 7985 |
| `D1_newline_50k` | prepare-commit-msg | attribution_preview | 731 | 731 |
| `D1_newline_50k` | prepare-commit-msg | discover | 46 | 46 |
| `D1_newline_50k` | prepare-commit-msg | live_sessions | 2375 | 2375 |
| `D1_newline_50k` | prepare-commit-msg | open_projection | 4707 | 4707 |
| `D1_newline_50k` | prepare-commit-msg | predict_id | 31918 | 873 (parent, `redact` subtracted) |
| `D1_newline_50k` | prepare-commit-msg | raw_capture | 2932 | 2932 |
| `D1_newline_50k` | prepare-commit-msg | read_facts | 215 | 215 |
| `D1_newline_50k` | prepare-commit-msg | redact | 31045 | 31045 |
| `D1_newline_50k` | prepare-commit-msg | rule_packs | 44 | 44 |
| `D1_newline_50k` | prepare-commit-msg | tokio_build | 32 | 32 |
| `D1_newline_50k` | prepare-commit-msg | total | 44078 | 44078 |
| `F1_newline_8k` | post-commit | discover | 67 | 67 |
| `F1_newline_8k` | post-commit | enqueue | 746 | 746 |
| `F1_newline_8k` | post-commit | gix_open | 1369 | 1369 |
| `F1_newline_8k` | post-commit | head_hex | 717 | 717 |
| `F1_newline_8k` | post-commit | live_sessions | 1757 | 1757 |
| `F1_newline_8k` | post-commit | spawn_worker | 1894 | 1894 |
| `F1_newline_8k` | post-commit | total | 6850 | 6850 |
| `F1_newline_8k` | prepare-commit-msg | attribution_preview | 577 | 577 |
| `F1_newline_8k` | prepare-commit-msg | discover | 33 | 33 |
| `F1_newline_8k` | prepare-commit-msg | live_sessions | 1149 | 1149 |
| `F1_newline_8k` | prepare-commit-msg | open_projection | 3924 | 3924 |
| `F1_newline_8k` | prepare-commit-msg | predict_id | 24486 | 221 (parent, `redact` subtracted) |
| `F1_newline_8k` | prepare-commit-msg | raw_capture | 687 | 687 |
| `F1_newline_8k` | prepare-commit-msg | read_facts | 141 | 141 |
| `F1_newline_8k` | prepare-commit-msg | redact | 24265 | 24265 |
| `F1_newline_8k` | prepare-commit-msg | rule_packs | 18 | 18 |
| `F1_newline_8k` | prepare-commit-msg | tokio_build | 23 | 23 |
| `F1_newline_8k` | prepare-commit-msg | total | 32184 | 32184 |
| `F2_newline_16k` | post-commit | discover | 49 | 49 |
| `F2_newline_16k` | post-commit | enqueue | 439 | 439 |
| `F2_newline_16k` | post-commit | gix_open | 1126 | 1126 |
| `F2_newline_16k` | post-commit | head_hex | 686 | 686 |
| `F2_newline_16k` | post-commit | live_sessions | 1294 | 1294 |
| `F2_newline_16k` | post-commit | spawn_worker | 1985 | 1985 |
| `F2_newline_16k` | post-commit | total | 5884 | 5884 |
| `F2_newline_16k` | prepare-commit-msg | attribution_preview | 375 | 375 |
| `F2_newline_16k` | prepare-commit-msg | discover | 63 | 63 |
| `F2_newline_16k` | prepare-commit-msg | live_sessions | 2036 | 2036 |
| `F2_newline_16k` | prepare-commit-msg | open_projection | 4176 | 4176 |
| `F2_newline_16k` | prepare-commit-msg | predict_id | 24644 | 217 (parent, `redact` subtracted) |
| `F2_newline_16k` | prepare-commit-msg | raw_capture | 1559 | 1559 |
| `F2_newline_16k` | prepare-commit-msg | read_facts | 191 | 191 |
| `F2_newline_16k` | prepare-commit-msg | redact | 24427 | 24427 |
| `F2_newline_16k` | prepare-commit-msg | rule_packs | 36 | 36 |
| `F2_newline_16k` | prepare-commit-msg | tokio_build | 30 | 30 |
| `F2_newline_16k` | prepare-commit-msg | total | 33987 | 33987 |
| `F3_newline_24k` | post-commit | discover | 88 | 88 |
| `F3_newline_24k` | post-commit | enqueue | 729 | 729 |
| `F3_newline_24k` | post-commit | gix_open | 1027 | 1027 |
| `F3_newline_24k` | post-commit | head_hex | 606 | 606 |
| `F3_newline_24k` | post-commit | live_sessions | 1903 | 1903 |
| `F3_newline_24k` | post-commit | spawn_worker | 2938 | 2938 |
| `F3_newline_24k` | post-commit | total | 7625 | 7625 |
| `F3_newline_24k` | prepare-commit-msg | attribution_preview | 598 | 598 |
| `F3_newline_24k` | prepare-commit-msg | discover | 63 | 63 |
| `F3_newline_24k` | prepare-commit-msg | live_sessions | 1740 | 1740 |
| `F3_newline_24k` | prepare-commit-msg | open_projection | 4110 | 4110 |
| `F3_newline_24k` | prepare-commit-msg | predict_id | 28508 | 423 (parent, `redact` subtracted) |
| `F3_newline_24k` | prepare-commit-msg | raw_capture | 1657 | 1657 |
| `F3_newline_24k` | prepare-commit-msg | read_facts | 196 | 196 |
| `F3_newline_24k` | prepare-commit-msg | redact | 28085 | 28085 |
| `F3_newline_24k` | prepare-commit-msg | rule_packs | 36 | 36 |
| `F3_newline_24k` | prepare-commit-msg | tokio_build | 29 | 29 |
| `F3_newline_24k` | prepare-commit-msg | total | 38033 | 38033 |
| `F4_newline_32k` | post-commit | discover | 32 | 32 |
| `F4_newline_32k` | post-commit | enqueue | 466 | 466 |
| `F4_newline_32k` | post-commit | gix_open | 797 | 797 |
| `F4_newline_32k` | post-commit | head_hex | 450 | 450 |
| `F4_newline_32k` | post-commit | live_sessions | 1055 | 1055 |
| `F4_newline_32k` | post-commit | spawn_worker | 1374 | 1374 |
| `F4_newline_32k` | post-commit | total | 4426 | 4426 |
| `F4_newline_32k` | prepare-commit-msg | attribution_preview | 386 | 386 |
| `F4_newline_32k` | prepare-commit-msg | discover | 32 | 32 |
| `F4_newline_32k` | prepare-commit-msg | live_sessions | 1066 | 1066 |
| `F4_newline_32k` | prepare-commit-msg | open_projection | 2697 | 2697 |
| `F4_newline_32k` | prepare-commit-msg | predict_id | 22030 | 415 (parent, `redact` subtracted) |
| `F4_newline_32k` | prepare-commit-msg | raw_capture | 1063 | 1063 |
| `F4_newline_32k` | prepare-commit-msg | read_facts | 127 | 127 |
| `F4_newline_32k` | prepare-commit-msg | redact | 21615 | 21615 |
| `F4_newline_32k` | prepare-commit-msg | rule_packs | 20 | 20 |
| `F4_newline_32k` | prepare-commit-msg | tokio_build | 24 | 24 |
| `F4_newline_32k` | prepare-commit-msg | total | 28408 | 28408 |
| `F5_newline_40k` | post-commit | discover | 40 | 40 |
| `F5_newline_40k` | post-commit | enqueue | 689 | 689 |
| `F5_newline_40k` | post-commit | gix_open | 1063 | 1063 |
| `F5_newline_40k` | post-commit | head_hex | 726 | 726 |
| `F5_newline_40k` | post-commit | live_sessions | 1318 | 1318 |
| `F5_newline_40k` | post-commit | spawn_worker | 2937 | 2937 |
| `F5_newline_40k` | post-commit | total | 7064 | 7064 |
| `F5_newline_40k` | prepare-commit-msg | attribution_preview | 407 | 407 |
| `F5_newline_40k` | prepare-commit-msg | discover | 50 | 50 |
| `F5_newline_40k` | prepare-commit-msg | live_sessions | 1436 | 1436 |
| `F5_newline_40k` | prepare-commit-msg | open_projection | 4076 | 4076 |
| `F5_newline_40k` | prepare-commit-msg | predict_id | 21107 | 473 (parent, `redact` subtracted) |
| `F5_newline_40k` | prepare-commit-msg | raw_capture | 2176 | 2176 |
| `F5_newline_40k` | prepare-commit-msg | read_facts | 199 | 199 |
| `F5_newline_40k` | prepare-commit-msg | redact | 20634 | 20634 |
| `F5_newline_40k` | prepare-commit-msg | rule_packs | 42 | 42 |
| `F5_newline_40k` | prepare-commit-msg | tokio_build | 30 | 30 |
| `F5_newline_40k` | prepare-commit-msg | total | 30563 | 30563 |

### Phase audit

`total` is measured independently of the phases, so disjoint phases can never exceed it. `over-attributed` is therefore a proof that something is nested which the harness does not declare — not a rounding remark.

| arm | hook | total µs | attributed µs | unattributed µs | verdict |
|---|---|---|---|---|---|
| `B1_live` | post-commit | 6112 | 5833 | 279 | adds up |
| `B1_live` | prepare-commit-msg | 38450 | 37468 | 982 | adds up |
| `D1_newline_50k` | post-commit | 7985 | 7559 | 426 | adds up |
| `D1_newline_50k` | prepare-commit-msg | 44078 | 43000 | 1078 | adds up |
| `F1_newline_8k` | post-commit | 6850 | 6550 | 300 | adds up |
| `F1_newline_8k` | prepare-commit-msg | 32184 | 31038 | 1146 | adds up |
| `F2_newline_16k` | post-commit | 5884 | 5579 | 305 | adds up |
| `F2_newline_16k` | prepare-commit-msg | 33987 | 33110 | 877 | adds up |
| `F3_newline_24k` | post-commit | 7625 | 7291 | 334 | adds up |
| `F3_newline_24k` | prepare-commit-msg | 38033 | 36937 | 1096 | adds up |
| `F4_newline_32k` | post-commit | 4426 | 4174 | 252 | adds up |
| `F4_newline_32k` | prepare-commit-msg | 28408 | 27445 | 963 | adds up |
| `F5_newline_40k` | post-commit | 7064 | 6773 | 291 | adds up |
| `F5_newline_40k` | prepare-commit-msg | 30563 | 29523 | 1040 | adds up |
