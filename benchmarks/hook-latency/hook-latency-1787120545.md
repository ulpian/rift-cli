# Hook latency — git-latency sweep

- harness `0.0.1`, artifact schema 1, generated 1787120545
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
| `A_bare` | 58.37 ms | 50.10 ms | — | 1.00× | control |
| `B0_idle` | 117.12 ms | 106.78 ms | 58.75 ms | 2.01× | meets |
| `B1_live` | 143.71 ms | 126.67 ms | 85.34 ms | 2.46× | meets |
| `C1_include_50k` | 158.02 ms | 131.04 ms | 99.65 ms | 2.71× | meets |
| `C2_include_250k` | 189.63 ms | 180.58 ms | 131.26 ms | 3.25× | **BREACH** |
| `C3_include_1mib` | 229.54 ms | 186.63 ms | 171.16 ms | 3.93× | **BREACH** |

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

**BREACH: `C2_include_250k`, `C3_include_1mib`.** The budget is 100 ms of git latency and is HARD (set by the owner). The corpus was not adjusted in response — the arms are the sizes they were declared at.

**Magnitude and attribution come from different commits.** `git latency` is the median of the timed commits minus the control's; every phase column comes from the arm's one **extra untimed** commit with the timer on. Read the residue as an anatomy of a representative commit beside the magnitude of the median, never as an exact decomposition of one commit.

`predict_id` here is **exclusive** — `redact` subtracted — so the columns are disjoint and may be added.

| arm | bytes read | handed on | ×read | git latency | hook bodies | outside bodies | `redact` | `predict_id` less `redact` | verdict |
|---|---|---|---|---|---|---|---|---|---|
| `B0_idle` | — | — | — | 58.74 ms | 3.67 ms | 55.07 ms | 0.00 ms | 0.00 ms | meets |
| `B1_live` | — | — | — | 85.34 ms | 122.83 ms | 0.00 ms | 101.89 ms | 0.09 ms | meets |
| `C1_include_50k` | 51200 | 52696 | 1.03× | 99.65 ms | 29.93 ms | 69.72 ms | 18.59 ms | 0.32 ms | meets |
| `C2_include_250k` | 256000 | 263480 | 1.03× | 131.26 ms | 68.06 ms | 63.20 ms | 45.47 ms | 2.67 ms | **BREACH** |
| `C3_include_1mib` | 1048576 | 1066352 | 1.02× | 171.16 ms | 88.97 ms | 82.19 ms | 65.61 ms | 7.96 ms | **BREACH** |

**One arm's hook bodies measured larger than its median git latency.** That is possible across two populations — a slow phase commit against a fast median — and it is reported rather than clamped, because the alternative is a zero in the `outside bodies` column that would read as "process creation is free".

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
| `C3_include_1mib` | rift/canopy/ui-state.db 90112 B; rift/<repo-id>/projection.db 565248 B | present | 12 checkpoint(s), 13 shard(s) | 60 |

**More than one projection under a single arm's cache.** That is the shape of the defect this table exists for: the number may be about a store rift never opened.

## Phases

From one extra **untimed** commit per arm with `RIFT_HOOK_TIMING=1`, straight out of the shipped binary. Untimed because the timer takes clock readings the shipped default never takes.

**Read the `exclusive` column, and never sum `recorded`.** The timer records on completion, so a nested phase prints before its parent and the parent's figure already contains it: `predict_id` INCLUDES `redact`. `recorded` is what the binary printed; `exclusive` has every declared child subtracted, so those values are disjoint and may be added. Summing `recorded` would double-count the redaction — the one quantity the opted-in arms exist to separate.

