# Hook latency — git-latency sweep

- harness `0.0.1`, artifact schema 2, generated 1788101269
- host: macos / x86_64 (8 logical CPUs)
- control git: git version 2.50.1 (Apple Git-155)
- protocol: 3 warmup + 11 measured commits per arm
- budget: **100 ms** of git latency (a hard budget, set by the owner)

## Subject

`/Users/ulpian/rift-worktrees/cleanup/target/release/rift` — rift 0.0.1, resolved via repo target/release, built 1788099828

**Record.** No commit is recorded for this run. It is kept as a *record* — a real measurement, quotable as history, barred from setting a bar — because the history that held its subject was collapsed to a single fresh root on 2026-09-02.

Newest source in the checkout: `crates/rift-shadow/src/project.rs` at 1788099427 (316 files).

## Arms

`hook latency` is this arm's p50 minus the control's, in the same run; `× bare` is the ratio, and the ratio is the portable number.

| arm | p50 | min | hook latency | × bare | verdict |
|---|---|---|---|---|---|
| `A_bare` | 34.96 ms | 33.74 ms | — | 1.00× | control · load1 5.42 |
| `B0_idle` | 64.33 ms | 62.97 ms | 29.36 ms | 1.84× | meets · load1 5.42 |
| `B1_live` | 80.29 ms | 78.16 ms | 45.33 ms | 2.30× | meets · load1 5.42 |
| `G1_line_2k` | 94.51 ms | 89.05 ms | 59.54 ms | 2.70× | meets · load1 5.28 |
| `G2_line_4k` | 99.34 ms | 96.95 ms | 64.38 ms | 2.84× | meets · load1 5.17 |
| `G3_line_8k` | 103.29 ms | 101.31 ms | 68.32 ms | 2.95× | meets · load1 4.99 |
| `H1_newline_2k` | 106.67 ms | 95.38 ms | 71.71 ms | 3.05× | meets · load1 4.99 |
| `H2_newline_4k` | 146.35 ms | 121.91 ms | 111.39 ms | 4.19× | **BREACH** · load1 5.13 |
| `H3_newline_8k` | 136.98 ms | 120.14 ms | 102.01 ms | 3.92× | **BREACH** · load1 5.64 |

- `A_bare` — bare commit, no hooks installed (control)

- `B0_idle` — hooks installed, nothing opted in, no live session

- `B1_live` — hooks installed, a session live at commit time

- `G1_line_2k` — live session + `.worktreeinclude` over 4 files (2048 B read, 2428 B handed on, 1.19x)

- `G2_line_4k` — live session + `.worktreeinclude` over 4 files (4096 B read, 4508 B handed on, 1.10x)

- `G3_line_8k` — live session + `.worktreeinclude` over 8 files (8192 B read, 9016 B handed on, 1.10x)

- `H1_newline_2k` — live session + `.worktreeinclude` over 4 files (2048 B read, 4192 B handed on, 2.05x)

- `H2_newline_4k` — live session + `.worktreeinclude` over 4 files (4096 B read, 8288 B handed on, 2.02x)

- `H3_newline_8k` — live session + `.worktreeinclude` over 8 files (8192 B read, 16576 B handed on, 2.02x)

## Opted-in corpus

`MAX_TOTAL_BYTES` is 8 KiB and is charged on bytes **read**; `Walk::file` prefixes every content line with one space, so N bytes contribute up to **2N + 2** to the document the redactor is handed. A full budget therefore hands on just over 16 KiB, not 8 KiB. Both numbers are published, because an arm described by one figure alone misstates the ceiling in one direction or the other.

| arm | files | bytes read | budget | document | factor | files read (explain) |
|---|---|---|---|---|---|---|
| `G1_line_2k` | 4 | 2048 | under | 2428 | 1.19× | — |
| `G2_line_4k` | 4 | 4096 | under | 4508 | 1.10× | — |
| `G3_line_8k` | 8 | 8192 | **at/over the 8 KiB read ceiling** | 9016 | 1.10× | 1 |
| `H1_newline_2k` | 4 | 2048 | under | 4192 | 2.05× | — |
| `H2_newline_4k` | 4 | 4096 | under | 8288 | 2.02× | — |
| `H3_newline_8k` | 8 | 8192 | **at/over the 8 KiB read ceiling** | 16576 | 2.02× | 1 |

