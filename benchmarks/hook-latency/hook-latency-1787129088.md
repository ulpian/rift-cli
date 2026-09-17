# Hook latency — git-latency sweep

- harness `0.0.1`, artifact schema 1, generated 1787129088
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
| `A_bare` | 47.39 ms | 44.35 ms | — | 1.00× | control |
| `D1_newline_50k` | 163.02 ms | 123.64 ms | 115.63 ms | 3.44× | **BREACH** |
| `E1_newline_64k` | 159.88 ms | 129.94 ms | 112.49 ms | 3.37× | **BREACH** |
| `E2_newline_96k` | 163.39 ms | 132.43 ms | 115.99 ms | 3.45× | **BREACH** |
| `E3_newline_128k` | 159.72 ms | 133.32 ms | 112.33 ms | 3.37× | **BREACH** |
| `E4_newline_192k` | 168.10 ms | 139.76 ms | 120.71 ms | 3.55× | **BREACH** |
| `E5_newline_250k_8f` | 205.36 ms | 172.53 ms | 157.97 ms | 4.33× | **BREACH** |

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
| `D1_newline_50k` | 51200 | 102592 | 2.00× | 115.62 ms | 45.01 ms | 70.61 ms | 30.24 ms | 0.86 ms | **BREACH** |
| `E1_newline_64k` | 65536 | 131264 | 2.00× | 112.48 ms | 42.41 ms | 70.08 ms | 28.35 ms | 0.73 ms | **BREACH** |
| `E2_newline_96k` | 98304 | 196800 | 2.00× | 115.99 ms | 49.90 ms | 66.09 ms | 32.20 ms | 1.58 ms | **BREACH** |
| `E3_newline_128k` | 131072 | 262336 | 2.00× | 112.33 ms | 56.11 ms | 56.21 ms | 35.54 ms | 1.99 ms | **BREACH** |
| `E4_newline_192k` | 196608 | 393408 | 2.00× | 120.71 ms | 78.58 ms | 42.13 ms | 52.64 ms | 3.29 ms | **BREACH** |
| `E5_newline_250k_8f` | 256000 | 512192 | 2.00× | 157.97 ms | 67.32 ms | 90.65 ms | 44.91 ms | 2.47 ms | **BREACH** |

`outside bodies` is the floor a Rust change cannot move: three `/bin/sh` invocations and three execs that git requires, plus git's own cost of running them. If the budget ever has to halve, that column is what has to be attacked, and it is not in Rust code.

## Oracles — what each arm exercised

Reported for every arm including the control, because a check that only ever confirms is not a check: the idle arm's **absent** trailer is what makes the live arm's present one mean something.

| arm | projections | trailer | verify | events |
|---|---|---|---|---|
| `A_bare` | none | absent | — | — |
| `D1_newline_50k` | rift/<repo-id>/projection.db 688128 B; rift/canopy/ui-state.db 90112 B | present | 23 checkpoint(s), 24 shard(s) | 100 |
| `E1_newline_64k` | rift/<repo-id>/projection.db 679936 B; rift/canopy/ui-state.db 90112 B | present | — | 100 |
| `E2_newline_96k` | rift/<repo-id>/projection.db 679936 B; rift/canopy/ui-state.db 90112 B | present | 22 checkpoint(s), 23 shard(s) | 100 |
| `E3_newline_128k` | rift/<repo-id>/projection.db 679936 B; rift/canopy/ui-state.db 90112 B | present | 22 checkpoint(s), 22 shard(s) | 100 |
| `E4_newline_192k` | rift/<repo-id>/projection.db 671744 B; rift/canopy/ui-state.db 90112 B | present | — | 100 |
| `E5_newline_250k_8f` | rift/<repo-id>/projection.db 671744 B; rift/canopy/ui-state.db 90112 B | present | 21 checkpoint(s), 21 shard(s) | 100 |

**More than one projection under a single arm's cache.** That is the shape of the defect this table exists for: the number may be about a store rift never opened.

## Phases