| arm | hook | phase | recorded µs | exclusive µs |
|---|---|---|---|---|
| `B0_idle` | post-commit | discover | 31 | 31 |
| `B0_idle` | post-commit | enqueue | 523 | 523 |
| `B0_idle` | post-commit | gix_open | 815 | 815 |
| `B0_idle` | post-commit | head_hex | 391 | 391 |
| `B0_idle` | post-commit | live_sessions | 24 | 24 |
| `B0_idle` | post-commit | spawn_worker | 1492 | 1492 |
| `B0_idle` | post-commit | total | 3526 | 3526 |
| `B0_idle` | prepare-commit-msg | discover | 41 | 41 |
| `B0_idle` | prepare-commit-msg | live_sessions | 15 | 15 |
| `B0_idle` | prepare-commit-msg | total | 147 | 147 |
| `B1_live` | post-commit | discover | 53 | 53 |
| `B1_live` | post-commit | enqueue | 880 | 880 |
| `B1_live` | post-commit | gix_open | 1501 | 1501 |
| `B1_live` | post-commit | head_hex | 784 | 784 |
| `B1_live` | post-commit | live_sessions | 1594 | 1594 |
| `B1_live` | post-commit | spawn_worker | 2318 | 2318 |
| `B1_live` | post-commit | total | 7512 | 7512 |
| `B1_live` | prepare-commit-msg | attribution_preview | 5322 | 5322 |
| `B1_live` | prepare-commit-msg | discover | 73 | 73 |
| `B1_live` | prepare-commit-msg | live_sessions | 1372 | 1372 |
| `B1_live` | prepare-commit-msg | open_projection | 4924 | 4924 |
| `B1_live` | prepare-commit-msg | predict_id | 101983 | 94 (parent, `redact` subtracted) |
| `B1_live` | prepare-commit-msg | raw_capture | 61 | 61 |
| `B1_live` | prepare-commit-msg | read_facts | 315 | 315 |
| `B1_live` | prepare-commit-msg | redact | 101889 | 101889 |
| `B1_live` | prepare-commit-msg | rule_packs | 31 | 31 |
| `B1_live` | prepare-commit-msg | tokio_build | 34 | 34 |
| `B1_live` | prepare-commit-msg | total | 115313 | 115313 |
| `C1_include_50k` | post-commit | discover | 41 | 41 |
| `C1_include_50k` | post-commit | enqueue | 857 | 857 |
| `C1_include_50k` | post-commit | gix_open | 919 | 919 |
| `C1_include_50k` | post-commit | head_hex | 693 | 693 |
| `C1_include_50k` | post-commit | live_sessions | 1043 | 1043 |
| `C1_include_50k` | post-commit | spawn_worker | 1430 | 1430 |
| `C1_include_50k` | post-commit | total | 5259 | 5259 |
| `C1_include_50k` | prepare-commit-msg | attribution_preview | 401 | 401 |
| `C1_include_50k` | prepare-commit-msg | discover | 79 | 79 |
| `C1_include_50k` | prepare-commit-msg | live_sessions | 874 | 874 |
| `C1_include_50k` | prepare-commit-msg | open_projection | 2668 | 2668 |
| `C1_include_50k` | prepare-commit-msg | predict_id | 18908 | 315 (parent, `redact` subtracted) |
| `C1_include_50k` | prepare-commit-msg | raw_capture | 820 | 820 |
| `C1_include_50k` | prepare-commit-msg | read_facts | 137 | 137 |
| `C1_include_50k` | prepare-commit-msg | redact | 18593 | 18593 |
| `C1_include_50k` | prepare-commit-msg | rule_packs | 26 | 26 |
| `C1_include_50k` | prepare-commit-msg | tokio_build | 22 | 22 |
| `C1_include_50k` | prepare-commit-msg | total | 24669 | 24669 |
| `C2_include_250k` | post-commit | discover | 54 | 54 |
| `C2_include_250k` | post-commit | enqueue | 822 | 822 |
| `C2_include_250k` | post-commit | gix_open | 1276 | 1276 |
| `C2_include_250k` | post-commit | head_hex | 799 | 799 |
| `C2_include_250k` | post-commit | live_sessions | 1236 | 1236 |
| `C2_include_250k` | post-commit | spawn_worker | 2389 | 2389 |
| `C2_include_250k` | post-commit | total | 6941 | 6941 |
| `C2_include_250k` | prepare-commit-msg | attribution_preview | 670 | 670 |
| `C2_include_250k` | prepare-commit-msg | discover | 71 | 71 |
| `C2_include_250k` | prepare-commit-msg | live_sessions | 1304 | 1304 |
| `C2_include_250k` | prepare-commit-msg | open_projection | 4923 | 4923 |
| `C2_include_250k` | prepare-commit-msg | predict_id | 48142 | 2672 (parent, `redact` subtracted) |
| `C2_include_250k` | prepare-commit-msg | raw_capture | 4145 | 4145 |
| `C2_include_250k` | prepare-commit-msg | read_facts | 226 | 226 |
| `C2_include_250k` | prepare-commit-msg | redact | 45470 | 45470 |
| `C2_include_250k` | prepare-commit-msg | rule_packs | 43 | 43 |
| `C2_include_250k` | prepare-commit-msg | tokio_build | 36 | 36 |
| `C2_include_250k` | prepare-commit-msg | total | 61118 | 61118 |
| `C3_include_1mib` | post-commit | discover | 35 | 35 |
| `C3_include_1mib` | post-commit | enqueue | 702 | 702 |
| `C3_include_1mib` | post-commit | gix_open | 766 | 766 |
| `C3_include_1mib` | post-commit | head_hex | 395 | 395 |
| `C3_include_1mib` | post-commit | live_sessions | 826 | 826 |
| `C3_include_1mib` | post-commit | spawn_worker | 1547 | 1547 |
| `C3_include_1mib` | post-commit | total | 4581 | 4581 |
| `C3_include_1mib` | prepare-commit-msg | attribution_preview | 588 | 588 |
| `C3_include_1mib` | prepare-commit-msg | discover | 33 | 33 |
| `C3_include_1mib` | prepare-commit-msg | live_sessions | 759 | 759 |
| `C3_include_1mib` | prepare-commit-msg | open_projection | 3264 | 3264 |
| `C3_include_1mib` | prepare-commit-msg | predict_id | 73567 | 7958 (parent, `redact` subtracted) |
| `C3_include_1mib` | prepare-commit-msg | raw_capture | 3573 | 3573 |
| `C3_include_1mib` | prepare-commit-msg | read_facts | 134 | 134 |
| `C3_include_1mib` | prepare-commit-msg | redact | 65609 | 65609 |
| `C3_include_1mib` | prepare-commit-msg | rule_packs | 39 | 39 |
| `C3_include_1mib` | prepare-commit-msg | tokio_build | 24 | 24 |
| `C3_include_1mib` | prepare-commit-msg | total | 84388 | 84388 |

### Phase audit

`total` is measured independently of the phases, so disjoint phases can never exceed it. `over-attributed` is therefore a proof that something is nested which the harness does not declare — not a rounding remark.

| arm | hook | total µs | attributed µs | unattributed µs | verdict |
|---|---|---|---|---|---|
| `B0_idle` | post-commit | 3526 | 3276 | 250 | adds up |
| `B0_idle` | prepare-commit-msg | 147 | 56 | 91 | adds up |
| `B1_live` | post-commit | 7512 | 7130 | 382 | adds up |
| `B1_live` | prepare-commit-msg | 115313 | 114115 | 1198 | adds up |
| `C1_include_50k` | post-commit | 5259 | 4983 | 276 | adds up |
| `C1_include_50k` | prepare-commit-msg | 24669 | 23935 | 734 | adds up |
| `C2_include_250k` | post-commit | 6941 | 6576 | 365 | adds up |
| `C2_include_250k` | prepare-commit-msg | 61118 | 59560 | 1558 | adds up |
| `C3_include_1mib` | post-commit | 4581 | 4271 | 310 | adds up |
| `C3_include_1mib` | prepare-commit-msg | 84388 | 81981 | 2407 | adds up |
