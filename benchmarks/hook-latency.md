# Hook latency — the git-latency budget artifact

**Subject:** `fbeb55c` (release), sha256
`eb6a2128461819691ea4102cc632c0e255189d861133106528da106b26c76934`.
Small-store arms and the process probes are carried from `d216640` (sha256
`f91fc3350122add05a4be5c46d16c8f86c6f40fa6bb259507be1a6900fda1c98`), which
differs from the subject only in `rift-store`'s open path; each table says which
run it came from.
**Budget:** hook latency ≤ 100 ms warm, where *hook latency* is the owner's
git-latency definition — the time a `git commit` spends beyond a bare commit,
summed over all three of the commit's hooks (`prepare-commit-msg`,
`commit-msg`, `post-commit`).

> **§9 and §10 (2026-08-19) carry the first numbers from the tracked
> instrument, and the opted-in path breaches the hard budget.** §10 is the one
> the budget question turns on: the 2× worst-case corpus reaches **245.8 ms
> median, 2.5× the hard budget**, with `redact` at 74% of the hook-body time
> and the process floor unchanged between the twins. **§11 proposes
> `MAX_TOTAL_BYTES = 8 KiB` with its margin justified, and records that no cap
> makes the budget safe at p95.** Read those before §2–§8.
>
> **UNCONFIRMED BY THE PRODUCT — read this before any byte figure below.**
> `explain_files_read` is `null` in **135 of 135** recorded values across the
> committed run artifacts. The `files read (explain)` column has never once been
> answered, so **every "bytes read" figure in §9–§12 — including the
> `1,048,576 B, the ceiling exactly` claim that anchors the budget argument — is
> the harness's own arithmetic over the corpus it wrote, agreed with by nothing
> the product reported back.** The corroboration probe built to prevent exactly
> that failed *silently*, which is worse than no probe: it occupied the slot
> where a check would go, and it is the one instrument that would have caught
> the `MAX_TOTAL_BYTES` cut on its own. The timings are measurements; **the byte
> columns are declarations.** A per-run artifact now prints this beside its own
> numbers, but that reaches future runs only — this notice is here because this
> is the document the claims are read from.
>
> **Provenance, read this first.** Every number in §2–§8 comes from an
> instrument that no longer exists and cannot be re-run (§1). They are kept as
> a record, including of a breach that was real; they are not a protocol a
> second person can repeat. The tracked replacement is
> `benches/native-baseline`'s `native-baseline hook-latency`, and it publishes into
> `benchmarks/hook-latency/`.

## Verdict

**The budget is met on both store sizes.** It was not when this artifact was
first written: a 52 MB projection breached it at 118.3 ms, one call was named,
the store fixed it (`fbeb55c`), and the arm was re-measured. The history is kept
deliberately — see §4 and §8.

| store | arm | hook latency | run | verdict |
|---|---|---|---|---|
| 405 KB | hooks installed, nothing opted in | 50.1 ms | 2 | meets |
| 405 KB | live session, native archive | 61.7 ms | 2 | meets |
| 405 KB | live session, native, no keychain | 54.2 ms | 2 | meets |
| 52 MB | hooks installed, nothing opted in | 47.2 ms | 3 | meets |
| **52 MB** | **live session, native archive** | **57.5 ms** | **3** | **meets** (was 118.3, run 2) |
| 405 KB | live session, wrapper route | 103.1 ms | 2 | breach — see §5 |

**One documented exception, and it is one-time.** The commit that *applies*
migration v10 to an existing store pays a 64.29 ms index build inside
`prepare-commit-msg` — about 124 ms of hook latency for that single commit, once
per repository, and 2 ms per open forever after (§4). Stated here rather than
buried, because this program has already once mistaken a one-time cost for a
per-commit one.

## 1. Protocol

**CORRECTED 2026-08-19. The sentence that stood here was false, and
it was the load-bearing sentence of the section.** It read *"Reproducible by a
second person; every number below comes from these commands"*, and the commands
it then listed were `decompose.py` and `certify-tip.sh` — **two scripts that
were never committed to this repository or any other, and are unrecoverable**.
They existed in a working directory that no longer exists. So from the moment
that shell closed, a tracked artifact backing a hard budget claimed
reproducibility that nobody could exercise — and the claim went on standing,
because a missing file reddens nothing. That is the general lesson and it is
not about these two scripts: **an instrument that is not in the tree is not an
instrument**, and a protocol section naming one is a promise with no guard
behind it.

Two consequences, and they are different:

1. **The instrument now exists, tracked and buildable** — the standing rule
   being that a claim's instrument ships in the tree beside it:

   ```bash
   cd benches/native-baseline
   cargo run --release -- hook-latency --iterations 11 --warmup 3
   ```

   It writes JSON + Markdown into `benchmarks/hook-latency/`. It runs a bare
   commit control (`A_bare`) that **cannot be filtered out**, an idle arm, a
   live arm, and six `.worktreeinclude` opt-in arms — **paired** cheap/worst
   twins reading identical bytes:

   | arm | bytes read | handed on |
   |---|---|---|
   | `G1_line_2k` / `H1_newline_2k` | 2,048 | ~1.0× / ~2.0× |
   | `G2_line_4k` / `H2_newline_4k` | 4,096 | ~1.0× / ~2.0× |
   | `G3_line_8k` / `H3_newline_8k` | 8,192 (the ceiling exactly) | ~1.0× / ~2.0× |

   **These six have been executed exactly once**, by the sweep of 2026-08-30
   (`hook-latency-1788101269`). The fence at the end of this file publishes all
   six at `runs: 1`, so each figure is a single run's median and cannot tell a
   slow arm from a busy machine; two of them — `H2_newline_4k` at 111.39 ms and
   `H3_newline_8k` at 102.01 ms — are over the budget. Read that as a finding
   owed three clean runs, not as a verdict. They are sized for the
   shipped-cap regime (`MAX_TOTAL_BYTES` 8 KiB, `MAX_FILE_BYTES` 4 KiB);
   the arms that produced §9–§11 were sized for the 1 MiB regime and all but
   `F1_newline_8k` would now be declined — **most by the per-file limit, but
   `F2`/`F3`/`F4` by the total budget instead**, since their 2,048/3,072/4,096 B
   files are all within the 4 KiB per-file cap. They were removed rather than
   left describing an instrument that no longer runs them. *(This paragraph
   first said "every one of them" and "file-by-file"; §9 was right and it was
   wrong on both counts — `F1` is fully capturable at 8,192 B total, 1,024 B per
   file, 8 files.)* **The sizes in this
   table are pinned against `arms()` by
   `section_one_states_the_arms_the_instrument_actually_runs`, so the next
   re-size reddens this prose instead of outliving it.**

   It publishes the oracles beside every timing, phases from one extra untimed
   commit with `RIFT_HOOK_TIMING=1`, and the subject binary's modification time
   against the checkout's newest source, so a number taken from a stale binary
   is refused rather than published.

