# Hook latency — git-latency sweep

- harness `0.0.1`, artifact schema 1, generated 1787120100
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
| `A_bare` | 40.90 ms | 35.31 ms | — | 1.00× | control |
| `B0_idle` | — | — | — | — | FAILED — `/Users/ulpian/Documents/codebase/rift/rift-lane-substrate/target/release/rift init --no-provision --no-recall` exited with Some(2): error: unexpected argument '--no-provision' found

Usage: rift init [OPTIONS]

For more information, try '--help'.
 |
| `B1_live` | — | — | — | — | FAILED — `/Users/ulpian/Documents/codebase/rift/rift-lane-substrate/target/release/rift init --no-provision --no-recall` exited with Some(2): error: unexpected argument '--no-provision' found

Usage: rift init [OPTIONS]

For more information, try '--help'.
 |
| `C1_include_50k` | — | — | — | — | FAILED — `/Users/ulpian/Documents/codebase/rift/rift-lane-substrate/target/release/rift init --no-provision --no-recall` exited with Some(2): error: unexpected argument '--no-provision' found

Usage: rift init [OPTIONS]

For more information, try '--help'.
 |
| `C2_include_250k` | — | — | — | — | FAILED — `/Users/ulpian/Documents/codebase/rift/rift-lane-substrate/target/release/rift init --no-provision --no-recall` exited with Some(2): error: unexpected argument '--no-provision' found

Usage: rift init [OPTIONS]

For more information, try '--help'.
 |
| `C3_include_1mib` | — | — | — | — | FAILED — `/Users/ulpian/Documents/codebase/rift/rift-lane-substrate/target/release/rift init --no-provision --no-recall` exited with Some(2): error: unexpected argument '--no-provision' found

Usage: rift init [OPTIONS]

For more information, try '--help'.
 |

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

## Oracles — what each arm exercised

Reported for every arm including the control, because a check that only ever confirms is not a check: the idle arm's **absent** trailer is what makes the live arm's present one mean something.

| arm | projections | trailer | verify | events |
|---|---|---|---|---|
| `A_bare` | none | absent | — | — |
| `B0_idle` | none | absent | — | — |
| `B1_live` | none | absent | — | — |
| `C1_include_50k` | none | absent | — | — |
| `C2_include_250k` | none | absent | — | — |
| `C3_include_1mib` | none | absent | — | — |