**UNCONFIRMED BY THE PRODUCT: `G1_line_2k`, `G2_line_4k`, `H1_newline_2k`, `H2_newline_4k`.** `files read (explain)` is `—` for these arms, so every byte figure in their rows is the harness's own arithmetic over the corpus it wrote — nothing the product reported back agrees with it. That is the cross-check probe failing *silently*, which is worse than no probe because it occupies the slot where a check would go. Read those byte columns as declarations, not measurements, until it answers.

The factor here is near 1.0 because these lines are long. The 2× is the              pathological end — a file that is all newlines, where the one-space prefix is the              whole line — so a full budget hands on nearly twice the read ceiling. Neither end is the other, and this table says which one each arm is.

`files read (explain)` is `rift checkpoint explain`'s **own** label, not a span              count: the inclusion walk's record count is on no CLI surface today, so the              harness publishes the figure the product publishes rather than relabelling it as              something it has not established.

## Anatomy — the curve, the units, and the phase that owns it

**BREACH: `H2_newline_4k`, `H3_newline_8k`.** The budget is 100 ms of git latency and is HARD (set by the owner). The corpus was not adjusted in response — the arms are the sizes they were declared at.

**Magnitude and attribution come from different commits.** `git latency` is the median of the timed commits minus the control's; every phase column comes from the arm's one **extra untimed** commit with the timer on. Read the residue as an anatomy of a representative commit beside the magnitude of the median, never as an exact decomposition of one commit.

`predict_id` here is **exclusive** — `redact` subtracted — so the columns are disjoint and may be added.

| arm | bytes read | handed on | ×read | git latency | hook bodies | outside bodies | `redact` | `predict_id` less `redact` | verdict |
|---|---|---|---|---|---|---|---|---|---|
| `B0_idle` | — | — | — | 29.36 ms | 3.13 ms | 26.23 ms | 0.00 ms | 0.00 ms | meets |
| `B1_live` | — | — | — | 45.33 ms | 21.32 ms | 24.01 ms | 13.81 ms | 0.09 ms | meets |
| `G1_line_2k` | 2048 | 2428 | 1.19× | 59.54 ms | 24.06 ms | 35.49 ms | 15.56 ms | 0.10 ms | meets |
| `G2_line_4k` | 4096 | 4508 | 1.10× | 64.38 ms | 25.63 ms | 38.75 ms | 16.69 ms | 0.11 ms | meets |
| `G3_line_8k` | 8192 | 9016 | 1.10× | 68.32 ms | 25.65 ms | 42.67 ms | 16.52 ms | 0.13 ms | meets |
| `H1_newline_2k` | 2048 | 4192 | 2.05× | 71.71 ms | 27.10 ms | 44.61 ms | 17.50 ms | 0.10 ms | meets |
| `H2_newline_4k` | 4096 | 8288 | 2.02× | 111.39 ms | 43.09 ms | 68.29 ms | 27.84 ms | 0.26 ms | **BREACH** |
| `H3_newline_8k` | 8192 | 16576 | 2.02× | 102.01 ms | 36.37 ms | 65.64 ms | 22.55 ms | 0.29 ms | **BREACH** |

`outside bodies` is measured rather than assumed: process creation, hook dispatch, and git's own cost of running the three hooks. Fresh Unix installs use direct executable links for unchained hooks; preserved user-hook chains and non-Unix installs retain shell shims.

## Oracles — what each arm exercised

Reported for every arm including the control, because a check that only ever confirms is not a check: the idle arm's **absent** trailer is what makes the live arm's present one mean something.