2. **Runs 2 and 3 below are HISTORICAL and are not reproducible by a second
   person.** They are kept because they are true records of what was measured
   and of a real breach that was found and fixed — not because anyone can
   re-derive them. Any statement in this document that implies otherwise about
   §2 through §8 is superseded by this paragraph. Numbers produced by the new
   instrument will be published as their own runs, under their own headings,
   and will not be blended with these.

**What the two runs were.** Run 2 = the seven-arm sweep at `d216640` (sha256
`f91fc335…`), which found the breach. Run 3 = the closing measurement at
**`fbeb55c`** (sha256
`eb6a2128461819691ea4102cc632c0e255189d861133106528da106b26c76934`), scoped to
the breached arm and its controls. Both: 11 timed commits after 3 warm-ups per
arm, each arm a fresh repository with rift's hooks installed and an isolated
`RIFT_CACHE_DIR`; `A_bare` measured in the same run regardless of any arm
filter, because an arm quoted against a control from a different run is the
error the same-run control rule exists to prevent.

**Conditions of the runs reported here:** one machine (i5-8279U, macOS 24.6.0),
one session, idle — load1 **1.65** at run 2's arms and **2.13** at run 3's, **2.59–2.73** at the probes;
nothing else running, the machine quiet by arrangement. Absolutes move ~2x with
machine load, so **every arm is quoted against a bare-commit control measured in
the same run**, and the ratio is the portable number.

## 2. Arms

`hook latency` = arm p50 − bare control p50, same run. `× bare` is the ratio.

| arm | p50 (ms) | min | hook latency | × bare | projection |
|---|---|---|---|---|---|
| bare commit, no hooks (control) | 37.6 | 35.9 | — | 1.00x | — |
| hooks installed, nothing opted in | 87.7 | 84.8 | 50.1 | 2.33x | 405 KB |
| + live session, native archive | 99.3 | 94.5 | 61.7 | 2.64x | 405 KB |
| + live session, native, `RIFT_NO_KEYCHAIN` | 91.8 | 89.6 | 54.2 | 2.44x | 405 KB |
| + live session, wrapper route | 140.7 | 136.1 | **103.1** | 3.74x | 405 KB |
| hooks installed, nothing opted in | 84.8 | 83.0 | 47.2 | 2.26x | **52 MB** |
| + live session, native archive | 155.9 | 150.6 | **118.3** | 4.15x | **52 MB** |

### Run 3 — `fbeb55c`, the breached arm and its controls

| arm | p50 (ms) | min | hook latency | × bare | projection | schema |
|---|---|---|---|---|---|---|
| bare commit, no hooks (control) | 37.2 | 36.5 | — | 1.00x | — | — |
| hooks installed, nothing opted in | 84.4 | 82.9 | 47.2 | 2.27x | 55.7 MB | v10 |
| + live session, native archive | 94.7 | 93.2 | **57.5** | 2.55x | 55.7 MB | v10 |

The prediction on record before this run was 58–65 ms; it landed at **57.5**,
half a millisecond under the low end. The store grew 52.3 → 55.7 MB: the partial
index is ~3.4 MB.

## 3. Phase breakdown (from the shipped binary, `RIFT_HOOK_TIMING=1`)

> **NESTED PHASES: subtract, never sum. `predict_id` INCLUDES `redact`.**
> Added 2026-08-19, before the first sweep that can see it. `HookTimer::phase`
> records on *completion*, so an inner phase prints before its parent and the
> parent's figure already contains it, while `report_line`'s own doc invites
> summing by key — which is right for a *repeated* key (a call site that ran
> twice) and wrong across a containment pair. `redact` inside `predict_id` is
> the instrument's first nested phase, so nobody had hit this before, and
> summing would over-report by **exactly the redaction**: the one quantity the
> opted-in arms exist to isolate, in the one hook that cannot detach. A
> benchmark whose total exceeds its parts by precisely the quantity under
> investigation is the worst failure available here.
>
> **This artifact reads phases as EXCLUSIVE**: every declared child is
> subtracted from its parent, so the values are disjoint and may be added. The
> sweep publishes both columns (`recorded` as the binary printed it,
> `exclusive` after subtraction) and audits them against the independently
> measured `total_us` — disjoint phases can never exceed it, so
> *over-attributed* is a **proof** of an undeclared nesting rather than a
> rounding remark.
>
> The tables below predate the nested phase and are unaffected: their
> `predict_id` figures are whole-call numbers with no `redact` recorded beside
> them, so nothing here has been double-counted.

Medians of 11. `commit-msg` is not instrumented; its body is a message read,
a normalize and a conditional write, and it does not appear in any measurement
above the noise floor.

| phase | 405 KB store | 52 MB, before `fbeb55c` | 52 MB, after |
|---|---|---|---|
| **prepare-commit-msg body** | **8.71** | **68.92** | **11.76** |
| ↳ `open_projection` | 2.02 | **59.95** | **2.03** |
| ↳ `predict_id` (redaction + hashing) | 5.69 | 5.72 | 5.71 |
| ↳ `attribution_preview` | 0.11 | 1.65 | 1.66 |
| ↳ `read_facts` | 0.11 | 1.37 | 1.55 |
| ↳ `live_sessions` | 0.11 | 0.11 | 0.11 |
| ↳ `rule_packs` | 0.02 | 0.01 | 0.01 |
| ↳ `raw_capture`, `tokio_build`, `discover` | 0.01–0.03 | 0.01–0.03 | 0.01–0.03 |
| **post-commit body** | **2.65** | **2.83** | **2.73** |
| ↳ `spawn_worker` | 1.16 | 1.24 | 1.21 |
| ↳ `gix_open` | 0.57 | 0.58 | 0.58 |
| ↳ `enqueue` | 0.36 | 0.37 | 0.37 |
| ↳ `head_hex` | 0.30 | 0.30 | 0.29 |
| ↳ `live_sessions` | 0.09 | 0.09 | 0.09 |

