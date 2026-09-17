# Hook latency — git-latency sweep

- harness `0.0.1`, artifact schema 1, generated 1787129241
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
| `A_bare` | 55.42 ms | 49.43 ms | — | 1.00× | control |
| `D1_newline_50k` | 175.15 ms | 149.70 ms | 119.72 ms | 3.16× | **BREACH** |
| `E1_newline_64k` | 169.81 ms | 142.20 ms | 114.39 ms | 3.06× | **BREACH** |
| `E2_newline_96k` | 169.80 ms | 145.52 ms | 114.37 ms | 3.06× | **BREACH** |
| `E3_newline_128k` | 233.66 ms | 166.03 ms | 178.24 ms | 4.22× | **BREACH** |
| `E4_newline_192k` | 187.83 ms | 156.43 ms | 132.41 ms | 3.39× | **BREACH** |
| `E5_newline_250k_8f` | 196.99 ms | 169.73 ms | 141.57 ms | 3.55× | **BREACH** |

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
| `D1_newline_50k` | 51200 | 102592 | 2.00× | 119.72 ms | 50.34 ms | 69.38 ms | 30.28 ms | 0.88 ms | **BREACH** |
| `E1_newline_64k` | 65536 | 131264 | 2.00× | 114.39 ms | 44.85 ms | 69.54 ms | 28.76 ms | 0.78 ms | **BREACH** |
| `E2_newline_96k` | 98304 | 196800 | 2.00× | 114.37 ms | 54.07 ms | 60.30 ms | 36.11 ms | 1.23 ms | **BREACH** |
| `E3_newline_128k` | 131072 | 262336 | 2.00× | 178.24 ms | 68.61 ms | 109.63 ms | 44.39 ms | 2.08 ms | **BREACH** |
| `E4_newline_192k` | 196608 | 393408 | 2.00× | 132.41 ms | 79.06 ms | 53.35 ms | 51.90 ms | 3.08 ms | **BREACH** |
| `E5_newline_250k_8f` | 256000 | 512192 | 2.00× | 141.57 ms | 85.05 ms | 56.52 ms | 54.97 ms | 3.98 ms | **BREACH** |

`outside bodies` is the floor a Rust change cannot move: three `/bin/sh` invocations and three execs that git requires, plus git's own cost of running them. If the budget ever has to halve, that column is what has to be attacked, and it is not in Rust code.

## Oracles — what each arm exercised

Reported for every arm including the control, because a check that only ever confirms is not a check: the idle arm's **absent** trailer is what makes the live arm's present one mean something.

| arm | projections | trailer | verify | events |
|---|---|---|---|---|
| `A_bare` | none | absent | — | — |
| `D1_newline_50k` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 688128 B | present | 23 checkpoint(s), 23 shard(s) | 100 |
| `E1_newline_64k` | rift/<repo-id>/projection.db 688128 B; rift/canopy/ui-state.db 90112 B | present | 23 checkpoint(s), 24 shard(s) | 100 |
| `E2_newline_96k` | rift/<repo-id>/projection.db 679936 B; rift/canopy/ui-state.db 90112 B | present | 22 checkpoint(s), 23 shard(s) | 100 |
| `E3_newline_128k` | rift/<repo-id>/projection.db 679936 B; rift/canopy/ui-state.db 90112 B | present | 22 checkpoint(s), 22 shard(s) | 100 |
| `E4_newline_192k` | rift/<repo-id>/projection.db 671744 B; rift/canopy/ui-state.db 90112 B | present | 21 checkpoint(s), 21 shard(s) | 100 |
| `E5_newline_250k_8f` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 671744 B | present | 21 checkpoint(s), 21 shard(s) | 100 |

**More than one projection under a single arm's cache.** That is the shape of the defect this table exists for: the number may be about a store rift never opened.

## Phases

From one extra **untimed** commit per arm with `RIFT_HOOK_TIMING=1`, straight out of the shipped binary. Untimed because the timer takes clock readings the shipped default never takes.

