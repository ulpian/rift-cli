# Hook latency — git-latency sweep

- harness `0.0.1`, artifact schema 1, generated 1787120240
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
| `A_bare` | 40.06 ms | 35.32 ms | — | 1.00× | control |
| `B0_idle` | 95.61 ms | 81.80 ms | 55.56 ms | 2.39× | meets |
| `B1_live` | 195.44 ms | 143.82 ms | 155.38 ms | 4.88× | **BREACH** |
| `C1_include_50k` | 149.49 ms | 139.94 ms | 109.43 ms | 3.73× | **BREACH** |
| `C2_include_250k` | 203.75 ms | 187.72 ms | 163.69 ms | 5.09× | **BREACH** |
| `C3_include_1mib` | 220.06 ms | 188.70 ms | 180.01 ms | 5.49× | **BREACH** |

- `A_bare` — bare commit, no hooks installed (control)

- `B0_idle` — hooks installed, nothing opted in, no live session

- `B1_live` — hooks installed, a session live at commit time

- `C1_include_50k` — live session + `.worktreeinclude` over 8 files (51200 B read, 52696 B handed on, 1.03x)

- `C2_include_250k` — live session + `.worktreeinclude` over 40 files (256000 B read, 263480 B handed on, 1.03x)

- `C3_include_1mib` — live session + `.worktreeinclude` over 16 files (1048576 B read, 1066352 B handed on, 1.02x)

## Opted-in corpus

`MAX_TOTAL_BYTES` is 1 MiB and is charged on bytes **read**; `Walk::file` prefixes every content line with one space, so N bytes contribute up to **2N + 2** to the document the redactor is handed. Both numbers are published, because an arm described as "1 MiB" without saying which 1 MiB misstates the ceiling in one direction or the other.

| arm | files | bytes read | budget | document | factor | files read (explain) |
|---|---|---|---|---|---|---|
| `C1_include_50k` | 8 | 51200 | under | 52696 | 1.03× | — |
| `C2_include_250k` | 40 | 256000 | under | 263480 | 1.03× | — |
| `C3_include_1mib` | 16 | 1048576 | **at/over the 1 MiB read ceiling** | 1066352 | 1.02× | — |

The factor here is near 1.0 because these lines are long. The 2× is the pathological end — a file that is all newlines, where the one-space prefix is the whole line — so a full budget can hand on a document just over 2 MiB. Neither end is the other, and this table says which one each arm is.

`files read (explain)` is `rift checkpoint explain`'s **own** label, not a span count: the inclusion walk's record count is on no CLI surface today, so the harness publishes the figure the product publishes rather than relabelling it as something it has not established.

## Anatomy — the curve, the units, and the phase that owns it

**BREACH: `B1_live`, `C1_include_50k`, `C2_include_250k`, `C3_include_1mib`.** The budget is 100 ms of git latency and is HARD (set by the owner). The corpus was not adjusted in response — the arms are the sizes they were declared at.

**Magnitude and attribution come from different commits.** `git latency` is the median of the timed commits minus the control's; every phase column comes from the arm's one **extra untimed** commit with the timer on. Read the residue as an anatomy of a representative commit beside the magnitude of the median, never as an exact decomposition of one commit.

`predict_id` here is **exclusive** — `redact` subtracted — so the columns are disjoint and may be added.

| arm | bytes read | handed on | ×read | git latency | hook bodies | outside bodies | `redact` | `predict_id` less `redact` | verdict |
|---|---|---|---|---|---|---|---|---|---|
| `B0_idle` | — | — | — | 55.55 ms | 3.01 ms | 52.54 ms | 0.00 ms | 0.00 ms | meets |
| `B1_live` | — | — | — | 155.38 ms | 44.46 ms | 110.92 ms | 30.09 ms | 0.10 ms | **BREACH** |
| `C1_include_50k` | 51200 | 52696 | 1.03× | 109.43 ms | 28.61 ms | 80.81 ms | 18.43 ms | 0.45 ms | **BREACH** |
| `C2_include_250k` | 256000 | 263480 | 1.03× | 163.69 ms | 55.82 ms | 107.87 ms | 38.52 ms | 1.66 ms | **BREACH** |
| `C3_include_1mib` | 1048576 | 1066352 | 1.02× | 180.00 ms | 90.53 ms | 89.47 ms | 66.01 ms | 8.25 ms | **BREACH** |

`outside bodies` is the floor a Rust change cannot move: three `/bin/sh` invocations and three execs that git requires, plus git's own cost of running them. If the budget ever has to halve, that column is what has to be attacked, and it is not in Rust code.

## Oracles — what each arm exercised

