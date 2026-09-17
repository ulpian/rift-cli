# Hook latency — git-latency sweep

- harness `0.0.1`, artifact schema 1, generated 1787129660
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
| `A_bare` | 49.55 ms | 45.00 ms | — | 1.00× | control |
| `B1_live` | 142.23 ms | 131.04 ms | 92.68 ms | 2.87× | meets |
| `D1_newline_50k` | 158.71 ms | 137.08 ms | 109.17 ms | 3.20× | **BREACH** |
| `F1_newline_8k` | 185.77 ms | 137.50 ms | 136.22 ms | 3.75× | **BREACH** |
| `F2_newline_16k` | 164.35 ms | 145.10 ms | 114.81 ms | 3.32× | **BREACH** |
| `F3_newline_24k` | 175.97 ms | 143.59 ms | 126.42 ms | 3.55× | **BREACH** |
| `F4_newline_32k` | 165.96 ms | 141.40 ms | 116.41 ms | 3.35× | **BREACH** |
| `F5_newline_40k` | 165.26 ms | 131.91 ms | 115.72 ms | 3.34× | **BREACH** |

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

**BREACH: `D1_newline_50k`, `F1_newline_8k`, `F2_newline_16k`, `F3_newline_24k`, `F4_newline_32k`, `F5_newline_40k`.** The budget is 100 ms of git latency and is HARD (set by the owner). The corpus was not adjusted in response — the arms are the sizes they were declared at.

**Magnitude and attribution come from different commits.** `git latency` is the median of the timed commits minus the control's; every phase column comes from the arm's one **extra untimed** commit with the timer on. Read the residue as an anatomy of a representative commit beside the magnitude of the median, never as an exact decomposition of one commit.

`predict_id` here is **exclusive** — `redact` subtracted — so the columns are disjoint and may be added.

| arm | bytes read | handed on | ×read | git latency | hook bodies | outside bodies | `redact` | `predict_id` less `redact` | verdict |
|---|---|---|---|---|---|---|---|---|---|
| `B1_live` | — | — | — | 92.68 ms | 30.26 ms | 62.42 ms | 17.75 ms | 0.08 ms | meets |
| `D1_newline_50k` | 51200 | 102592 | 2.00× | 109.17 ms | 40.25 ms | 68.92 ms | 26.82 ms | 0.59 ms | **BREACH** |
| `F1_newline_8k` | 8192 | 16576 | 2.02× | 136.22 ms | 39.88 ms | 96.34 ms | 22.18 ms | 0.15 ms | **BREACH** |
| `F2_newline_16k` | 16384 | 32960 | 2.01× | 114.81 ms | 47.01 ms | 67.80 ms | 29.32 ms | 0.37 ms | **BREACH** |
| `F3_newline_24k` | 24576 | 49344 | 2.01× | 126.42 ms | 32.53 ms | 93.89 ms | 19.90 ms | 0.57 ms | **BREACH** |
| `F4_newline_32k` | 32768 | 65728 | 2.01× | 116.41 ms | 39.34 ms | 77.08 ms | 25.47 ms | 0.39 ms | **BREACH** |
| `F5_newline_40k` | 40960 | 82112 | 2.00× | 115.72 ms | 48.71 ms | 67.00 ms | 29.72 ms | 0.58 ms | **BREACH** |

`outside bodies` is the floor a Rust change cannot move: three `/bin/sh` invocations and three execs that git requires, plus git's own cost of running them. If the budget ever has to halve, that column is what has to be attacked, and it is not in Rust code.

## Oracles — what each arm exercised

Reported for every arm including the control, because a check that only ever confirms is not a check: the idle arm's **absent** trailer is what makes the live arm's present one mean something.

| arm | projections | trailer | verify | events |
|---|---|---|---|---|
| `A_bare` | none | absent | — | — |
| `B1_live` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 696320 B | present | — | 100 |
| `D1_newline_50k` | rift/<repo-id>/projection.db 696320 B; rift/canopy/ui-state.db 90112 B | present | — | 100 |
| `F1_newline_8k` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 696320 B | present | 23 checkpoint(s), 24 shard(s) | 100 |
| `F2_newline_16k` | rift/<repo-id>/projection.db 696320 B; rift/canopy/ui-state.db 90112 B | present | — | 100 |
| `F3_newline_24k` | rift/<repo-id>/projection.db 688128 B; rift/canopy/ui-state.db 90112 B | present | 23 checkpoint(s), 23 shard(s) | 100 |
| `F4_newline_32k` | rift/<repo-id>/projection.db 696320 B; rift/canopy/ui-state.db 90112 B | present | — | 100 |
| `F5_newline_40k` | rift/<repo-id>/projection.db 696320 B; rift/canopy/ui-state.db 90112 B | present | 23 checkpoint(s), 24 shard(s) | 100 |