Two results worth stating because they retire proposals rather than support them:
**`tokio_build` is 0.02 ms** (a hook does not need an async runtime, and removing
it would buy twenty microseconds), and **`rule_packs` is 0.01–0.02 ms** on a
repository with no packs — the redactor-pack load added to `prepare-commit-msg`
does not move the warm number.

## 4. The breach — named, fixed, re-measured

### It was one call, and two instruments agreed to 0.5%

* in-process, inside the hook: `open_projection` = **59.95 ms**;
* out of process, against the same file:
  `SELECT DISTINCT agent, session_id FROM agent_hook_event WHERE session_id IS
  NOT NULL ORDER BY agent, session_id` = **60.23 ms p50 / 57.34 min**, returning
  **59 rows** out of **57,463** in `agent_hook_event`. A trivial open-and-read on
  the same file is **0.69 ms**.

`rift_store::open_capture_projection` called `backfill_capture_session_identities`
on **every open**: a full scan of the largest table in the store, run to insert
59 rows that were already there, on **the one hook that cannot be detached** —
the trailer has to be in the commit message. `post-commit` no longer opens the
projection at all (which is why the 52 MB idle arm was already the fastest of the
six), so the whole breach lived in `prepare-commit-msg`.

### The fix — `fbeb55c`, in `rift-store`

Three halves, all of them: the backfill now runs behind a coverage guard (an
anti-join over the session table), migration **v10** appends a partial index
`idx_agent_hook_event_identity ON agent_hook_event(agent, session_id) WHERE
session_id IS NOT NULL`, and the scan that still fires for a pre-live-path store
walks that index instead of the table. `rift-store` is not this harness's to
change, so this was raised with the owner, carrying the measurement and three
candidate shapes, rather than patched here.

### Re-measured (run 3)

| | before `fbeb55c` | after |
|---|---|---|
| `open_projection`, 52 MB store | **59.95 ms** | **2.03 ms** |
| `prepare-commit-msg` body, 52 MB | 68.92 ms | 11.76 ms |
| hook latency, 52 MB live+native | **118.3 ms** | **57.5 ms** |

The term did not shrink, it **left**: 2.03 ms on a 52 MB store against 2.02 ms on
a 405 KB one — the open no longer scales with the store at all.

### The migration itself, priced

The real-use source store was **already at v10** when run 3 copied it, so those
arms exercise the *guarded steady-state* open and not the apply path. Saying so
and stopping there would leave the more interesting half unmeasured, so a
separate probe rolled a copy back to v9 (dropping the index and the ledger row)
and opened it through a real hook:

| open | `open_projection` | ledger |
|---|---|---|
| first, applies v10 over 57,559 rows | **64.29 ms** (hook body 73.75 ms) | v9 → **v10**, index present |
| the seven after it | **2.11 ms** p50, 1.96 min | v10 |

So the fix costs **one 64 ms open per repository**, on the first hook after the
upgrade — roughly 124 ms of hook latency for that single commit, over budget
once — and 2 ms per open forever after. It is called out in the verdict rather
than left in a table, because a one-time cost read as a per-commit one is a
mistake this program has already made once.

## 5. The wrapper-route arm

103.1 ms of hook latency with hook bodies totalling **11.5 ms**, so the excess is
outside them. The mechanism is **an ARGUMENT, not a measurement**: this route's
archive fails by construction on this machine (the pinned binary has no archive
verb at all), so every commit's worker retries the backlog and competes with the
next commit's hooks. Consistent with `queue_left = 14`, and with the native arm's
`queue_left = 0` at 61.7 ms — but not isolated, and I did not isolate it because
**this route has since been deleted outright**. Recorded so the number is not
mistaken for the native path's.

## 6. Oracles — what stops these numbers being true about the harness only

A hook that got fast by ceasing to work would look identical in every timing
above. Four checks ride every run and are printed beside the timings:

1. **Work landed.** `archive_decision` and `file_revision` rows in each arm's
   projection: **26 and 26 for 26 commits** on the native arm — the detached
   worker condensed every commit after git returned. On the pre-detach tree the
   same arm recorded 26 and 26: the work moved, it did not disappear.
2. **The live path was taken.** The `Rift-Checkpoint` trailer is present in every
   live arm and **absent in every idle arm** — reported for both, since a check
   that only ever confirms is not a check.
3. **The archive left the hook path.** git child processes per commit: **12 → 3**
   across the detach (the nine the archive spawns), 3 in every arm here.
4. **The arm is what it claims to be.** `projection_bytes` is stat'ed from the
   file rift actually opened: **52,269,056 / 52,264,960** for run 2's large-store
   arms and **55,676,928 / 55,672,832** for run 3's (the partial index adds
   ~3.4 MB), **405,504** for the rest. This probe exists because its absence hid
   a defect — see §8.
5. **The store reached the schema the arm assumes.** `MAX(version)` from
   `_migrations` on the file rift opened: **v10** in both run-3 arms. The apply
   path is proved separately in §4, since the copied store arrived already
   migrated.

Queue state, with the qualification that travels with it: **`queue_left` = 0 in
every arm whose archive can succeed; in run 2 the wrapper arm left 14 and the
52 MB live arm left 1, each entry at ≤2 attempts against a bound of 3, and in
run 3 the 52 MB live arm left 0** — with the open term gone, the worker keeps up.

## 7. The process floor

40 iterations per timing, 7 repeats, median.

| probe | p50 (ms) | min |
|---|---|---|
| `exec /usr/bin/true` (1 process) | 3.48 | 3.29 |
| `sh -c 'exec /usr/bin/true'` (2 processes) | 9.07 | 8.86 |
| `rift … post-merge` — a hook name no capture body matches | 10.51 | 9.27 |
| **the installed shim, as git runs it** | **15.91** | 14.89 |

A `git commit` runs three hooks, so **47.7 ms per commit is process creation**
before any hook body executes — against hook bodies totalling ~11.4 ms on a small
store. Rift's own startup over a trivial exec is **7.0 ms**; the rest was
`/bin/sh` and the exec in the implementation measured here.

> **Superseded implementation note (2026-08-26).** Fresh Unix installs now use
> direct executable links for unchained hooks; preserved user-hook chains and
> non-Unix installs retain the shell shim. These numbers remain the historical
> evidence for the shim implementation, not a claim about the current Unix
> process floor.

