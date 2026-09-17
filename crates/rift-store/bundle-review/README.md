# Bundle pass-proof: the blind reader vs. the shipped fixes

This directory is the evidence behind the bundle work's pass-proof
obligation. A **blind** reader — a Python script written from the bundle
format document alone, never from Rift's own code, kept out of the
repository and **not** edited to produce these results — was run against
three FRESH bundles exported by the `rift` binary built from the tree that
carried the fixes (tip `4366cf1f…`), never the bundles the original review
shipped.

## Recipe

A synthetic repository (`git init`, three commits, each followed by
`rift hooks git post-commit --git-dir …` under a distinct
`.git/rift-sessions/<id>.json` session marker, then `rift doctor --fix` to
drain the detached archive) produced a real 3-position chain. From it:

- `full`  — `rift bundle export --out full.riftbundle` (whole chain, 3 links, from origin)
- `incr`  — `rift bundle export --since <cursor at position 1> --out incr.riftbundle` (2 links)
- `empty` — `rift bundle export --since <cursor at position 3, i.e. the current head> --out empty.riftbundle` (0 links, `from == to`, **non-origin** — the exact case the head-field defect was about)

Each was then run through the blind reader unedited:
`bundle_reader.py <file> --git --deep --repo <fresh target repo>`.

## Results

| Bundle | Verdict | Pass | Fail | Info |
|---|---|---|---|---|
| `full`  | ACCEPTED | 26 | 0 | 1 |
| `incr`  | ACCEPTED | 26 | 0 | 1 |
| `empty` | **REFUSED** | 12 | 1 | 3 |

Full output of each run is in `full.verdict.txt`, `incr.verdict.txt`,
`empty.verdict.txt` (verbatim stdout + exit code, nothing edited).

## The one failure is expected, not a regression

`empty.verdict.txt`'s single `FAIL` is `agree.5_chain_head`: the reader
computes `expect_head = "" if not seg.links else seg.links[-1][0]` (its own
`GUESS 10`, frozen at the time of the review — verification step 5's "last
link id" reading), while the fresh `empty.riftbundle` carries the REAL tip
id in `chain_head` (the field table's reading, which is also what the
shipped binary has always written and read). That IS the head-field defect:
the two doc rules disagree only at an empty window on a non-origin cursor,
and the reader is pinned to the reading `docs/sync-format.md` no longer
states after the fix. The reader was NOT changed to force agreement (the
task's own mandate); this divergence is the artifact the pass-proof was
asked to surface, not conceal.

Everything else agrees, on all three bundles, including the properties the
fixes touch directly:

- `refs.6_legal_names` / `refs.6_legal_oids` — the reader's own `legal_ref`
  (its `GUESS 9`) and the shipped `is_legal_ref_name` independently
  implement the same fully-qualified + `check-ref-format` rule, and both
  accept this repository's real refs.
- `git.ref_list_matches_bundle_heads(GUESS13)` — the reader's own
  cross-check (`git bundle list-heads` vs. `ref_list`) passes on both
  non-empty bundles, the same check `cmd_import` gained for the same
  reason.
- `chain_ref.*` (the `--deep` cross-checks) — every link sits at its claimed
  `chain_seq` and the chain ref's own trailers are all present, over a REAL
  3-commit chain, not a synthetic fixture.

## FP-census cost (measured before this directory was committed)

Before adding this directory: `entropy.high 424` (582 files, 9,844,931 bytes,
499 spans) via `cargo test -p rift-shadow-redact --lib
the_recorded_false_positive_baseline_holds_over_this_repositorys_own_text --
--nocapture`. It was re-measured with the directory in place, and the
per-file attribution recorded, before the commit landed — measured, never
assumed.

## Re-derivation harness (tracked, supersedes the script)

The blind reader above was a git-ignored script; its instrument is now
tracked Rust: `bins/rift/tests/bundle_conformance.rs` with `mod blind` in
`bins/rift/tests/bundle_conformance/blind.rs` — an independent reader of the
document's pure steps 1–6 (frame, manifest, digests, segment, window
agreement, ref legality) with steps 7–9 asked of git in a scratch copy, a
fixture module that runs the recipe above through the shipped binary, a
deterministic mutation matrix (40+ cases) and an agreement test that holds
the blind verdict and `rift --json bundle import`'s exit code equal
(0 ⇔ Accept, 1 | 65 ⇔ Refuse; refs unchanged after every refusal). The
self-pin `the_blind_reader_uses_nothing_of_rifts_own` keeps `mod blind` free
of every rift crate. `KNOWN_DISAGREEMENTS` is empty and shrink-only.

`empty` now ACCEPTS through the harness: the `empty.verdict.txt` above
records the pre-fix script and stays as history.

**Re-certification recipe.** When `docs/sync-format.md`'s Transfer bundle,
Stream cursor or Chain segment entry changes:

1. Diff the entries against the text the last Results row certified.
2. Edit `mod blind` from the document alone — never from `bins/rift/src/bundle.rs`
   or `rift-store`.
3. Run `cargo test -p rift --test bundle_conformance`.
4. Every disagreement is a doc defect or a shipped defect and is fixed at its
   root; a `KNOWN_DISAGREEMENTS` row is never added without one.
5. Append a dated Results row below.

| Date | Tree | Cases | Disagreements | Note |
|---|---|---|---|---|
| 2026-08-29 | first tracked run | 44 | 0 | `manifest.reordered` / `manifest.pretty_printed` accepted by both and recorded non-conforming (writer rule); the outstanding attach decision pinned as `…_decision_8_pending` |
| 2026-08-29 | attach decision taken | 44 | 0 | `mod blind` gains the chain-segment attach step (6b, `Rift-Chain-Checkpoint` order); shipped `cmd_import` refuses an incremental bundle into a repository with no chain before any object is written; the pin is now `…_is_refused_by_both_readers`. Independence note: step 6b was added from the doc's chain-segment and chain-ref entries by someone who had already read the shipped importer — so it is NOT blind; the self-pin (no Rift crates) holds, and a future re-certification should re-derive 6b from the doc alone if independence of that step matters |

## 2026-08-29 — the ref-side attach check

The store side of the attach rule was closed by the fixes above; the
ref side was not. `rift bundle import` is refs-only by design and never
opened the projection, so nothing on that path asked whether the segment
attached to the repository's own chain — the blind review's defect (c)
("the incremental bundle imported cleanly into a repo holding no chain")
was still reproducible through the shipped binary.

Closed by ONE predicate rather than a second implementation:
`rift_store::attaches_by(local, segment, id_of)` is the attach rule, generic
over how the local chain is named, and both callers run it —
`import_segment` over the projection's stored links, `cmd_import` over the
chain ref's checkpoint ids (`rift_git::chain::read_order`, oldest first,
empty when the ref is absent). Its three arms: an incremental window (a
non-origin `from_cursor`) cannot bootstrap a repository with no chain;
`from_cursor` must name a position the chain holds (`suffix_after_by`'s two
fail-closed arms); and where the two overlap they must be the same chain. The
check is step 6b of `cmd_import` — after the offline steps 1–6, before the
git-bundle part is written anywhere — so a refusal leaves the object store
untouched. Pinned by `attaches_by_is_the_one_attach_rule` (store),
`an_incremental_bundle_does_not_bootstrap_an_empty_repository` (bin unit:
the git-bundle part is not a bundle, so the refusal's wording proves the
order) and `an_incremental_bundle_is_refused_by_a_repository_with_no_chain`
(e2e, with the origin-then-incremental positive control). `cmd_import` stays
refs-only; `import_segment` stays the pinned reference implementation.
