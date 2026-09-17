# Hook latency — git-latency sweep

- harness `0.0.1`, artifact schema 1, generated 1787120379
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
| `A_bare` | 48.88 ms | 46.56 ms | — | 1.00× | control |
| `B0_idle` | 126.59 ms | 117.07 ms | 77.71 ms | 2.59× | meets |
| `B1_live` | 145.07 ms | 129.89 ms | 96.18 ms | 2.97× | meets |
| `C1_include_50k` | 190.61 ms | 176.58 ms | 141.73 ms | 3.90× | **BREACH** |
| `C2_include_250k` | 158.27 ms | 138.38 ms | 109.38 ms | 3.24× | **BREACH** |
| `C3_include_1mib` | 247.65 ms | 213.84 ms | 198.77 ms | 5.07× | **BREACH** |

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

**BREACH: `C1_include_50k`, `C2_include_250k`, `C3_include_1mib`.** The budget is 100 ms of git latency and is HARD (set by the owner). The corpus was not adjusted in response — the arms are the sizes they were declared at.

**Magnitude and attribution come from different commits.** `git latency` is the median of the timed commits minus the control's; every phase column comes from the arm's one **extra untimed** commit with the timer on. Read the residue as an anatomy of a representative commit beside the magnitude of the median, never as an exact decomposition of one commit.

`predict_id` here is **exclusive** — `redact` subtracted — so the columns are disjoint and may be added.

| arm | bytes read | handed on | ×read | git latency | hook bodies | outside bodies | `redact` | `predict_id` less `redact` | verdict |
|---|---|---|---|---|---|---|---|---|---|
| `B0_idle` | — | — | — | 77.70 ms | 4.49 ms | 73.22 ms | 0.00 ms | 0.00 ms | meets |
| `B1_live` | — | — | — | 96.18 ms | 40.50 ms | 55.68 ms | 27.30 ms | 0.08 ms | meets |
| `C1_include_50k` | 51200 | 52696 | 1.03× | 141.73 ms | 39.36 ms | 102.37 ms | 24.86 ms | 0.37 ms | **BREACH** |
| `C2_include_250k` | 256000 | 263480 | 1.03× | 109.38 ms | 67.91 ms | 41.47 ms | 42.19 ms | 2.33 ms | **BREACH** |
| `C3_include_1mib` | 1048576 | 1066352 | 1.02× | 198.77 ms | 79.21 ms | 119.56 ms | 58.89 ms | 6.27 ms | **BREACH** |

`outside bodies` is the floor a Rust change cannot move: three `/bin/sh` invocations and three execs that git requires, plus git's own cost of running them. If the budget ever has to halve, that column is what has to be attacked, and it is not in Rust code.

## Oracles — what each arm exercised

Reported for every arm including the control, because a check that only ever confirms is not a check: the idle arm's **absent** trailer is what makes the live arm's present one mean something.

| arm | projections | trailer | verify | events |
|---|---|---|---|---|
| `A_bare` | none | absent | — | — |
| `B0_idle` | rift/<repo-id>/projection.db 430080 B; rift/canopy/ui-state.db 90112 B | absent | 1 checkpoint(s), 0 shard(s) | 0 |
| `B1_live` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | — | 60 |
| `C1_include_50k` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 60 |
| `C2_include_250k` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 565248 B | present | — | 60 |
| `C3_include_1mib` | rift/<repo-id>/projection.db 565248 B; rift/canopy/ui-state.db 90112 B | present | 12 checkpoint(s), 13 shard(s) | 60 |

**More than one projection under a single arm's cache.** That is the shape of the defect this table exists for: the number may be about a store rift never opened.

## Phases

From one extra **untimed** commit per arm with `RIFT_HOOK_TIMING=1`, straight out of the shipped binary. Untimed because the timer takes clock readings the shipped default never takes.

**Read the `exclusive` column, and never sum `recorded`.** The timer records on completion, so a nested phase prints before its parent and the parent's figure already contains it: `predict_id` INCLUDES `redact`. `recorded` is what the binary printed; `exclusive` has every declared child subtracted, so those values are disjoint and may be added. Summing `recorded` would double-count the redaction — the one quantity the opted-in arms exist to separate.