Reported for every arm including the control, because a check that only ever confirms is not a check: the idle arm's **absent** trailer is what makes the live arm's present one mean something.

| arm | projections | trailer | verify | events |
|---|---|---|---|---|
| `A_bare` | none | absent | — | — |
| `B0_idle` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 430080 B | absent | 1 checkpoint(s), 0 shard(s) | 0 |
| `B1_live` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `C1_include_50k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `C2_include_250k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `C3_include_1mib` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 12 checkpoint(s), 13 shard(s) | 60 |

**More than one projection under a single arm's cache.** That is the shape of the defect this table exists for: the number may be about a store rift never opened.

## Phases

From one extra **untimed** commit per arm with `RIFT_HOOK_TIMING=1`, straight out of the shipped binary. Untimed because the timer takes clock readings the shipped default never takes.

**Read the `exclusive` column, and never sum `recorded`.** The timer records on completion, so a nested phase prints before its parent and the parent's figure already contains it: `predict_id` INCLUDES `redact`. `recorded` is what the binary printed; `exclusive` has every declared child subtracted, so those values are disjoint and may be added. Summing `recorded` would double-count the redaction — the one quantity the opted-in arms exist to separate.

| arm | hook | phase | recorded µs | exclusive µs |
|---|---|---|---|---|
| `B0_idle` | post-commit | discover | 27 | 27 |
| `B0_idle` | post-commit | enqueue | 393 | 393 |
| `B0_idle` | post-commit | gix_open | 659 | 659 |
| `B0_idle` | post-commit | head_hex | 370 | 370 |
| `B0_idle` | post-commit | live_sessions | 18 | 18 |
| `B0_idle` | post-commit | spawn_worker | 1176 | 1176 |
| `B0_idle` | post-commit | total | 2811 | 2811 |
| `B0_idle` | prepare-commit-msg | discover | 40 | 40 |
| `B0_idle` | prepare-commit-msg | live_sessions | 36 | 36 |
| `B0_idle` | prepare-commit-msg | total | 201 | 201 |
| `B1_live` | post-commit | discover | 40 | 40 |
| `B1_live` | post-commit | enqueue | 588 | 588 |
| `B1_live` | post-commit | gix_open | 930 | 930 |
| `B1_live` | post-commit | head_hex | 465 | 465 |
| `B1_live` | post-commit | live_sessions | 699 | 699 |
| `B1_live` | post-commit | spawn_worker | 1740 | 1740 |
| `B1_live` | post-commit | total | 4743 | 4743 |
| `B1_live` | prepare-commit-msg | attribution_preview | 793 | 793 |
| `B1_live` | prepare-commit-msg | discover | 77 | 77 |
| `B1_live` | prepare-commit-msg | live_sessions | 1519 | 1519 |
| `B1_live` | prepare-commit-msg | open_projection | 5416 | 5416 |
| `B1_live` | prepare-commit-msg | predict_id | 30193 | 104 (parent, `redact` subtracted) |
| `B1_live` | prepare-commit-msg | raw_capture | 55 | 55 |
| `B1_live` | prepare-commit-msg | read_facts | 291 | 291 |
| `B1_live` | prepare-commit-msg | redact | 30089 | 30089 |
| `B1_live` | prepare-commit-msg | rule_packs | 45 | 45 |
| `B1_live` | prepare-commit-msg | tokio_build | 36 | 36 |
| `B1_live` | prepare-commit-msg | total | 39717 | 39717 |
| `C1_include_50k` | post-commit | discover | 41 | 41 |
| `C1_include_50k` | post-commit | enqueue | 486 | 486 |
| `C1_include_50k` | post-commit | gix_open | 750 | 750 |
| `C1_include_50k` | post-commit | head_hex | 404 | 404 |
| `C1_include_50k` | post-commit | live_sessions | 657 | 657 |
| `C1_include_50k` | post-commit | spawn_worker | 1370 | 1370 |
| `C1_include_50k` | post-commit | total | 3956 | 3956 |
| `C1_include_50k` | prepare-commit-msg | attribution_preview | 437 | 437 |
| `C1_include_50k` | prepare-commit-msg | discover | 52 | 52 |
| `C1_include_50k` | prepare-commit-msg | live_sessions | 978 | 978 |
| `C1_include_50k` | prepare-commit-msg | open_projection | 2664 | 2664 |
| `C1_include_50k` | prepare-commit-msg | predict_id | 18873 | 446 (parent, `redact` subtracted) |
| `C1_include_50k` | prepare-commit-msg | raw_capture | 597 | 597 |
| `C1_include_50k` | prepare-commit-msg | read_facts | 137 | 137 |
| `C1_include_50k` | prepare-commit-msg | redact | 18427 | 18427 |
| `C1_include_50k` | prepare-commit-msg | rule_packs | 27 | 27 |
| `C1_include_50k` | prepare-commit-msg | tokio_build | 23 | 23 |
| `C1_include_50k` | prepare-commit-msg | total | 24656 | 24656 |
| `C2_include_250k` | post-commit | discover | 39 | 39 |
| `C2_include_250k` | post-commit | enqueue | 619 | 619 |
| `C2_include_250k` | post-commit | gix_open | 960 | 960 |
| `C2_include_250k` | post-commit | head_hex | 586 | 586 |
| `C2_include_250k` | post-commit | live_sessions | 901 | 901 |
| `C2_include_250k` | post-commit | spawn_worker | 1916 | 1916 |
| `C2_include_250k` | post-commit | total | 5322 | 5322 |
| `C2_include_250k` | prepare-commit-msg | attribution_preview | 428 | 428 |
| `C2_include_250k` | prepare-commit-msg | discover | 40 | 40 |
| `C2_include_250k` | prepare-commit-msg | live_sessions | 1012 | 1012 |
| `C2_include_250k` | prepare-commit-msg | open_projection | 3760 | 3760 |
| `C2_include_250k` | prepare-commit-msg | predict_id | 40182 | 1664 (parent, `redact` subtracted) |
| `C2_include_250k` | prepare-commit-msg | raw_capture | 3703 | 3703 |
| `C2_include_250k` | prepare-commit-msg | read_facts | 171 | 171 |
| `C2_include_250k` | prepare-commit-msg | redact | 38518 | 38518 |
| `C2_include_250k` | prepare-commit-msg | rule_packs | 34 | 34 |
| `C2_include_250k` | prepare-commit-msg | tokio_build | 27 | 27 |
| `C2_include_250k` | prepare-commit-msg | total | 50495 | 50495 |
| `C3_include_1mib` | post-commit | discover | 58 | 58 |
| `C3_include_1mib` | post-commit | enqueue | 741 | 741 |
| `C3_include_1mib` | post-commit | gix_open | 1267 | 1267 |
| `C3_include_1mib` | post-commit | head_hex | 629 | 629 |
| `C3_include_1mib` | post-commit | live_sessions | 1114 | 1114 |
| `C3_include_1mib` | post-commit | spawn_worker | 1998 | 1998 |
| `C3_include_1mib` | post-commit | total | 6122 | 6122 |
| `C3_include_1mib` | prepare-commit-msg | attribution_preview | 615 | 615 |
| `C3_include_1mib` | prepare-commit-msg | discover | 41 | 41 |
| `C3_include_1mib` | prepare-commit-msg | live_sessions | 892 | 892 |
| `C3_include_1mib` | prepare-commit-msg | open_projection | 2716 | 2716 |
| `C3_include_1mib` | prepare-commit-msg | predict_id | 74257 | 8250 (parent, `redact` subtracted) |
| `C3_include_1mib` | prepare-commit-msg | raw_capture | 3043 | 3043 |
| `C3_include_1mib` | prepare-commit-msg | read_facts | 131 | 131 |
| `C3_include_1mib` | prepare-commit-msg | redact | 66007 | 66007 |
| `C3_include_1mib` | prepare-commit-msg | rule_packs | 26 | 26 |
| `C3_include_1mib` | prepare-commit-msg | tokio_build | 23 | 23 |
| `C3_include_1mib` | prepare-commit-msg | total | 84409 | 84409 |

### Phase audit

`total` is measured independently of the phases, so disjoint phases can never exceed it. `over-attributed` is therefore a proof that something is nested which the harness does not declare — not a rounding remark.

| arm | hook | total µs | attributed µs | unattributed µs | verdict |
|---|---|---|---|---|---|
| `B0_idle` | post-commit | 2811 | 2643 | 168 | adds up |
| `B0_idle` | prepare-commit-msg | 201 | 76 | 125 | adds up |
| `B1_live` | post-commit | 4743 | 4462 | 281 | adds up |
| `B1_live` | prepare-commit-msg | 39717 | 38425 | 1292 | adds up |
| `C1_include_50k` | post-commit | 3956 | 3708 | 248 | adds up |
| `C1_include_50k` | prepare-commit-msg | 24656 | 23788 | 868 | adds up |
| `C2_include_250k` | post-commit | 5322 | 5021 | 301 | adds up |
| `C2_include_250k` | prepare-commit-msg | 50495 | 49357 | 1138 | adds up |
| `C3_include_1mib` | post-commit | 6122 | 5807 | 315 | adds up |
| `C3_include_1mib` | prepare-commit-msg | 84409 | 81744 | 2665 | adds up |