Historic, from the certified before/after at `086a682` (same machine, same
protocol, idle): the shim's `$(dirname "$0")` → `${0%/*}` rewrite moved the
installed shim from **19.34 → 15.78 ms**, i.e. 3.56 ms per hook and 10.7 ms per
commit.

## 8. What this artifact corrected about itself

**The two "50 MB real-store projection" arms were not large-store arms at all.** The
harness copied the 52 MB projection to `<cache>/<repo-id>/projection.db` while
rift reads `<cache>/rift/<repo-id>/projection.db`, so rift created a fresh 405 KB
store and the large file sat beside it unread. Every earlier claim about a large
projection — including the certified sentence "**+1.5 ms** against the real 50 MB
store" — was measured on a fresh store and is **withdrawn**. The true cost is
**+58 ms on `prepare-commit-msg`**, and it is a budget breach rather than a
rounding difference.

It was found by checking `archive_decision` counts that should have grown and had
not, i.e. by an oracle disagreeing with the harness — not by review. The
`projection_bytes` probe in §6.4 is the mechanization of that lesson: an arm now
states what it exercised, in the same JSON as its timing.

The before/after history for the detach itself is unaffected: it was measured on
small-store arms at `086a682` and is cited above rather than re-run.

Nothing above is softened by the fix that followed it. The +58 ms was real, it
was a breach, and it shipped: it reached a user-visible budget because a harness
defect hid the arm that would have caught it for three consecutive runs. §4
prices the repair; this section is why the repair was needed at all.

## 9. The opted-in path, measured — 2026-08-19, four runs

> **REGIME: `MAX_TOTAL_BYTES = 1 MiB`, `MAX_FILE_BYTES = 256 KiB` (flat).** §9–§11 were
> all taken before the owner cut the cap to **8 KiB / 4 KiB**. They are
> the evidence that **produced** that cut, so they are kept exactly as
> measured — but **no arm in them is reproducible now**: under a 4 KiB per-file
> limit every corpus below except `F1` would be declined — `F5` and everything
> at 50 KB and above by the **per-file** limit, but `F2`/`F3`/`F4` (8 × 2,048,
> 8 × 3,072, 8 × 4,096 B) by the **total** budget instead, since each of their
> files is within the 4 KiB cap. The
> harness's arm set has been re-sized to the new regime, and §9–§11 are the
> pre-cap record.
>
> *(Corrected: these labels first said `512 KiB`. The pre-cap `MAX_FILE_BYTES`
> was a **flat** `256 * 1024`; `MAX_TOTAL_BYTES / 2` is the rule that arrived
> **with** the cut, and applying it backwards invented a limit that regime never
> had. No conclusion moves — every pre-cap arm was ≤ 65,536 B/file — but it is a
> wrong number inside a freshly-added provenance label, which is exactly the
> class this round was about.)*

**This section is produced by the tracked instrument** (`native-baseline hook-latency`,
§1), not by the vanished scripts. Subject: `rift` built from `4e45efa` plus the
benches-only commits on top of it, mtime `1787120056`, admitted by the freshness guard
(the same guard refused the previous build by name after the rebase — see §9.5).
Artifacts: `benchmarks/hook-latency/hook-latency-{1787120240,1787120379,
1787120478,1787120545}.{json,md}`. Box confirmed clear at the start and end of
every run: `pgrep -x cargo` 0, `pgrep -x rustc` 0, `pgrep -x cargo-clippy` 0.

### 9.1 What was measured, and what the prediction was

`git grep -n worktreeinclude -- benches benchmarks` returned **zero** before
this run: the 100 ms hard budget had never been measured with anything opted in,
at any size. The prediction recorded before measuring was **140–320 ms at the
1 MiB ceiling**.

### 9.2 The four runs

Four identical runs of the same binary, one sitting, 3 warm-ups + 11 measured
commits per arm. `latency` is arm p50 − control p50 in the same run.

| arm | latency ms, 4 runs (sorted) | median | × bare (sorted) | median | over 100 ms |
|---|---|---|---|---|---|
| `B0_idle` | 52.55 · 55.56 · 58.75 · 77.71 | **57.2** | 1.85 · 2.01 · 2.39 · 2.59 | 2.20× | **0 / 4** |
| `B1_live` | 85.34 · 96.18 · 115.50 · 155.38 | **105.8** | 2.46 · 2.87 · 2.97 · 4.88 | 2.92× | 2 / 4 |
| `C1_include_50k` | 99.65 · 108.81 · 109.43 · 141.73 | **109.1** | 2.71 · 2.77 · 3.73 · 3.90 | 3.25× | 3 / 4 |
| `C2_include_250k` | 109.02 · 109.38 · 131.26 · 163.69 | **120.3** | 2.77 · 3.24 · 3.25 · 5.09 | 3.25× | **4 / 4** |
| `C3_include_1mib` | 171.16 · 175.99 · 180.01 · 198.77 | **178.0** | 3.86 · 3.93 · 5.07 · 5.49 | 4.50× | **4 / 4** |

### 9.3 Verdict — the ceiling arm breaches, and the prediction was right

**`C3_include_1mib` breaches the 100 ms hard budget in all four runs**, at
171.16–198.77 ms, and is the slowest arm in every run. That lands inside the
predicted 140–320 ms band, in its lower half. `C2_include_250k` breaches 4/4.
`C1_include_50k` breaches 3/4 — its fourth run landed at 99.65 ms, 0.35 ms under
the line, which is a coin-flip and is reported as one rather than rounded either
way. **The corpus was not adjusted in response, and the arm sizes are exactly
what §9.6 declares.**

`B0_idle` — hooks installed, nothing opted in — **meets in all four runs**
(52.55–77.71 ms). The budget is not breached by installing Rift; it is breached
by opting bytes in.

`B1_live` with **no** corpus straddles the line (85.34–155.38, 2/4 over) and
**no verdict is claimed for it from four runs**. Its own spread is wider than
the gap between it and `C1`, so anyone reading a single `B1_live` number as
"the live path costs X" is reading noise.

### 9.4 Where the time goes — and the column that decides what a fix can be

`predict_id` here is **exclusive of `redact`** (§3), so the columns are disjoint
and may be added. From the arm's phase commit; see §3 on the two populations.