| arm | hook | phase | recorded µs | exclusive µs |
|---|---|---|---|---|
| `B0_idle` | post-commit | discover | 49 | 49 |
| `B0_idle` | post-commit | enqueue | 795 | 795 |
| `B0_idle` | post-commit | gix_open | 947 | 947 |
| `B0_idle` | post-commit | head_hex | 680 | 680 |
| `B0_idle` | post-commit | live_sessions | 34 | 34 |
| `B0_idle` | post-commit | spawn_worker | 1540 | 1540 |
| `B0_idle` | post-commit | total | 4331 | 4331 |
| `B0_idle` | prepare-commit-msg | discover | 34 | 34 |
| `B0_idle` | prepare-commit-msg | live_sessions | 16 | 16 |
| `B0_idle` | prepare-commit-msg | total | 154 | 154 |
| `B1_live` | post-commit | discover | 55 | 55 |
| `B1_live` | post-commit | enqueue | 486 | 486 |
| `B1_live` | post-commit | gix_open | 1248 | 1248 |
| `B1_live` | post-commit | head_hex | 417 | 417 |
| `B1_live` | post-commit | live_sessions | 634 | 634 |
| `B1_live` | post-commit | spawn_worker | 1408 | 1408 |
| `B1_live` | post-commit | total | 4512 | 4512 |
| `B1_live` | prepare-commit-msg | attribution_preview | 584 | 584 |
| `B1_live` | prepare-commit-msg | discover | 122 | 122 |
| `B1_live` | prepare-commit-msg | live_sessions | 1591 | 1591 |
| `B1_live` | prepare-commit-msg | open_projection | 4681 | 4681 |
| `B1_live` | prepare-commit-msg | predict_id | 27379 | 82 (parent, `redact` subtracted) |
| `B1_live` | prepare-commit-msg | raw_capture | 30 | 30 |
| `B1_live` | prepare-commit-msg | read_facts | 187 | 187 |
| `B1_live` | prepare-commit-msg | redact | 27297 | 27297 |
| `B1_live` | prepare-commit-msg | rule_packs | 23 | 23 |
| `B1_live` | prepare-commit-msg | tokio_build | 28 | 28 |
| `B1_live` | prepare-commit-msg | total | 35991 | 35991 |
| `C1_include_50k` | post-commit | discover | 55 | 55 |
| `C1_include_50k` | post-commit | enqueue | 577 | 577 |
| `C1_include_50k` | post-commit | gix_open | 849 | 849 |
| `C1_include_50k` | post-commit | head_hex | 421 | 421 |
| `C1_include_50k` | post-commit | live_sessions | 637 | 637 |
| `C1_include_50k` | post-commit | spawn_worker | 1732 | 1732 |
| `C1_include_50k` | post-commit | total | 4603 | 4603 |
| `C1_include_50k` | prepare-commit-msg | attribution_preview | 496 | 496 |
| `C1_include_50k` | prepare-commit-msg | discover | 52 | 52 |
| `C1_include_50k` | prepare-commit-msg | live_sessions | 1689 | 1689 |
| `C1_include_50k` | prepare-commit-msg | open_projection | 4388 | 4388 |
| `C1_include_50k` | prepare-commit-msg | predict_id | 25229 | 370 (parent, `redact` subtracted) |
| `C1_include_50k` | prepare-commit-msg | raw_capture | 1378 | 1378 |
| `C1_include_50k` | prepare-commit-msg | read_facts | 229 | 229 |
| `C1_include_50k` | prepare-commit-msg | redact | 24859 | 24859 |
| `C1_include_50k` | prepare-commit-msg | rule_packs | 68 | 68 |
| `C1_include_50k` | prepare-commit-msg | tokio_build | 27 | 27 |
| `C1_include_50k` | prepare-commit-msg | total | 34757 | 34757 |
| `C2_include_250k` | post-commit | discover | 50 | 50 |
| `C2_include_250k` | post-commit | enqueue | 792 | 792 |
| `C2_include_250k` | post-commit | gix_open | 1922 | 1922 |
| `C2_include_250k` | post-commit | head_hex | 800 | 800 |
| `C2_include_250k` | post-commit | live_sessions | 1686 | 1686 |
| `C2_include_250k` | post-commit | spawn_worker | 2814 | 2814 |
| `C2_include_250k` | post-commit | total | 8643 | 8643 |
| `C2_include_250k` | prepare-commit-msg | attribution_preview | 594 | 594 |
| `C2_include_250k` | prepare-commit-msg | discover | 46 | 46 |
| `C2_include_250k` | prepare-commit-msg | live_sessions | 1656 | 1656 |
| `C2_include_250k` | prepare-commit-msg | open_projection | 4835 | 4835 |
| `C2_include_250k` | prepare-commit-msg | predict_id | 44519 | 2329 (parent, `redact` subtracted) |
| `C2_include_250k` | prepare-commit-msg | raw_capture | 5842 | 5842 |
| `C2_include_250k` | prepare-commit-msg | read_facts | 225 | 225 |
| `C2_include_250k` | prepare-commit-msg | redact | 42190 | 42190 |
| `C2_include_250k` | prepare-commit-msg | rule_packs | 38 | 38 |
| `C2_include_250k` | prepare-commit-msg | tokio_build | 32 | 32 |
| `C2_include_250k` | prepare-commit-msg | total | 59271 | 59271 |
| `C3_include_1mib` | post-commit | discover | 42 | 42 |
| `C3_include_1mib` | post-commit | enqueue | 687 | 687 |
| `C3_include_1mib` | post-commit | gix_open | 1024 | 1024 |
| `C3_include_1mib` | post-commit | head_hex | 597 | 597 |
| `C3_include_1mib` | post-commit | live_sessions | 928 | 928 |
| `C3_include_1mib` | post-commit | spawn_worker | 1465 | 1465 |
| `C3_include_1mib` | post-commit | total | 5020 | 5020 |
| `C3_include_1mib` | prepare-commit-msg | attribution_preview | 380 | 380 |
| `C3_include_1mib` | prepare-commit-msg | discover | 31 | 31 |
| `C3_include_1mib` | prepare-commit-msg | live_sessions | 616 | 616 |
| `C3_include_1mib` | prepare-commit-msg | open_projection | 2943 | 2943 |
| `C3_include_1mib` | prepare-commit-msg | predict_id | 65159 | 6271 (parent, `redact` subtracted) |
| `C3_include_1mib` | prepare-commit-msg | raw_capture | 3406 | 3406 |
| `C3_include_1mib` | prepare-commit-msg | read_facts | 137 | 137 |
| `C3_include_1mib` | prepare-commit-msg | redact | 58888 | 58888 |
| `C3_include_1mib` | prepare-commit-msg | rule_packs | 23 | 23 |
| `C3_include_1mib` | prepare-commit-msg | tokio_build | 22 | 22 |
| `C3_include_1mib` | prepare-commit-msg | total | 74189 | 74189 |

### Phase audit

`total` is measured independently of the phases, so disjoint phases can never exceed it. `over-attributed` is therefore a proof that something is nested which the harness does not declare — not a rounding remark.

| arm | hook | total µs | attributed µs | unattributed µs | verdict |
|---|---|---|---|---|---|
| `B0_idle` | post-commit | 4331 | 4045 | 286 | adds up |
| `B0_idle` | prepare-commit-msg | 154 | 50 | 104 | adds up |
| `B1_live` | post-commit | 4512 | 4248 | 264 | adds up |
| `B1_live` | prepare-commit-msg | 35991 | 34625 | 1366 | adds up |
| `C1_include_50k` | post-commit | 4603 | 4271 | 332 | adds up |
| `C1_include_50k` | prepare-commit-msg | 34757 | 33556 | 1201 | adds up |
| `C2_include_250k` | post-commit | 8643 | 8064 | 579 | adds up |
| `C2_include_250k` | prepare-commit-msg | 59271 | 57787 | 1484 | adds up |
| `C3_include_1mib` | post-commit | 5020 | 4743 | 277 | adds up |
| `C3_include_1mib` | prepare-commit-msg | 74189 | 72717 | 1472 | adds up |
