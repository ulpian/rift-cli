# replay-savings

- build: `2523c4e` (HEAD `2523c4e`)
- facts from: history at each window's commit (refs: main, current, prepurge)
- corpus: 1416 transcript files, 23 session trees, 421 context windows, 58942 tool calls
- tokenizer: cl100k_base
- started: 2026-09-29T23:25:57Z (1790724357)
- finished: 2026-09-29T23:40:33Z (1790725233)

```text
replay-savings — what this build's read gate would have decided
  schema 2 · tokenizer cl100k_base · aggregates only
  facts from: history at each window's commit

  session trees                                          23
  context windows                                       421
  tool calls replayed                                58,942
  every tool result (informational ceiling)          58,934     30,488,481
  revisions advanced / windows emptied                1,233             12
  paths answered at a window's commit / worktree      3,494            464
  windows with no commit at or before them                0

  repository-content calls, one bucket each
                                                      calls         tokens   of content
  first sight of (path, revision) in this window        798      1,351,682      28.54 %
  repeat inside coverage — DECLINED                      20         27,577       0.58 %
  repeat inside coverage, under the line floor           13          6,362       0.13 %
  repeat overlapping held coverage                      106        187,093       3.95 %
  repeat of a read revision, over unheld lines          225        290,881       6.14 %
  slice past the credit bound; nothing recorded          17        183,914       3.88 %
  result written to a file; a pointer reached the window       84         58,861       1.24 %
  recognised read with no usable line range               7          6,340       0.13 %
  read tool call outside the pinned argument set          2         10,763       0.23 %
  no name or line count for the path                    955      2,605,389      55.01 %
  the client's own repeat guard answered it               3          6,415       0.14 %
  name or content search                                  4            949       0.02 %
  read channels                                       2,230      4,735,277      99.98 %
  search channel                                          4            949       0.02 %
  repository content (the denominator)                2,234      4,736,226     100.00 %

  what the share is
  declinable by the journal                              33         33,939       0.72 %
  declined by this build                                 20         27,577       0.58 %
  declined by the client's own repeat guard               3          6,415       0.14 %
  declined, net of the reason frames                     20         24,331       0.51 %
  reason frames cost                                                 3,246
  declines resting on the window's own write              5
  declines lost to an unheld sibling read                10

  the two levers a replay has to model on purpose
  results with a response object / stating a delivery      259             46
  reads credited from the client's statement              0              0
  deliveries the statement took back                      1
  calls performing several reads / members              373          2,755

  what is left, and why
  first-read floor (never declinable)                   798      1,351,682      28.54 %
    of it, read again by a sibling window               141        544,621      11.50 %
    later windows' first reads of them                  285        544,621      11.50 %
      fork: began from another window's history           0              0       0.00 %
      paste: the spawning prompt carried the lines        0              0       0.00 %
      by name: the spawning prompt named the path       100        237,940       5.02 %
      none: nothing handed down                         185        306,681       6.48 %
  reads after a non-write edit (not modelled)            27         28,328       0.60 %
  invisible shell reads, loose bound                 41,135     15,610,553
    of them, naming a file this checkout has         12,754      7,107,308
  shell calls / of them recognised by the grammar    54,278          5,417
  reads outside the work tree (out of scope)          4,858      7,526,614
  unmeasured remainder (every other tool)                       25,752,255

  machine-format reads, by path shape
                                                      calls         tokens   of content
  lockfile                                                0              0       0.00 %
    of it, first sight                                    0              0       0.00 %
  minified                                                0              0       0.00 %
    of it, first sight                                    0              0       0.00 %
  notebook                                                0              0       0.00 %
    of it, first sight                                    0              0       0.00 %
  svg                                                     0              0       0.00 %
    of it, first sight                                    0              0       0.00 %
  tabular                                                 0              0       0.00 %
    of it, first sight                                    0              0       0.00 %

  first sight, by the size of the file read
  < 100 lines                                            99        133,382       2.82 %
  100–399 lines                                         159        305,003       6.44 %
  400–999 lines                                         237        525,360      11.09 %
  1,000–1,499 lines                                      57         88,930       1.88 %
  ≥ 1,500 lines                                         246        299,007       6.31 %
  < 4 KiB                                                47         39,798       0.84 %
  4–16 KiB                                              215        397,224       8.39 %
  16–64 KiB                                             311        646,555      13.65 %
  ≥ 64 KiB                                              225        268,105       5.66 %
  ≥ 400 lines or ≥ 16,384 bytes                         551        946,386      19.98 %
    of it, code                                         451        611,089      12.90 %
    of it, Markdown                                      96        331,414       7.00 %
    of it, TOML                                           0              0       0.00 %

  the constants this build decided with
  max slice bytes (decline ceiling)                   98304
  min slice lines (decline floor)                        28
  max delivered bytes, read tool                      47500
  max delivered bytes, terminal                       30000
  context lines around a changed site                     3
  max inline hash bytes                             1048576
  mode                                              enforce
  edit echo                                          pinned
  other tool names seen                                  17

  Reading it: the declined share is an estimated upper bound on what this
  build would have kept out of context, never a claim that the reads would
  have happened. Searches are in the denominator and the journal never
  declines one, so the read-only share is the larger of the two rates.
```
