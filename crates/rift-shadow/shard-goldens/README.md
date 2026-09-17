# Shard goldens — the total artifact oracle

One deterministic checkpoint carrying **every artifact the pipeline can write**,
with each artifact's bytes frozen in this tree. The live side is built by
`rift_shadow::shard_fixture::every_artifact_archive` through the real compactor
and the real redaction gate; the comparison is `tests/shard_goldens.rs`.

## Why it exists

The parity anchors under `../parity/anchors/` are a **partial** oracle: they
compare four artifact names. Everything added since —
`content.redacted`, the transcript pair, the four `digest.*` documents, the
`tasks/<id>/` subtrees, the `sessions/<index>/` metadata — sat outside that
comparison, so an artifact added, changed or **planted** under an unlisted name
moved no compared byte.

And the two byte-stability claims the crate did make were vacuous:
`compaction_is_byte_stable` compared `compact_jsonl(x)` with `compact_jsonl(x)`
in one process, `digests_are_byte_stable` compared `failures_json(x)` with
`failures_json(x)`. `f(x) == f(x)` holds for every implementation, so neither
test could ever fail. Byte stability needs bytes frozen by an **earlier build**,
which is what `artifacts/` holds.

## Layout

| path | what it is |
|---|---|
| `every_artifact/raw.jsonl` | INPUT — the session transcript the compactor reduces |
| `every_artifact/task_raw.jsonl` | INPUT — the dispatched subagent's own transcript |
| `every_artifact/capture.txt` | INPUT — the session capture document (`rift-capture/1` grammar) |
| `MANIFEST` (beside the inputs) | the emitted artifact set, one shard-relative path per line |
| `every_artifact/artifacts/**` | OUTPUT — the frozen bytes, one file per emitted artifact |

## The inputs are chosen, not arbitrary

`raw.jsonl` carries a user prompt (the intent digest's locator), a streamed
assistant message in two fragments sharing one id (the merge-group the compact
boundary rounds toward), a thinking block (dropped, so a signature never
replays), a tool result longer than `MAX_INLINE_CHARS` (elided to a handle), a
failing tool result (the failures digest), a `Task` dispatch (the subagent
case), a line that is not JSON (the `unparsed` counter) and a record shape the
compactor does not model (the `unrecognised` counter).

## These bytes are frozen

Change one and `shard_goldens` reds — that is the point of them.

**There is no bless switch.** `place_missing_goldens` is `#[ignore]`d — run
explicitly with `-- --ignored` — writes only files that are **absent**, and
refuses to overwrite one that exists, so it can create a new fixture and can
never make a failing comparison pass. It is ignored deliberately: placing
files is an act a person asks for, never something an ordinary test run does on
its own. If an
artifact's bytes legitimately change, delete that golden by hand, re-run the
one-shot, read what it wrote, and land it in its own commit with its reason —
the discipline the import-fixture README beside this one states, for the same
reason.