| arm | projections | trailer | verify | events |
|---|---|---|---|---|
| `A_bare` | none | absent | — | — |
| `B0_idle` | rift/<repo-id>/projection.db 438272 B; rift/canopy/ui-state.db 90112 B | absent | 1 checkpoint(s), 0 shard(s) | 0 |
| `B1_live` | rift/<repo-id>/projection.db 573440 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 90 |
| `G1_line_2k` | rift/<repo-id>/projection.db 573440 B; rift/canopy/ui-state.db 90112 B | present | 9 checkpoint(s), 9 shard(s) | 90 |
| `G2_line_4k` | rift/<repo-id>/projection.db 573440 B; rift/canopy/ui-state.db 90112 B | present | 9 checkpoint(s), 9 shard(s) | 90 |
| `G3_line_8k` | rift/<repo-id>/projection.db 573440 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 90 |
| `H1_newline_2k` | rift/<repo-id>/projection.db 573440 B; rift/canopy/ui-state.db 90112 B | present | 7 checkpoint(s), 7 shard(s) | 90 |
| `H2_newline_4k` | rift/<repo-id>/projection.db 573440 B; rift/canopy/ui-state.db 90112 B | present | — | 90 |
| `H3_newline_8k` | rift/<repo-id>/projection.db 573440 B; rift/canopy/ui-state.db 90112 B | present | 14 checkpoint(s), 15 shard(s) | 90 |

**More than one projection under a single arm's cache.** That is the shape of the defect this table exists for: the number may be about a store rift never opened.

## Phases

From one extra **untimed** commit per arm with `RIFT_HOOK_TIMING=1`, straight out of the shipped binary. Untimed because the timer takes clock readings the shipped default never takes.

**Read the `exclusive` column, and never sum `recorded`.** The timer records on completion, so a nested phase prints before its parent and the parent's figure already contains it: `predict_id` INCLUDES `redact`. `recorded` is what the binary printed; `exclusive` has every declared child subtracted, so those values are disjoint and may be added. Summing `recorded` would double-count the redaction — the one quantity the opted-in arms exist to separate.