| arm | bytes read | handed on | ×read | git latency | hook bodies | outside bodies | `redact` | `predict_id` less `redact` |
|---|---|---|---|---|---|---|---|---|
| `B0_idle` | — | — | — | 55.55 | 3.01 | **52.54** | 0.00 | 0.00 |
| `B1_live` | — | — | — | 155.38 | 44.46 | **110.92** | 30.09 | 0.10 |
| `C1_include_50k` | 51,200 | 52,696 | 1.03× | 109.43 | 28.61 | **80.81** | 18.43 | 0.45 |
| `C2_include_250k` | 256,000 | 263,480 | 1.03× | 163.69 | 55.82 | **107.87** | 38.52 | 1.66 |
| `C3_include_1mib` | 1,048,576 | 1,066,352 | 1.02× | 180.00 | 90.53 | **89.47** | 66.01 | 8.25 |

**`redact` owns the opted-in cost.** At the ceiling it is 66.01 ms of a 90.53 ms
hook-body total — 73% of everything the hooks do — and it scales with the corpus
(18.43 → 38.52 → 66.01 ms across 51 KB → 250 KB → 1 MiB) while the id derivation
around it stays small (0.45 → 1.66 → 8.25 ms). Before the `predict_redaction`
seam split them, these two were one number and this sentence could not have
been written.

**In these pre-direct-link measurements, `outside bodies` was roughly half the
breach.** It is git latency minus the three hook bodies: at that revision, three
`/bin/sh` invocations, three execs, and git's own cost of running three hooks —
80–111 ms here, consistent with §7's independently measured 47.7 ms floor plus
this run's heavier load. The 2026-08-26 direct-link implementation removed the
shell term for fresh unchained Unix hooks; these rows remain historical evidence
for the revision they identify. Even if `redact` were free at that revision, the
ceiling arm would still have sat near the budget.

### 9.5 Which side of the 2× every number is on — stated, not implied

`MAX_TOTAL_BYTES` is 1 MiB charged on bytes **read**; `Walk::file` prefixes every
content line with a space, so N bytes contribute up to **2N + 2** and a full
budget can hand on a document just over 2 MiB. **Every arm above is at
1.02–1.03×, the low end.** These corpora are 63-character lines; the 2× end is a
file that is all newlines, where the prefix is the whole line. So the measured
breach is the *cheap* side of the ceiling: a pathological corpus reads the same
1 MiB and hands the redactor nearly twice as much, and `redact` is the term that
scales. **Nothing here measures that case, and no number above may be quoted for
it.**

### 9.6 Arm sizes

`C1` 8 files × 100 lines; `C2` 40 × 100; `C3` 16 × 1024. All lines 63 characters
plus a newline, so `C3` reads 16 × 1024 × 64 = **1,048,576 B — the ceiling
exactly**, not near it. Corpora are git-ignored and opted back in by
`.worktreeinclude`, which is the surface under test.

### 9.7 Oracles — these arms did the work they claim

Every live arm: checkpoint trailer **present**, 12–14 checkpoints re-derived,
13–15 shards redaction/Merkle-checked, **60 durable events**. `B0_idle`: trailer
**absent**, 0 events — the negative control, reported because a check that only
ever confirms is not a check. Every arm's projection was `stat`ed at the path
rift actually opened.

**One over-fire in this table, stated rather than left to be discovered.** The
"more than one projection" warning fires on every capture arm because the second
file is `rift/canopy/ui-state.db`, the machine-global Canopy registry — not a
second capture projection. The warning is doing its job (it enumerates rather
than counts) but its threshold is wrong; it should key on `projection.db`.

### 9.8 The finding that changes the perf gate: >10% on one run is not usable

The performance plan asked for a gate failing on >10% regression. **Measured,
on this box: the ratio ratchet at 10% fails 5 arms of 5 between two identical runs of
the same binary.**

```
native-baseline ratchet --baseline hook-latency-1787120545.json \
                  --candidate hook-latency-1787120240.json
B0_idle +19.0%  B1_live +98.2%  C1 +37.8%  C2 +56.6%  C3 +39.7%   → exit 1
```

The ratio is doing its job — the bare control itself moved **40.06 → 61.63 ms
across the four runs, a 54% spread**, and the ratio absorbs that. What it cannot
absorb is per-arm run-to-run variance, which reaches **+98%** between identical
runs. So the ratchet's design is right and its *sample size* is wrong: a usable
gate needs several runs a side and a median-of-medians, more iterations per arm,
or arms interleaved rather than run in blocks (block order confounds a slow arm
with a drifting machine). **A 10% gate over one run per side would red on noise
and teach everyone to ignore it.** Recorded here rather than in a ledger because
the next person to wire that gate needs the number.

### 9.9 What is still unmeasured

The pathological 2× corpus (§9.5). `B1_live`'s true cost, which four runs cannot
resolve. The 50 KB → 250 KB step: `C1` and `C2` have the **same** median ratio
(3.25×), so the curve is only resolvable at the ceiling. And a `rift init` flag
that had been dead since `5028107` — see §9.10.

### 9.10 The instrument had been unable to measure anything for weeks