From one extra **untimed** commit per arm with `RIFT_HOOK_TIMING=1`, straight out of the shipped binary. Untimed because the timer takes clock readings the shipped default never takes.

**Read the `exclusive` column, and never sum `recorded`.** The timer records on completion, so a nested phase prints before its parent and the parent's figure already contains it: `predict_id` INCLUDES `redact`. `recorded` is what the binary printed; `exclusive` has every declared child subtracted, so those values are disjoint and may be added. Summing `recorded` would double-count the redaction — the one quantity the opted-in arms exist to separate.

| arm | hook | phase | recorded µs | exclusive µs |
|---|---|---|---|---|
| `D1_newline_50k` | post-commit | discover | 36 | 36 |
| `D1_newline_50k` | post-commit | enqueue | 447 | 447 |
| `D1_newline_50k` | post-commit | gix_open | 923 | 923 |
| `D1_newline_50k` | post-commit | head_hex | 430 | 430 |
| `D1_newline_50k` | post-commit | live_sessions | 1020 | 1020 |
| `D1_newline_50k` | post-commit | spawn_worker | 1270 | 1270 |
| `D1_newline_50k` | post-commit | total | 4366 | 4366 |
| `D1_newline_50k` | prepare-commit-msg | attribution_preview | 628 | 628 |
| `D1_newline_50k` | prepare-commit-msg | discover | 30 | 30 |
| `D1_newline_50k` | prepare-commit-msg | live_sessions | 1677 | 1677 |
| `D1_newline_50k` | prepare-commit-msg | open_projection | 4063 | 4063 |
| `D1_newline_50k` | prepare-commit-msg | predict_id | 31097 | 859 (parent, `redact` subtracted) |
| `D1_newline_50k` | prepare-commit-msg | raw_capture | 1934 | 1934 |
| `D1_newline_50k` | prepare-commit-msg | read_facts | 138 | 138 |
| `D1_newline_50k` | prepare-commit-msg | redact | 30238 | 30238 |
| `D1_newline_50k` | prepare-commit-msg | rule_packs | 33 | 33 |
| `D1_newline_50k` | prepare-commit-msg | tokio_build | 23 | 23 |
| `D1_newline_50k` | prepare-commit-msg | total | 40646 | 40646 |
| `E1_newline_64k` | post-commit | discover | 32 | 32 |
| `E1_newline_64k` | post-commit | enqueue | 565 | 565 |
| `E1_newline_64k` | post-commit | gix_open | 932 | 932 |
| `E1_newline_64k` | post-commit | head_hex | 694 | 694 |
| `E1_newline_64k` | post-commit | live_sessions | 1077 | 1077 |
| `E1_newline_64k` | post-commit | spawn_worker | 1302 | 1302 |
| `E1_newline_64k` | post-commit | total | 4868 | 4868 |
| `E1_newline_64k` | prepare-commit-msg | attribution_preview | 619 | 619 |
| `E1_newline_64k` | prepare-commit-msg | discover | 30 | 30 |
| `E1_newline_64k` | prepare-commit-msg | live_sessions | 1336 | 1336 |
| `E1_newline_64k` | prepare-commit-msg | open_projection | 3180 | 3180 |
| `E1_newline_64k` | prepare-commit-msg | predict_id | 29080 | 730 (parent, `redact` subtracted) |
| `E1_newline_64k` | prepare-commit-msg | raw_capture | 2038 | 2038 |
| `E1_newline_64k` | prepare-commit-msg | read_facts | 205 | 205 |
| `E1_newline_64k` | prepare-commit-msg | redact | 28350 | 28350 |
| `E1_newline_64k` | prepare-commit-msg | rule_packs | 38 | 38 |
| `E1_newline_64k` | prepare-commit-msg | tokio_build | 29 | 29 |
| `E1_newline_64k` | prepare-commit-msg | total | 37541 | 37541 |
| `E2_newline_96k` | post-commit | discover | 34 | 34 |
| `E2_newline_96k` | post-commit | enqueue | 716 | 716 |
| `E2_newline_96k` | post-commit | gix_open | 782 | 782 |
| `E2_newline_96k` | post-commit | head_hex | 439 | 439 |
| `E2_newline_96k` | post-commit | live_sessions | 990 | 990 |
| `E2_newline_96k` | post-commit | spawn_worker | 4335 | 4335 |
| `E2_newline_96k` | post-commit | total | 7620 | 7620 |
| `E2_newline_96k` | prepare-commit-msg | attribution_preview | 638 | 638 |
| `E2_newline_96k` | prepare-commit-msg | discover | 38 | 38 |
| `E2_newline_96k` | prepare-commit-msg | live_sessions | 984 | 984 |
| `E2_newline_96k` | prepare-commit-msg | open_projection | 2750 | 2750 |
| `E2_newline_96k` | prepare-commit-msg | predict_id | 33776 | 1576 (parent, `redact` subtracted) |
| `E2_newline_96k` | prepare-commit-msg | raw_capture | 2363 | 2363 |
| `E2_newline_96k` | prepare-commit-msg | read_facts | 147 | 147 |
| `E2_newline_96k` | prepare-commit-msg | redact | 32200 | 32200 |
| `E2_newline_96k` | prepare-commit-msg | rule_packs | 32 | 32 |
| `E2_newline_96k` | prepare-commit-msg | tokio_build | 24 | 24 |
| `E2_newline_96k` | prepare-commit-msg | total | 42279 | 42279 |
| `E3_newline_128k` | post-commit | discover | 33 | 33 |
| `E3_newline_128k` | post-commit | enqueue | 581 | 581 |
| `E3_newline_128k` | post-commit | gix_open | 1178 | 1178 |
| `E3_newline_128k` | post-commit | head_hex | 654 | 654 |
| `E3_newline_128k` | post-commit | live_sessions | 1375 | 1375 |
| `E3_newline_128k` | post-commit | spawn_worker | 1883 | 1883 |
| `E3_newline_128k` | post-commit | total | 6013 | 6013 |
| `E3_newline_128k` | prepare-commit-msg | attribution_preview | 416 | 416 |
| `E3_newline_128k` | prepare-commit-msg | discover | 50 | 50 |
| `E3_newline_128k` | prepare-commit-msg | live_sessions | 1170 | 1170 |
| `E3_newline_128k` | prepare-commit-msg | open_projection | 3791 | 3791 |
| `E3_newline_128k` | prepare-commit-msg | predict_id | 37532 | 1988 (parent, `redact` subtracted) |
| `E3_newline_128k` | prepare-commit-msg | raw_capture | 5771 | 5771 |
| `E3_newline_128k` | prepare-commit-msg | read_facts | 226 | 226 |
| `E3_newline_128k` | prepare-commit-msg | redact | 35544 | 35544 |
| `E3_newline_128k` | prepare-commit-msg | rule_packs | 46 | 46 |
| `E3_newline_128k` | prepare-commit-msg | tokio_build | 31 | 31 |
| `E3_newline_128k` | prepare-commit-msg | total | 50100 | 50100 |
| `E4_newline_192k` | post-commit | discover | 40 | 40 |
| `E4_newline_192k` | post-commit | enqueue | 720 | 720 |
| `E4_newline_192k` | post-commit | gix_open | 1501 | 1501 |
| `E4_newline_192k` | post-commit | head_hex | 523 | 523 |
| `E4_newline_192k` | post-commit | live_sessions | 1337 | 1337 |
| `E4_newline_192k` | post-commit | spawn_worker | 1793 | 1793 |
| `E4_newline_192k` | post-commit | total | 6235 | 6235 |
| `E4_newline_192k` | prepare-commit-msg | attribution_preview | 571 | 571 |
| `E4_newline_192k` | prepare-commit-msg | discover | 45 | 45 |
| `E4_newline_192k` | prepare-commit-msg | live_sessions | 2249 | 2249 |
| `E4_newline_192k` | prepare-commit-msg | open_projection | 4886 | 4886 |
| `E4_newline_192k` | prepare-commit-msg | predict_id | 55929 | 3288 (parent, `redact` subtracted) |
| `E4_newline_192k` | prepare-commit-msg | raw_capture | 6810 | 6810 |
| `E4_newline_192k` | prepare-commit-msg | read_facts | 205 | 205 |
| `E4_newline_192k` | prepare-commit-msg | redact | 52641 | 52641 |
| `E4_newline_192k` | prepare-commit-msg | rule_packs | 47 | 47 |
| `E4_newline_192k` | prepare-commit-msg | tokio_build | 28 | 28 |
| `E4_newline_192k` | prepare-commit-msg | total | 72346 | 72346 |
| `E5_newline_250k_8f` | post-commit | discover | 44 | 44 |
| `E5_newline_250k_8f` | post-commit | enqueue | 517 | 517 |
| `E5_newline_250k_8f` | post-commit | gix_open | 1221 | 1221 |
| `E5_newline_250k_8f` | post-commit | head_hex | 808 | 808 |
| `E5_newline_250k_8f` | post-commit | live_sessions | 1240 | 1240 |
| `E5_newline_250k_8f` | post-commit | spawn_worker | 1530 | 1530 |
| `E5_newline_250k_8f` | post-commit | total | 5638 | 5638 |
| `E5_newline_250k_8f` | prepare-commit-msg | attribution_preview | 368 | 368 |
| `E5_newline_250k_8f` | prepare-commit-msg | discover | 45 | 45 |
| `E5_newline_250k_8f` | prepare-commit-msg | live_sessions | 1716 | 1716 |
| `E5_newline_250k_8f` | prepare-commit-msg | open_projection | 3631 | 3631 |
| `E5_newline_250k_8f` | prepare-commit-msg | predict_id | 47381 | 2466 (parent, `redact` subtracted) |
| `E5_newline_250k_8f` | prepare-commit-msg | raw_capture | 6933 | 6933 |
| `E5_newline_250k_8f` | prepare-commit-msg | read_facts | 178 | 178 |
| `E5_newline_250k_8f` | prepare-commit-msg | redact | 44915 | 44915 |
| `E5_newline_250k_8f` | prepare-commit-msg | rule_packs | 34 | 34 |
| `E5_newline_250k_8f` | prepare-commit-msg | tokio_build | 28 | 28 |
| `E5_newline_250k_8f` | prepare-commit-msg | total | 61680 | 61680 |