**Read the `exclusive` column, and never sum `recorded`.** The timer records on completion, so a nested phase prints before its parent and the parent's figure already contains it: `predict_id` INCLUDES `redact`. `recorded` is what the binary printed; `exclusive` has every declared child subtracted, so those values are disjoint and may be added. Summing `recorded` would double-count the redaction — the one quantity the opted-in arms exist to separate.

| arm | hook | phase | recorded µs | exclusive µs |
|---|---|---|---|---|
| `D1_newline_50k` | post-commit | discover | 65 | 65 |
| `D1_newline_50k` | post-commit | enqueue | 656 | 656 |
| `D1_newline_50k` | post-commit | gix_open | 1188 | 1188 |
| `D1_newline_50k` | post-commit | head_hex | 648 | 648 |
| `D1_newline_50k` | post-commit | live_sessions | 1782 | 1782 |
| `D1_newline_50k` | post-commit | spawn_worker | 3053 | 3053 |
| `D1_newline_50k` | post-commit | total | 7731 | 7731 |
| `D1_newline_50k` | prepare-commit-msg | attribution_preview | 621 | 621 |
| `D1_newline_50k` | prepare-commit-msg | discover | 52 | 52 |
| `D1_newline_50k` | prepare-commit-msg | live_sessions | 2500 | 2500 |
| `D1_newline_50k` | prepare-commit-msg | open_projection | 4352 | 4352 |
| `D1_newline_50k` | prepare-commit-msg | predict_id | 31161 | 879 (parent, `redact` subtracted) |
| `D1_newline_50k` | prepare-commit-msg | raw_capture | 2573 | 2573 |
| `D1_newline_50k` | prepare-commit-msg | read_facts | 217 | 217 |
| `D1_newline_50k` | prepare-commit-msg | redact | 30282 | 30282 |
| `D1_newline_50k` | prepare-commit-msg | rule_packs | 41 | 41 |
| `D1_newline_50k` | prepare-commit-msg | tokio_build | 28 | 28 |
| `D1_newline_50k` | prepare-commit-msg | total | 42610 | 42610 |
| `E1_newline_64k` | post-commit | discover | 56 | 56 |
| `E1_newline_64k` | post-commit | enqueue | 762 | 762 |
| `E1_newline_64k` | post-commit | gix_open | 980 | 980 |
| `E1_newline_64k` | post-commit | head_hex | 778 | 778 |
| `E1_newline_64k` | post-commit | live_sessions | 1833 | 1833 |
| `E1_newline_64k` | post-commit | spawn_worker | 2009 | 2009 |
| `E1_newline_64k` | post-commit | total | 6769 | 6769 |
| `E1_newline_64k` | prepare-commit-msg | attribution_preview | 481 | 481 |
| `E1_newline_64k` | prepare-commit-msg | discover | 42 | 42 |
| `E1_newline_64k` | prepare-commit-msg | live_sessions | 1167 | 1167 |
| `E1_newline_64k` | prepare-commit-msg | open_projection | 2860 | 2860 |
| `E1_newline_64k` | prepare-commit-msg | predict_id | 29540 | 778 (parent, `redact` subtracted) |
| `E1_newline_64k` | prepare-commit-msg | raw_capture | 2896 | 2896 |
| `E1_newline_64k` | prepare-commit-msg | read_facts | 157 | 157 |
| `E1_newline_64k` | prepare-commit-msg | redact | 28762 | 28762 |
| `E1_newline_64k` | prepare-commit-msg | rule_packs | 72 | 72 |
| `E1_newline_64k` | prepare-commit-msg | tokio_build | 25 | 25 |
| `E1_newline_64k` | prepare-commit-msg | total | 38082 | 38082 |
| `E2_newline_96k` | post-commit | discover | 33 | 33 |
| `E2_newline_96k` | post-commit | enqueue | 527 | 527 |
| `E2_newline_96k` | post-commit | gix_open | 791 | 791 |
| `E2_newline_96k` | post-commit | head_hex | 366 | 366 |
| `E2_newline_96k` | post-commit | live_sessions | 1038 | 1038 |
| `E2_newline_96k` | post-commit | spawn_worker | 1967 | 1967 |
| `E2_newline_96k` | post-commit | total | 5028 | 5028 |
| `E2_newline_96k` | prepare-commit-msg | attribution_preview | 396 | 396 |
| `E2_newline_96k` | prepare-commit-msg | discover | 46 | 46 |
| `E2_newline_96k` | prepare-commit-msg | live_sessions | 1776 | 1776 |
| `E2_newline_96k` | prepare-commit-msg | open_projection | 4724 | 4724 |
| `E2_newline_96k` | prepare-commit-msg | predict_id | 37342 | 1233 (parent, `redact` subtracted) |
| `E2_newline_96k` | prepare-commit-msg | raw_capture | 3529 | 3529 |
| `E2_newline_96k` | prepare-commit-msg | read_facts | 197 | 197 |
| `E2_newline_96k` | prepare-commit-msg | redact | 36109 | 36109 |
| `E2_newline_96k` | prepare-commit-msg | rule_packs | 29 | 29 |
| `E2_newline_96k` | prepare-commit-msg | tokio_build | 29 | 29 |
| `E2_newline_96k` | prepare-commit-msg | total | 49040 | 49040 |
| `E3_newline_128k` | post-commit | discover | 47 | 47 |
| `E3_newline_128k` | post-commit | enqueue | 691 | 691 |
| `E3_newline_128k` | post-commit | gix_open | 1233 | 1233 |
| `E3_newline_128k` | post-commit | head_hex | 711 | 711 |
| `E3_newline_128k` | post-commit | live_sessions | 2570 | 2570 |
| `E3_newline_128k` | post-commit | spawn_worker | 1654 | 1654 |
| `E3_newline_128k` | post-commit | total | 7243 | 7243 |
| `E3_newline_128k` | prepare-commit-msg | attribution_preview | 584 | 584 |
| `E3_newline_128k` | prepare-commit-msg | discover | 69 | 69 |
| `E3_newline_128k` | prepare-commit-msg | live_sessions | 1762 | 1762 |
| `E3_newline_128k` | prepare-commit-msg | open_projection | 5289 | 5289 |
| `E3_newline_128k` | prepare-commit-msg | predict_id | 46465 | 2078 (parent, `redact` subtracted) |
| `E3_newline_128k` | prepare-commit-msg | raw_capture | 5204 | 5204 |
| `E3_newline_128k` | prepare-commit-msg | read_facts | 202 | 202 |
| `E3_newline_128k` | prepare-commit-msg | redact | 44387 | 44387 |
| `E3_newline_128k` | prepare-commit-msg | rule_packs | 59 | 59 |
| `E3_newline_128k` | prepare-commit-msg | tokio_build | 30 | 30 |
| `E3_newline_128k` | prepare-commit-msg | total | 61362 | 61362 |
| `E4_newline_192k` | post-commit | discover | 46 | 46 |
| `E4_newline_192k` | post-commit | enqueue | 709 | 709 |
| `E4_newline_192k` | post-commit | gix_open | 1116 | 1116 |
| `E4_newline_192k` | post-commit | head_hex | 815 | 815 |
| `E4_newline_192k` | post-commit | live_sessions | 1902 | 1902 |
| `E4_newline_192k` | post-commit | spawn_worker | 2860 | 2860 |
| `E4_newline_192k` | post-commit | total | 7786 | 7786 |
| `E4_newline_192k` | prepare-commit-msg | attribution_preview | 717 | 717 |
| `E4_newline_192k` | prepare-commit-msg | discover | 45 | 45 |
| `E4_newline_192k` | prepare-commit-msg | live_sessions | 1655 | 1655 |
| `E4_newline_192k` | prepare-commit-msg | open_projection | 4990 | 4990 |
| `E4_newline_192k` | prepare-commit-msg | predict_id | 54984 | 3081 (parent, `redact` subtracted) |
| `E4_newline_192k` | prepare-commit-msg | raw_capture | 7308 | 7308 |
| `E4_newline_192k` | prepare-commit-msg | read_facts | 192 | 192 |
| `E4_newline_192k` | prepare-commit-msg | redact | 51903 | 51903 |
| `E4_newline_192k` | prepare-commit-msg | rule_packs | 64 | 64 |
| `E4_newline_192k` | prepare-commit-msg | tokio_build | 29 | 29 |
| `E4_newline_192k` | prepare-commit-msg | total | 71272 | 71272 |
| `E5_newline_250k_8f` | post-commit | discover | 34 | 34 |
| `E5_newline_250k_8f` | post-commit | enqueue | 774 | 774 |
| `E5_newline_250k_8f` | post-commit | gix_open | 1216 | 1216 |
| `E5_newline_250k_8f` | post-commit | head_hex | 754 | 754 |
| `E5_newline_250k_8f` | post-commit | live_sessions | 1801 | 1801 |
| `E5_newline_250k_8f` | post-commit | spawn_worker | 2907 | 2907 |
| `E5_newline_250k_8f` | post-commit | total | 7802 | 7802 |
| `E5_newline_250k_8f` | prepare-commit-msg | attribution_preview | 658 | 658 |
| `E5_newline_250k_8f` | prepare-commit-msg | discover | 50 | 50 |
| `E5_newline_250k_8f` | prepare-commit-msg | live_sessions | 1924 | 1924 |
| `E5_newline_250k_8f` | prepare-commit-msg | open_projection | 4529 | 4529 |
| `E5_newline_250k_8f` | prepare-commit-msg | predict_id | 58951 | 3979 (parent, `redact` subtracted) |
| `E5_newline_250k_8f` | prepare-commit-msg | raw_capture | 9283 | 9283 |
| `E5_newline_250k_8f` | prepare-commit-msg | read_facts | 200 | 200 |
| `E5_newline_250k_8f` | prepare-commit-msg | redact | 54972 | 54972 |
| `E5_newline_250k_8f` | prepare-commit-msg | rule_packs | 38 | 38 |
| `E5_newline_250k_8f` | prepare-commit-msg | tokio_build | 31 | 31 |
| `E5_newline_250k_8f` | prepare-commit-msg | total | 77243 | 77243 |