The first sweep of this sitting failed on all five capture arms:
`rift init --no-provision` exited 2, *"unexpected argument `--no-provision`"*.
The flag was removed by `5028107` ("collapse the archive to the one route that
exists") and the harness went on passing it, so **every wired row's setup had
been failing since that commit** and nobody noticed, because nobody ran the
harness. It failed **loudly** — the arms recorded `FAILED`, not a fast number —
which is the one thing that went right. Fixed while this artifact was being written.

## 10. The 2× worst case, and the decision it produced

> **REGIME: pre-cap (1 MiB / 256 KiB), as §9.** No arm below is reproducible
> under the 8 KiB cap.

§9.5 said every arm measured was 1.02–1.03×, the cheap side, and that **no
number there may be quoted for the other end**. The owner took that at its word
and held the budget decision until this section existed. Four runs, nine arms,
box confirmed `pgrep -x cargo` / `-x rustc` / `-x cargo-clippy` = 0 at the start
and end of every run. Artifacts `hook-latency-{1787127649,1787127756,
1787127877,1787127988}.{json,md}`.

### 10.1 The design: pairs that differ in one variable

`C` arms are 63-character lines; `D` arms are **all newlines**, the constant's
own stated worst case — `Walk::file`'s one-space prefix *is* the whole line, so
N bytes read hand on 2N + 2. **Each D arm reads exactly the bytes its C twin
reads**, so a pair varies only what the redactor is handed.

| pair | bytes read | cheap hands on | worst hands on |
|---|---|---|---|
| `C1` / `D1` | 51,200 | 52,696 (1.03×) | 102,592 (**2.00×**) |
| `C2` / `D2` | 256,000 | 263,480 (1.03×) | 512,960 (**2.00×**) |
| `C3` / `D3` | 1,048,576 | 1,066,352 (1.02×) | 2,097,536 (**2.00×**) |

### 10.2 Results — median of four runs

| arm | latency ms, 4 runs (sorted) | median | × bare | over 100 ms |
|---|---|---|---|---|
| `B0_idle` | 56.49 · 57.53 · 57.75 · 60.51 | **57.6** | 2.10× | 0 / 4 |
| `B1_live` | 83.13 · 86.48 · 88.55 · 89.91 | **87.5** | 2.72× | 0 / 4 |
| `C1_include_50k` | 79.39 · 81.60 · 87.34 · 89.66 | **84.5** | 2.63× | 0 / 4 |
| `D1_newline_50k` | 85.02 · 94.19 · 106.96 · 111.92 | **100.6** | 2.98× | 2 / 4 |
| `C2_include_250k` | 95.91 · 116.41 · 117.57 · 126.52 | **117.0** | 3.22× | 3 / 4 |
| `D2_newline_250k` | 129.84 · 134.45 · 144.17 · 144.53 | **139.3** | 3.69× | **4 / 4** |
| `C3_include_1mib` | 140.76 · 165.16 · 167.73 · 172.15 | **166.4** | 4.21× | **4 / 4** |
| `D3_newline_1mib` | 242.00 · 243.94 · 247.62 · 265.44 | **245.8** | 5.94× | **4 / 4** |

### 10.3 What the 2× costs, from the same bytes read

| bytes read | cheap | worst | delta | ratio |
|---|---|---|---|---|
| 51,200 | 84.5 ms | 100.6 ms | +16.1 ms | 1.19× |
| 256,000 | 117.0 ms | 139.3 ms | +22.3 ms | 1.19× |
| **1,048,576** | 166.4 ms | **245.8 ms** | **+79.3 ms** | **1.48×** |

**The 2× document costs +79.3 ms at the ceiling — a 48% increase from reading
the identical number of bytes.** The penalty is not constant: it is 1.19× at
both smaller sizes and 1.48× at the ceiling, because the cost is in what is
handed on, not in what is read, and only at the ceiling does the handed-on
document reach ~2 MiB.

`D3_newline_1mib` is **245.8 ms median, 242.00–265.44 ms across four runs** —
the tightest band of any capture arm, while the bare control moved 42.53 →
62.72 ms (47%) underneath it. It breaches the 100 ms hard budget by **2.5×**,
and it sits in the upper half of the 140–320 ms predicted before any of this
was measured.

### 10.4 Where it goes — `redact`, decisively

| arm | git latency | hook bodies | outside bodies | `redact` | `predict_id` less `redact` |
|---|---|---|---|---|---|
| `C3_include_1mib` | 165.16 | 86.33 | 78.83 | 65.28 | 5.37 |
| `D3_newline_1mib` | 265.44 | 187.50 | 77.94 | **138.50** | 13.95 |

**`outside bodies` is unchanged between the twins — 78.83 vs 77.94 ms** — which
is what makes the rest attributable. *(Those two figures are run
`1787127988` alone, the most favourable of the four; in `1787127756` they are
32.28 vs 64.48. The conclusion survives at the median — 71.65 vs 71.89, with
`redact` 2.05× — and the median is the number to quote. Labelled rather than
re-picked.)* The process floor does not care what is in
the files, so the whole +100 ms of hook body between `C3` and `D3` is work,
not overhead. Of it, **`redact` alone doubles, 65.28 → 138.50 ms**, and is
**74% of the worst arm's legacy hook-body time**. The id derivation around it
goes 5.37 → 13.95 ms. Phase audit: "adds up" on every hook/arm pair, no
undeclared nesting.

### 10.5 Where the budget actually breaks

Reading 10.2 as a threshold rather than a table:

- **Nothing opted in never breaches** — idle 57.6 ms, live 87.5 ms, both 0/4.
  (The live figure is also the answer to §9's unresolved `B1_live`: on a
  quieter box it is 83–90 ms, a 7 ms spread, and the earlier 2/4 was noise.)
- **On the cheap side the budget survives 51 KB (0/4) and fails by 250 KB
  (3/4).**
- **On the worst-case side it is already marginal at 51 KB (2/4) and gone by
  250 KB (4/4).**

So the breach is not a property of opting in; it is a property of opting in
**more than roughly 50 KB**, and on a pathological corpus the margin at 50 KB
is a coin-flip. Every number here is a median of four runs on one machine and
none of it is a threshold anyone should encode without re-measuring.

### 10.6 The ratchet is REPORT-ONLY (the owner's call)

It computes and publishes a verdict and **does not fail a build**, because at
10% it fails 5 arms of 5 between two identical runs of the same binary. The
banner is printed by the renderer itself and re-emitted on exit, so a gate that
cannot fail says so in the same breath as its verdict. `--enforce` opts into
the failing exit.

**Correction, 2026-08-20.** Two sentences here were false about the code when
first written: `--enforce` was declared and **never read**, so the process
printed "this does not fail a build" and then failed the build — identically
with and without the flag, byte-identical output, exit 1 both times. The banner
was not re-emitted on the exit path either. Fixed the same day: verified
behaviourally, **exit 0 without the flag and exit 1 with it**, outputs
differing, and pinned by a test asserting the two outcomes differ rather than
asserting a string. What characterising the variance would take is recorded
in `ratchet::VARIANCE_NOTE`: four runs could not resolve the 50 KB → 250 KB
step in §9 (both arms landed on a 3.25× median), and a usable threshold needs
several runs a side reduced by median-of-medians, more iterations per arm, or
arms interleaved rather than run in blocks.

## 11. Where the budget crosses, and what `MAX_TOTAL_BYTES` should be

> **REGIME: pre-cap (1 MiB / 256 KiB), as §9.** These are the measurements that
> produced the 8 KiB cap; none of their arms is reproducible under it.

Thirteen sweeps. Box confirmed `pgrep -x cargo` / `-x rustc` / `-x cargo-clippy`
= 0 at the start and end of every one. **Worst-case (`line_width: 0`) arms only**,
per the owner's instruction: the cap must hold for the corpus a user really has.

### 11.1 The bracket I was given was wrong, and the measurement said so

An earlier reading put the crossing between `D1` (51,200 B, 2/4 over) and `D2`
(256,000 B, 4/4). With ten more runs **`D1` is 11/13 over, median 111.9 ms**.
So the crossing is **below** 51,200 B, and every rung of a ladder between D1 and
D2 was already past the budget before it started. The 2/4 was a small sample on
a quiet box, not a marginal size.

### 11.2 Below 50 KB the corpus is smaller than the machine

A ladder at 8 / 16 / 24 / 32 / 40 KB, three runs, `B1_live` (opting in
**nothing**) as the zero anchor:

| arm | bytes | latency ms (3 runs) | median |
|---|---|---|---|
| `B1_live` | 0 | 92.68 · 98.53 · 124.43 | 98.5 |
| `F1_newline_8k` | 8,192 | 96.34 · 114.10 · 136.22 | 114.1 |
| `F2_newline_16k` | 16,384 | 105.73 · 114.81 · 125.15 | 114.8 |
| `F3_newline_24k` | 24,576 | 106.73 · 126.42 · 127.20 | 126.4 |
| `F4_newline_32k` | 32,768 | 99.91 · 111.65 · 116.41 | 111.7 |
| `F5_newline_40k` | 40,960 | 101.13 · 110.85 · 115.72 | 110.9 |

**No trend.** Every rung's spread overlaps every other rung's, and 32 KB
measures *faster* than 24 KB. These runs were taken at load **12.0–16.7**
against **5.7–8.8** for §10, and `B1_live` — zero opted-in bytes — was itself
over budget in 1 of 3. This is §9.8's finding arriving where it matters: **the
variance is the box, not the arms**, and below ~50 KB the corpus contribution is
under the machine's own noise floor. **The crossing cannot be located more
finely than "at or below 51,200 B" on this hardware**, and no line fitted
through these six points would mean anything.

### 11.3 What is resolvable, and the rate that follows from it

Where the signal exceeds the noise, across every run of each arm:

| bytes read (2×) | n | median latency | over `B1_live` |
|---|---|---|---|
| 0 (`B1_live`) | 11 | 92.7 ms | — |
| 51,200 | 13 | 111.9 ms | +19.2 ms |
| 256,000 | 7 | 134.4 ms | +41.7 ms |
| 1,048,576 | 4 | 245.8 ms | +153.1 ms |

Near the low end that is **≈ 0.375 µs per byte read** at 2× (0.19 µs/B in the
middle, 0.15 µs/B at the ceiling — economy of scale, so the low-end rate is the
conservative one to size a cap with).

### 11.4 The proposal: `MAX_TOTAL_BYTES = 8 KiB`

**Headroom.** `B1_live` — hooks installed, session live, **nothing opted in** —
is 92.7 ms median over 11 runs (89.2 over the 8 quiet ones). Against a 100 ms
hard budget that leaves **7.3 ms** of median headroom, or 10.8 ms on a quiet
box. That headroom, not 1 MiB, is the whole budget available to opt-in.

**Margin, stated and justified.** A cap whose cost equals the headroom puts the
median *on* the line and fails about half the time — which is not a prediction,
it is what `D1` did: median 100.6 ms in the quiet session, 2/4 over. So the cap
is sized to spend **at most half** the measured median headroom:

- 7.3 ms ÷ 2 = 3.65 ms of corpus budget;
- at 0.375 µs/B that is ≈ 9,700 B;
- rounded down to a power of two: **8 KiB = 8,192 B, costing ≈ 3.1 ms**, about
  42% of the 11-run median headroom and 29% of the quiet-box headroom.

**16 KiB is the defensible upper bound** (≈ 6.1 ms, ~84% of median headroom) and
is only appropriate if the owner accepts protection at the median rather than
with margin. Anything above that is not supportable by this data.

8 KiB is a **128× reduction** from 1 MiB. Whether it is *enough for the feature*
is a product judgement, not mine — but the surface exists to fold `.env`-shaped
config back into capture, and the pre-Rift original's own file was two literal
paths.

### 11.5 The finding nobody asked for, and should have

**No value of `MAX_TOTAL_BYTES` makes the 100 ms budget safe at p95.** The
zero-opt-in live path is over budget in **3 of 11** runs on its own, before any
corpus exists, and `outside bodies` — process creation, the shim, git's cost of
running three hooks — is ~78 ms of it and no code change moves it. The cap
bounds what opting in can add; it cannot rescue a base path that already
consumes most of the budget. **A cap and a green gate are different claims**,
and only the first is available here.

### 11.6 What would sharpen this

The low ladder re-run on a quiet box (load < 6), which is the only way to
resolve 8 KB from 40 KB; and more runs per arm, since §9.8's variance note
applies with full force in exactly this region. The numbers above are medians
over 3–13 runs on one machine and **no one should encode a threshold from them
without re-measuring**.


## 12. The verdict is two facts, by owner decision

**`MAX_TOTAL_BYTES = 8 KiB` is ratified**, sized as §11.4 derives it. The
budget criterion reports **p50 met, p95 not met**, with the ~78 ms process
floor named as the cause, and the harness **cannot** print a single word for the pair: a
latency bar yields `BudgetVerdict::Latency { p50_met, p95_met }`, there is no
`fn met(&self) -> bool` to collapse it through, and the renderer emits
`p50 **met** · p95 **OVER**` beside a paragraph naming the floor. Collapsing
them would either credit the budget with a p95 it does not have, or fail a
median that genuinely meets it.

**A cap and a green gate are different claims, and only the first is
available** (§11.5): a cap bounds what opting in *adds*; it cannot rescue a base
path that already consumes most of the budget.

### 12.1 Two caveats that travel with the p95 verdict, not after it

**The p95 gap is partly the box.** The §11 ladder runs sat at load **12.0–16.7**
against **5.7–8.8** for §10, and on the quieter runs `B1_live` was 0/4 over
while on the busier ones it was 1/3. Some of the tail this verdict describes is
this machine being busy, not this code being slow.

**And this is one i5-8279U.** The ~78 ms floor is process creation, the shim and
git's own cost of running three hooks — **hardware**, not Rust. On a faster
machine the p95 gap may not exist at all. **Nothing here may be generalised into
a product claim about p95 latency**; it is one machine's measurement, and the
owner ruled with that in view rather than waiting for another.

### 12.2 A guard that could not fire, found by its own calibration

Deleting the `p50 met · p95 OVER` rendering and collapsing it to `**OVER**`
left the whole suite **green**: the one case that rule exists for was the one
nothing covered. The plant is what found it. It is now pinned by
`a_median_that_meets_and_a_tail_that_does_not_are_rendered_as_two_facts`, which
asserts the structured verdict, the rendered pair, and that the floor is named
beside it — re-planted afterwards and confirmed red.

## Gate-bearing facts

Generated by `native-baseline facts` from the JSON artifacts named in the fence below, and **the only gate-bearing numbers in this document**. Everything else in this file is commentary — it says what a number means, what produced it and what it retired, and it is not what the guard reads. `native-baseline facts --dry-run` prints the same numbers as a table.

**The reduction, stated because it is not the one the tables below use.** Each arm's figure is the nearest-rank median, across every committed artifact that measured that arm, of (arm p50 - that run's own control p50). Two consequences, and neither is a rounding difference:

1. **It pools sittings the sections keep apart.** Section 9 and section 10 both ran `C1_include_50k`, in different sessions at different machine loads, and report 109.1 ms and 84.5 ms. The fence pools all eight runs into one number, which is neither of those and is not a correction of either.
2. **Nearest-rank, not midpoint.** For an even number of runs the sections average the middle two; the harness returns an observed run. `D3_newline_1mib` is 245.8 ms in section 10.2 and 243.94 ms here, from the identical four runs, and both are right about their own definition. Nearest-rank is what every other percentile in this harness means, so it is what the fence means.

Read a section's own table for that session's finding; read the fence for what the committed artifacts, taken together, say.

```bench-facts
{
  "schema_version": 1,
  "budget_ms": 100,
  "family": "hook-latency",
  "artifacts": [
    1787120100,
    1787120240,
    1787120379,
    1787120478,
    1787120545,
    1787127649,
    1787127756,
    1787127877,
    1787127988,
    1787128560,
    1787128664,
    1787128771,
    1787128951,
    1787129088,
    1787129241,
    1787129490,
    1787129660,
    1787129832,
    1788101269
  ],
  "superseded_pending_rerun": [
    1787120100,
    1787120240,
    1787120379,
    1787120478,
    1787120545,
    1787127649,
    1787127756,
    1787127877,
    1787127988,
    1787128560,
    1787128664,
    1787128771,
    1787128951,
    1787129088,
    1787129241,
    1787129490,
    1787129660,
    1787129832,
    1788101269
  ],
  "superseded_reason": "Taken before the shipped caps (the owner cut MAX_TOTAL_BYTES from 1 MiB to 8 KiB roughly five hours after the last of these runs) and recorded under artifact schema 1, which carries no commit provenance. They are the evidence that PRODUCED that decision and are kept exactly as measured; they may not derive a floor and may not be quoted as current. Only a fresh quiet-machine sweep replaces them.",
  "control_p50_ns_median": 50048363,
  "arms": [
    {
      "arm": "B0_idle",
      "runs": 9,
      "git_latency_ns_median": 57532827,
      "over_budget_runs": 0
    },
    {
      "arm": "B1_live",
      "runs": 12,
      "git_latency_ns_median": 89906013,
      "over_budget_runs": 3
    },
    {
      "arm": "C1_include_50k",
      "runs": 8,
      "git_latency_ns_median": 89661788,
      "over_budget_runs": 3
    },
    {
      "arm": "C2_include_250k",
      "runs": 8,
      "git_latency_ns_median": 116412148,
      "over_budget_runs": 7
    },
    {
      "arm": "C3_include_1mib",
      "runs": 8,
      "git_latency_ns_median": 171163874,
      "over_budget_runs": 8
    },
    {
      "arm": "D1_newline_50k",
      "runs": 13,
      "git_latency_ns_median": 111919458,
      "over_budget_runs": 11
    },
    {
      "arm": "D2_newline_250k",
      "runs": 7,
      "git_latency_ns_median": 134447931,
      "over_budget_runs": 7
    },
    {
      "arm": "D3_newline_1mib",
      "runs": 4,
      "git_latency_ns_median": 243938059,
      "over_budget_runs": 4
    },
    {
      "arm": "E1_newline_64k",
      "runs": 6,
      "git_latency_ns_median": 114388918,
      "over_budget_runs": 5
    },
    {
      "arm": "E2_newline_96k",
      "runs": 6,
      "git_latency_ns_median": 110677742,
      "over_budget_runs": 6
    },
    {
      "arm": "E3_newline_128k",
      "runs": 6,
      "git_latency_ns_median": 119533449,
      "over_budget_runs": 5
    },
    {
      "arm": "E4_newline_192k",
      "runs": 6,
      "git_latency_ns_median": 132405823,
      "over_budget_runs": 6
    },
    {
      "arm": "E5_newline_250k_8f",
      "runs": 6,
      "git_latency_ns_median": 141567480,
      "over_budget_runs": 6
    },
    {
      "arm": "F1_newline_8k",
      "runs": 3,
      "git_latency_ns_median": 114097693,
      "over_budget_runs": 2
    },
    {
      "arm": "F2_newline_16k",
      "runs": 3,
      "git_latency_ns_median": 114806015,
      "over_budget_runs": 3
    },
    {
      "arm": "F3_newline_24k",
      "runs": 3,
      "git_latency_ns_median": 126418389,
      "over_budget_runs": 3
    },
    {
      "arm": "F4_newline_32k",
      "runs": 3,
      "git_latency_ns_median": 111652209,
      "over_budget_runs": 2
    },
    {
      "arm": "F5_newline_40k",
      "runs": 3,
      "git_latency_ns_median": 110854558,
      "over_budget_runs": 3
    },
    {
      "arm": "G1_line_2k",
      "runs": 1,
      "git_latency_ns_median": 59544523,
      "over_budget_runs": 0
    },
    {
      "arm": "G2_line_4k",
      "runs": 1,
      "git_latency_ns_median": 64379814,
      "over_budget_runs": 0
    },
    {
      "arm": "G3_line_8k",
      "runs": 1,
      "git_latency_ns_median": 68322355,
      "over_budget_runs": 0
    },
    {
      "arm": "H1_newline_2k",
      "runs": 1,
      "git_latency_ns_median": 71709028,
      "over_budget_runs": 0
    },
    {
      "arm": "H2_newline_4k",
      "runs": 1,
      "git_latency_ns_median": 111385758,
      "over_budget_runs": 1
    },
    {
      "arm": "H3_newline_8k",
      "runs": 1,
      "git_latency_ns_median": 102011326,
      "over_budget_runs": 1
    }
  ]
}
```