**More than one projection under a single arm's cache.** That is the shape of the defect this table exists for: the number may be about a store rift never opened.

## Phases

From one extra **untimed** commit per arm with `RIFT_HOOK_TIMING=1`, straight out of the shipped binary. Untimed because the timer takes clock readings the shipped default never takes.

**Read the `exclusive` column, and never sum `recorded`.** The timer records on completion, so a nested phase prints before its parent and the parent's figure already contains it: `predict_id` INCLUDES `redact`. `recorded` is what the binary printed; `exclusive` has every declared child subtracted, so those values are disjoint and may be added. Summing `recorded` would double-count the redaction — the one quantity the opted-in arms exist to separate.

| arm | hook | phase | recorded µs | exclusive µs |
|---|---|---|---|---|
| `B1_live` | post-commit | discover | 33 | 33 |
| `B1_live` | post-commit | enqueue | 682 | 682 |
| `B1_live` | post-commit | gix_open | 1251 | 1251 |
| `B1_live` | post-commit | head_hex | 853 | 853 |
| `B1_live` | post-commit | live_sessions | 1721 | 1721 |
| `B1_live` | post-commit | spawn_worker | 1817 | 1817 |
| `B1_live` | post-commit | total | 6637 | 6637 |
| `B1_live` | prepare-commit-msg | attribution_preview | 619 | 619 |
| `B1_live` | prepare-commit-msg | discover | 33 | 33 |
| `B1_live` | prepare-commit-msg | live_sessions | 1112 | 1112 |
| `B1_live` | prepare-commit-msg | open_projection | 2796 | 2796 |
| `B1_live` | prepare-commit-msg | predict_id | 17829 | 78 (parent, `redact` subtracted) |
| `B1_live` | prepare-commit-msg | raw_capture | 30 | 30 |
| `B1_live` | prepare-commit-msg | read_facts | 138 | 138 |
| `B1_live` | prepare-commit-msg | redact | 17751 | 17751 |
| `B1_live` | prepare-commit-msg | rule_packs | 18 | 18 |
| `B1_live` | prepare-commit-msg | tokio_build | 24 | 24 |
| `B1_live` | prepare-commit-msg | total | 23621 | 23621 |
| `D1_newline_50k` | post-commit | discover | 47 | 47 |
| `D1_newline_50k` | post-commit | enqueue | 636 | 636 |
| `D1_newline_50k` | post-commit | gix_open | 865 | 865 |
| `D1_newline_50k` | post-commit | head_hex | 479 | 479 |
| `D1_newline_50k` | post-commit | live_sessions | 1076 | 1076 |
| `D1_newline_50k` | post-commit | spawn_worker | 1555 | 1555 |
| `D1_newline_50k` | post-commit | total | 4984 | 4984 |
| `D1_newline_50k` | prepare-commit-msg | attribution_preview | 436 | 436 |
| `D1_newline_50k` | prepare-commit-msg | discover | 48 | 48 |
| `D1_newline_50k` | prepare-commit-msg | live_sessions | 1834 | 1834 |
| `D1_newline_50k` | prepare-commit-msg | open_projection | 2956 | 2956 |
| `D1_newline_50k` | prepare-commit-msg | predict_id | 27410 | 590 (parent, `redact` subtracted) |
| `D1_newline_50k` | prepare-commit-msg | raw_capture | 1523 | 1523 |
| `D1_newline_50k` | prepare-commit-msg | read_facts | 141 | 141 |
| `D1_newline_50k` | prepare-commit-msg | redact | 26820 | 26820 |
| `D1_newline_50k` | prepare-commit-msg | rule_packs | 28 | 28 |
| `D1_newline_50k` | prepare-commit-msg | tokio_build | 24 | 24 |
| `D1_newline_50k` | prepare-commit-msg | total | 35264 | 35264 |
| `F1_newline_8k` | post-commit | discover | 38 | 38 |
| `F1_newline_8k` | post-commit | enqueue | 609 | 609 |
| `F1_newline_8k` | post-commit | gix_open | 1052 | 1052 |
| `F1_newline_8k` | post-commit | head_hex | 2307 | 2307 |
| `F1_newline_8k` | post-commit | live_sessions | 1683 | 1683 |
| `F1_newline_8k` | post-commit | spawn_worker | 1768 | 1768 |
| `F1_newline_8k` | post-commit | total | 7724 | 7724 |
| `F1_newline_8k` | prepare-commit-msg | attribution_preview | 600 | 600 |
| `F1_newline_8k` | prepare-commit-msg | discover | 58 | 58 |
| `F1_newline_8k` | prepare-commit-msg | live_sessions | 2391 | 2391 |
| `F1_newline_8k` | prepare-commit-msg | open_projection | 4459 | 4459 |
| `F1_newline_8k` | prepare-commit-msg | predict_id | 22329 | 154 (parent, `redact` subtracted) |
| `F1_newline_8k` | prepare-commit-msg | raw_capture | 1057 | 1057 |
| `F1_newline_8k` | prepare-commit-msg | read_facts | 214 | 214 |
| `F1_newline_8k` | prepare-commit-msg | redact | 22175 | 22175 |
| `F1_newline_8k` | prepare-commit-msg | rule_packs | 36 | 36 |
| `F1_newline_8k` | prepare-commit-msg | tokio_build | 31 | 31 |
| `F1_newline_8k` | prepare-commit-msg | total | 32156 | 32156 |
| `F2_newline_16k` | post-commit | discover | 35 | 35 |
| `F2_newline_16k` | post-commit | enqueue | 744 | 744 |
| `F2_newline_16k` | post-commit | gix_open | 916 | 916 |
| `F2_newline_16k` | post-commit | head_hex | 625 | 625 |
| `F2_newline_16k` | post-commit | live_sessions | 2010 | 2010 |
| `F2_newline_16k` | post-commit | spawn_worker | 1881 | 1881 |
| `F2_newline_16k` | post-commit | total | 6530 | 6530 |
| `F2_newline_16k` | prepare-commit-msg | attribution_preview | 594 | 594 |
| `F2_newline_16k` | prepare-commit-msg | discover | 60 | 60 |
| `F2_newline_16k` | prepare-commit-msg | live_sessions | 1992 | 1992 |
| `F2_newline_16k` | prepare-commit-msg | open_projection | 5232 | 5232 |
| `F2_newline_16k` | prepare-commit-msg | predict_id | 29683 | 368 (parent, `redact` subtracted) |
| `F2_newline_16k` | prepare-commit-msg | raw_capture | 1546 | 1546 |
| `F2_newline_16k` | prepare-commit-msg | read_facts | 207 | 207 |
| `F2_newline_16k` | prepare-commit-msg | redact | 29315 | 29315 |
| `F2_newline_16k` | prepare-commit-msg | rule_packs | 37 | 37 |
| `F2_newline_16k` | prepare-commit-msg | tokio_build | 32 | 32 |
| `F2_newline_16k` | prepare-commit-msg | total | 40480 | 40480 |
| `F3_newline_24k` | post-commit | discover | 38 | 38 |
| `F3_newline_24k` | post-commit | enqueue | 482 | 482 |
| `F3_newline_24k` | post-commit | gix_open | 1034 | 1034 |
| `F3_newline_24k` | post-commit | head_hex | 460 | 460 |
| `F3_newline_24k` | post-commit | live_sessions | 1094 | 1094 |
| `F3_newline_24k` | post-commit | spawn_worker | 1660 | 1660 |
| `F3_newline_24k` | post-commit | total | 5018 | 5018 |
| `F3_newline_24k` | prepare-commit-msg | attribution_preview | 911 | 911 |
| `F3_newline_24k` | prepare-commit-msg | discover | 36 | 36 |
| `F3_newline_24k` | prepare-commit-msg | live_sessions | 990 | 990 |
| `F3_newline_24k` | prepare-commit-msg | open_projection | 2715 | 2715 |
| `F3_newline_24k` | prepare-commit-msg | predict_id | 20470 | 569 (parent, `redact` subtracted) |
| `F3_newline_24k` | prepare-commit-msg | raw_capture | 1097 | 1097 |
| `F3_newline_24k` | prepare-commit-msg | read_facts | 133 | 133 |
| `F3_newline_24k` | prepare-commit-msg | redact | 19901 | 19901 |
| `F3_newline_24k` | prepare-commit-msg | rule_packs | 29 | 29 |
| `F3_newline_24k` | prepare-commit-msg | tokio_build | 25 | 25 |
| `F3_newline_24k` | prepare-commit-msg | total | 27515 | 27515 |
| `F4_newline_32k` | post-commit | discover | 47 | 47 |
| `F4_newline_32k` | post-commit | enqueue | 709 | 709 |
| `F4_newline_32k` | post-commit | gix_open | 1399 | 1399 |
| `F4_newline_32k` | post-commit | head_hex | 683 | 683 |
| `F4_newline_32k` | post-commit | live_sessions | 1545 | 1545 |
| `F4_newline_32k` | post-commit | spawn_worker | 1955 | 1955 |
| `F4_newline_32k` | post-commit | total | 6743 | 6743 |
| `F4_newline_32k` | prepare-commit-msg | attribution_preview | 430 | 430 |
| `F4_newline_32k` | prepare-commit-msg | discover | 41 | 41 |
| `F4_newline_32k` | prepare-commit-msg | live_sessions | 1131 | 1131 |
| `F4_newline_32k` | prepare-commit-msg | open_projection | 2748 | 2748 |
| `F4_newline_32k` | prepare-commit-msg | predict_id | 25868 | 394 (parent, `redact` subtracted) |
| `F4_newline_32k` | prepare-commit-msg | raw_capture | 1124 | 1124 |
| `F4_newline_32k` | prepare-commit-msg | read_facts | 149 | 149 |
| `F4_newline_32k` | prepare-commit-msg | redact | 25474 | 25474 |
| `F4_newline_32k` | prepare-commit-msg | rule_packs | 27 | 27 |
| `F4_newline_32k` | prepare-commit-msg | tokio_build | 24 | 24 |
| `F4_newline_32k` | prepare-commit-msg | total | 32596 | 32596 |
| `F5_newline_40k` | post-commit | discover | 55 | 55 |
| `F5_newline_40k` | post-commit | enqueue | 758 | 758 |
| `F5_newline_40k` | post-commit | gix_open | 1425 | 1425 |
| `F5_newline_40k` | post-commit | head_hex | 654 | 654 |
| `F5_newline_40k` | post-commit | live_sessions | 1651 | 1651 |
| `F5_newline_40k` | post-commit | spawn_worker | 2842 | 2842 |
| `F5_newline_40k` | post-commit | total | 7725 | 7725 |
| `F5_newline_40k` | prepare-commit-msg | attribution_preview | 445 | 445 |
| `F5_newline_40k` | prepare-commit-msg | discover | 32 | 32 |
| `F5_newline_40k` | prepare-commit-msg | live_sessions | 2247 | 2247 |
| `F5_newline_40k` | prepare-commit-msg | open_projection | 4127 | 4127 |
| `F5_newline_40k` | prepare-commit-msg | predict_id | 30295 | 579 (parent, `redact` subtracted) |
| `F5_newline_40k` | prepare-commit-msg | raw_capture | 2659 | 2659 |
| `F5_newline_40k` | prepare-commit-msg | read_facts | 171 | 171 |
| `F5_newline_40k` | prepare-commit-msg | redact | 29716 | 29716 |
| `F5_newline_40k` | prepare-commit-msg | rule_packs | 47 | 47 |
| `F5_newline_40k` | prepare-commit-msg | tokio_build | 29 | 29 |
| `F5_newline_40k` | prepare-commit-msg | total | 40989 | 40989 |