### Phase audit

`total` is measured independently of the phases, so disjoint phases can never exceed it. `over-attributed` is therefore a proof that something is nested which the harness does not declare — not a rounding remark.

| arm | hook | total µs | attributed µs | unattributed µs | verdict |
|---|---|---|---|---|---|
| `D1_newline_50k` | post-commit | 4366 | 4126 | 240 | adds up |
| `D1_newline_50k` | prepare-commit-msg | 40646 | 39623 | 1023 | adds up |
| `E1_newline_64k` | post-commit | 4868 | 4602 | 266 | adds up |
| `E1_newline_64k` | prepare-commit-msg | 37541 | 36555 | 986 | adds up |
| `E2_newline_96k` | post-commit | 7620 | 7296 | 324 | adds up |
| `E2_newline_96k` | prepare-commit-msg | 42279 | 40752 | 1527 | adds up |
| `E3_newline_128k` | post-commit | 6013 | 5704 | 309 | adds up |
| `E3_newline_128k` | prepare-commit-msg | 50100 | 49033 | 1067 | adds up |
| `E4_newline_192k` | post-commit | 6235 | 5914 | 321 | adds up |
| `E4_newline_192k` | prepare-commit-msg | 72346 | 70770 | 1576 | adds up |
| `E5_newline_250k_8f` | post-commit | 5638 | 5360 | 278 | adds up |
| `E5_newline_250k_8f` | prepare-commit-msg | 61680 | 60314 | 1366 | adds up |