| arm | hook | phase | recorded µs | exclusive µs |
|---|---|---|---|---|
| `B0_idle` | post-commit | discover | 31 | 31 |
| `B0_idle` | post-commit | enqueue | 405 | 405 |
| `B0_idle` | post-commit | gix_open | 698 | 698 |
| `B0_idle` | post-commit | head_hex | 347 | 347 |
| `B0_idle` | post-commit | live_sessions | 12 | 12 |
| `B0_idle` | post-commit | spawn_worker | 1355 | 1355 |
| `B0_idle` | post-commit | total | 3020 | 3020 |
| `B0_idle` | prepare-commit-msg | discover | 25 | 25 |
| `B0_idle` | prepare-commit-msg | live_sessions | 13 | 13 |
| `B0_idle` | prepare-commit-msg | total | 108 | 108 |
| `B1_live` | post-commit | discover | 29 | 29 |
| `B1_live` | post-commit | enqueue | 356 | 356 |
| `B1_live` | post-commit | gix_open | 644 | 644 |
| `B1_live` | post-commit | head_hex | 318 | 318 |
| `B1_live` | post-commit | live_sessions | 452 | 452 |
| `B1_live` | post-commit | spawn_worker | 1247 | 1247 |
| `B1_live` | post-commit | total | 3218 | 3218 |
| `B1_live` | prepare-commit-msg | attribution_preview | 291 | 291 |
| `B1_live` | prepare-commit-msg | discover | 28 | 28 |
| `B1_live` | prepare-commit-msg | live_sessions | 505 | 505 |
| `B1_live` | prepare-commit-msg | open_projection | 2471 | 2471 |
| `B1_live` | prepare-commit-msg | predict_id | 13898 | 87 (parent, `redact` subtracted) |
| `B1_live` | prepare-commit-msg | raw_capture | 22 | 22 |
| `B1_live` | prepare-commit-msg | read_facts | 180 | 180 |
| `B1_live` | prepare-commit-msg | redact | 13811 | 13811 |
| `B1_live` | prepare-commit-msg | rule_packs | 17 | 17 |
| `B1_live` | prepare-commit-msg | tokio_build | 22 | 22 |
| `B1_live` | prepare-commit-msg | total | 18097 | 18097 |
| `G1_line_2k` | post-commit | discover | 32 | 32 |
| `G1_line_2k` | post-commit | enqueue | 395 | 395 |
| `G1_line_2k` | post-commit | gix_open | 737 | 737 |
| `G1_line_2k` | post-commit | head_hex | 361 | 361 |
| `G1_line_2k` | post-commit | live_sessions | 506 | 506 |
| `G1_line_2k` | post-commit | spawn_worker | 1404 | 1404 |
| `G1_line_2k` | post-commit | total | 3638 | 3638 |
| `G1_line_2k` | prepare-commit-msg | attribution_preview | 322 | 322 |
| `G1_line_2k` | prepare-commit-msg | discover | 31 | 31 |
| `G1_line_2k` | prepare-commit-msg | live_sessions | 552 | 552 |
| `G1_line_2k` | prepare-commit-msg | open_projection | 2705 | 2705 |
| `G1_line_2k` | prepare-commit-msg | predict_id | 15658 | 95 (parent, `redact` subtracted) |
| `G1_line_2k` | prepare-commit-msg | raw_capture | 257 | 257 |
| `G1_line_2k` | prepare-commit-msg | read_facts | 189 | 189 |
| `G1_line_2k` | prepare-commit-msg | redact | 15563 | 15563 |
| `G1_line_2k` | prepare-commit-msg | rule_packs | 19 | 19 |
| `G1_line_2k` | prepare-commit-msg | tokio_build | 24 | 24 |
| `G1_line_2k` | prepare-commit-msg | total | 20419 | 20419 |
| `G2_line_4k` | post-commit | discover | 35 | 35 |
| `G2_line_4k` | post-commit | enqueue | 415 | 415 |
| `G2_line_4k` | post-commit | gix_open | 767 | 767 |
| `G2_line_4k` | post-commit | head_hex | 370 | 370 |
| `G2_line_4k` | post-commit | live_sessions | 525 | 525 |
| `G2_line_4k` | post-commit | spawn_worker | 1405 | 1405 |
| `G2_line_4k` | post-commit | total | 3727 | 3727 |
| `G2_line_4k` | prepare-commit-msg | attribution_preview | 400 | 400 |
| `G2_line_4k` | prepare-commit-msg | discover | 32 | 32 |
| `G2_line_4k` | prepare-commit-msg | live_sessions | 574 | 574 |
| `G2_line_4k` | prepare-commit-msg | open_projection | 2799 | 2799 |
| `G2_line_4k` | prepare-commit-msg | predict_id | 16801 | 110 (parent, `redact` subtracted) |
| `G2_line_4k` | prepare-commit-msg | raw_capture | 281 | 281 |
| `G2_line_4k` | prepare-commit-msg | read_facts | 199 | 199 |
| `G2_line_4k` | prepare-commit-msg | redact | 16691 | 16691 |
| `G2_line_4k` | prepare-commit-msg | rule_packs | 21 | 21 |
| `G2_line_4k` | prepare-commit-msg | tokio_build | 27 | 27 |
| `G2_line_4k` | prepare-commit-msg | total | 21903 | 21903 |
| `G3_line_8k` | post-commit | discover | 33 | 33 |
| `G3_line_8k` | post-commit | enqueue | 427 | 427 |
| `G3_line_8k` | post-commit | gix_open | 732 | 732 |
| `G3_line_8k` | post-commit | head_hex | 371 | 371 |
| `G3_line_8k` | post-commit | live_sessions | 928 | 928 |
| `G3_line_8k` | post-commit | spawn_worker | 1373 | 1373 |
| `G3_line_8k` | post-commit | total | 4085 | 4085 |
| `G3_line_8k` | prepare-commit-msg | attribution_preview | 326 | 326 |
| `G3_line_8k` | prepare-commit-msg | discover | 28 | 28 |
| `G3_line_8k` | prepare-commit-msg | live_sessions | 568 | 568 |
| `G3_line_8k` | prepare-commit-msg | open_projection | 2702 | 2702 |
| `G3_line_8k` | prepare-commit-msg | predict_id | 16647 | 131 (parent, `redact` subtracted) |
| `G3_line_8k` | prepare-commit-msg | raw_capture | 380 | 380 |
| `G3_line_8k` | prepare-commit-msg | read_facts | 187 | 187 |
| `G3_line_8k` | prepare-commit-msg | redact | 16516 | 16516 |
| `G3_line_8k` | prepare-commit-msg | rule_packs | 20 | 20 |
| `G3_line_8k` | prepare-commit-msg | tokio_build | 25 | 25 |
| `G3_line_8k` | prepare-commit-msg | total | 21567 | 21567 |
| `H1_newline_2k` | post-commit | discover | 87 | 87 |
| `H1_newline_2k` | post-commit | enqueue | 548 | 548 |
| `H1_newline_2k` | post-commit | gix_open | 794 | 794 |
| `H1_newline_2k` | post-commit | head_hex | 361 | 361 |
| `H1_newline_2k` | post-commit | live_sessions | 507 | 507 |
| `H1_newline_2k` | post-commit | spawn_worker | 1538 | 1538 |
| `H1_newline_2k` | post-commit | total | 4101 | 4101 |
| `H1_newline_2k` | prepare-commit-msg | attribution_preview | 340 | 340 |
| `H1_newline_2k` | prepare-commit-msg | discover | 37 | 37 |
| `H1_newline_2k` | prepare-commit-msg | live_sessions | 694 | 694 |
| `H1_newline_2k` | prepare-commit-msg | open_projection | 3084 | 3084 |
| `H1_newline_2k` | prepare-commit-msg | predict_id | 17610 | 105 (parent, `redact` subtracted) |
| `H1_newline_2k` | prepare-commit-msg | raw_capture | 287 | 287 |
| `H1_newline_2k` | prepare-commit-msg | read_facts | 189 | 189 |
| `H1_newline_2k` | prepare-commit-msg | redact | 17505 | 17505 |
| `H1_newline_2k` | prepare-commit-msg | rule_packs | 21 | 21 |
| `H1_newline_2k` | prepare-commit-msg | tokio_build | 25 | 25 |
| `H1_newline_2k` | prepare-commit-msg | total | 23002 | 23002 |
| `H2_newline_4k` | post-commit | discover | 50 | 50 |
| `H2_newline_4k` | post-commit | enqueue | 733 | 733 |
| `H2_newline_4k` | post-commit | gix_open | 1240 | 1240 |
| `H2_newline_4k` | post-commit | head_hex | 628 | 628 |
| `H2_newline_4k` | post-commit | live_sessions | 1072 | 1072 |
| `H2_newline_4k` | post-commit | spawn_worker | 2232 | 2232 |
| `H2_newline_4k` | post-commit | total | 6307 | 6307 |
| `H2_newline_4k` | prepare-commit-msg | attribution_preview | 534 | 534 |
| `H2_newline_4k` | prepare-commit-msg | discover | 53 | 53 |
| `H2_newline_4k` | prepare-commit-msg | live_sessions | 1522 | 1522 |
| `H2_newline_4k` | prepare-commit-msg | open_projection | 4545 | 4545 |
| `H2_newline_4k` | prepare-commit-msg | predict_id | 28102 | 259 (parent, `redact` subtracted) |
| `H2_newline_4k` | prepare-commit-msg | raw_capture | 406 | 406 |
| `H2_newline_4k` | prepare-commit-msg | read_facts | 253 | 253 |
| `H2_newline_4k` | prepare-commit-msg | redact | 27843 | 27843 |
| `H2_newline_4k` | prepare-commit-msg | rule_packs | 32 | 32 |
| `H2_newline_4k` | prepare-commit-msg | tokio_build | 31 | 31 |
| `H2_newline_4k` | prepare-commit-msg | total | 36788 | 36788 |
| `H3_newline_8k` | post-commit | discover | 51 | 51 |
| `H3_newline_8k` | post-commit | enqueue | 480 | 480 |
| `H3_newline_8k` | post-commit | gix_open | 1186 | 1186 |
| `H3_newline_8k` | post-commit | head_hex | 623 | 623 |
| `H3_newline_8k` | post-commit | live_sessions | 1046 | 1046 |
| `H3_newline_8k` | post-commit | spawn_worker | 1838 | 1838 |
| `H3_newline_8k` | post-commit | total | 5492 | 5492 |
| `H3_newline_8k` | prepare-commit-msg | attribution_preview | 554 | 554 |
| `H3_newline_8k` | prepare-commit-msg | discover | 41 | 41 |
| `H3_newline_8k` | prepare-commit-msg | live_sessions | 693 | 693 |
| `H3_newline_8k` | prepare-commit-msg | open_projection | 4687 | 4687 |
| `H3_newline_8k` | prepare-commit-msg | predict_id | 22844 | 293 (parent, `redact` subtracted) |
| `H3_newline_8k` | prepare-commit-msg | raw_capture | 601 | 601 |
| `H3_newline_8k` | prepare-commit-msg | read_facts | 223 | 223 |
| `H3_newline_8k` | prepare-commit-msg | redact | 22551 | 22551 |
| `H3_newline_8k` | prepare-commit-msg | rule_packs | 23 | 23 |
| `H3_newline_8k` | prepare-commit-msg | tokio_build | 29 | 29 |
| `H3_newline_8k` | prepare-commit-msg | total | 30874 | 30874 |