### Phase audit

`total` is measured independently of the phases, so disjoint phases can never exceed it. `over-attributed` is therefore a proof that something is nested which the harness does not declare — not a rounding remark.

| arm | hook | total µs | attributed µs | unattributed µs | verdict |
|---|---|---|---|---|---|
| `B1_live` | post-commit | 6637 | 6357 | 280 | adds up |
| `B1_live` | prepare-commit-msg | 23621 | 22599 | 1022 | adds up |
| `D1_newline_50k` | post-commit | 4984 | 4658 | 326 | adds up |
| `D1_newline_50k` | prepare-commit-msg | 35264 | 34400 | 864 | adds up |
| `F1_newline_8k` | post-commit | 7724 | 7457 | 267 | adds up |
| `F1_newline_8k` | prepare-commit-msg | 32156 | 31175 | 981 | adds up |
| `F2_newline_16k` | post-commit | 6530 | 6211 | 319 | adds up |
| `F2_newline_16k` | prepare-commit-msg | 40480 | 39383 | 1097 | adds up |
| `F3_newline_24k` | post-commit | 5018 | 4768 | 250 | adds up |
| `F3_newline_24k` | prepare-commit-msg | 27515 | 26406 | 1109 | adds up |
| `F4_newline_32k` | post-commit | 6743 | 6338 | 405 | adds up |
| `F4_newline_32k` | prepare-commit-msg | 32596 | 31542 | 1054 | adds up |
| `F5_newline_40k` | post-commit | 7725 | 7385 | 340 | adds up |
| `F5_newline_40k` | prepare-commit-msg | 40989 | 40052 | 937 | adds up |