### Phase audit

`total` is measured independently of the phases, so disjoint phases can never exceed it. `over-attributed` is therefore a proof that something is nested which the harness does not declare — not a rounding remark.

| arm | hook | total µs | attributed µs | unattributed µs | verdict |
|---|---|---|---|---|---|
| `D1_newline_50k` | post-commit | 7731 | 7392 | 339 | adds up |
| `D1_newline_50k` | prepare-commit-msg | 42610 | 41545 | 1065 | adds up |
| `E1_newline_64k` | post-commit | 6769 | 6418 | 351 | adds up |
| `E1_newline_64k` | prepare-commit-msg | 38082 | 37240 | 842 | adds up |
| `E2_newline_96k` | post-commit | 5028 | 4722 | 306 | adds up |
| `E2_newline_96k` | prepare-commit-msg | 49040 | 48068 | 972 | adds up |
| `E3_newline_128k` | post-commit | 7243 | 6906 | 337 | adds up |
| `E3_newline_128k` | prepare-commit-msg | 61362 | 59664 | 1698 | adds up |
| `E4_newline_192k` | post-commit | 7786 | 7448 | 338 | adds up |
| `E4_newline_192k` | prepare-commit-msg | 71272 | 69984 | 1288 | adds up |
| `E5_newline_250k_8f` | post-commit | 7802 | 7486 | 316 | adds up |
| `E5_newline_250k_8f` | prepare-commit-msg | 77243 | 75664 | 1579 | adds up |