### Phase audit

`total` is measured independently of the phases, so disjoint phases can never exceed it. `over-attributed` is therefore a proof that something is nested which the harness does not declare — not a rounding remark.

| arm | hook | total µs | attributed µs | unattributed µs | verdict |
|---|---|---|---|---|---|
| `B0_idle` | post-commit | 3020 | 2848 | 172 | adds up |
| `B0_idle` | prepare-commit-msg | 108 | 38 | 70 | adds up |
| `B1_live` | post-commit | 3218 | 3046 | 172 | adds up |
| `B1_live` | prepare-commit-msg | 18097 | 17434 | 663 | adds up |
| `G1_line_2k` | post-commit | 3638 | 3435 | 203 | adds up |
| `G1_line_2k` | prepare-commit-msg | 20419 | 19757 | 662 | adds up |
| `G2_line_4k` | post-commit | 3727 | 3517 | 210 | adds up |
| `G2_line_4k` | prepare-commit-msg | 21903 | 21134 | 769 | adds up |
| `G3_line_8k` | post-commit | 4085 | 3864 | 221 | adds up |
| `G3_line_8k` | prepare-commit-msg | 21567 | 20883 | 684 | adds up |
| `H1_newline_2k` | post-commit | 4101 | 3835 | 266 | adds up |
| `H1_newline_2k` | prepare-commit-msg | 23002 | 22287 | 715 | adds up |
| `H2_newline_4k` | post-commit | 6307 | 5955 | 352 | adds up |
| `H2_newline_4k` | prepare-commit-msg | 36788 | 35478 | 1310 | adds up |
| `H3_newline_8k` | post-commit | 5492 | 5224 | 268 | adds up |
| `H3_newline_8k` | prepare-commit-msg | 30874 | 29695 | 1179 | adds up |

**Machine class.** Every absolute here is one machine's measurement and moves about 2x with load, which is why each arm is quoted against a bare-commit control measured in the SAME run and the ratio is the portable number. Fresh Unix installs invoke unchained hooks through direct executable links; preserved user-hook chains and non-Unix installs retain shell shims. Process launch is part of the measured implementation, and nothing here may be generalised into a product claim about another machine.
