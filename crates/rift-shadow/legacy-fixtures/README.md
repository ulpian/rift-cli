# Legacy-import golden fixtures

Two shards, one per on-disk layout the pre-Rift capture tool shipped. They are
the oracle for `rift_shadow::legacy_import` and for
`tests/legacy_import_golden.rs`, which renders each shard to a canonical
summary and compares it **byte for byte** with the `expected.txt` beside it.

Nothing here was fetched. Every legacy spelling below is cited to the in-tree
declaration that already carried it, and each fixture's `PROVENANCE` line names
its own:

| fact | where it already lived in this repo |
|---|---|
| artifact names | `crates/rift-shadow/parity/anchors/*/manifest.json` |
| shard path `<id[:2]>/<id[2:]>` | `crates/rift-shadow/src/backend/artifacts.rs` (`shard_path`) |
| branch ref, short-hex ids | `crates/rift-recall-substrate/src/lib.rs` (`Dialect::checkpoint_ref`) |
| per-checkpoint refs, ULID ids | the chain-ref layout notes and the current parser declarations |
| `session_ids`, `commit_oid`, `developer_id` | `crates/rift-recall-types/src/checkpoint.rs` (serde aliases) |
| `session_id` / `turn` / `event` / `occurred_at` | same file, `FullEventRecord` |
| `id` / `name` / `file_path` / `sha256` | same file, `ToolCallRecord` |
| absent `canon_version` ⇒ epoch 0 | `crates/rift-shadow/src/backend/mod.rs` (`NATIVE_CANON_VERSION`) |

**These bytes are frozen.** Change one and `legacy_import_golden` reds — that is
the point of them. Never edit an `expected.txt` to make a test pass; if the
reader's output legitimately changes, the change lands with its reason.

## The damaged fixture

`corrupt_shard/` is **deliberately broken**, and each artifact is broken in a
different way so the reader has to survive all three at once: `metadata.json`
is truncated mid-object, `full.jsonl` carries one truncated record between two
good ones, and `redaction_report.json` is well-formed JSON that is not a valid
report. It is the "corrupt shard" fixture, and what it pins is
that a half-readable legacy shard is **evidence, not a crash**: `parse_shard`
is total, every problem lands in `Shard::issues`, and the shard is REFUSED for
want of a valid redaction report rather than imported. It is kept out of
`fixtures()` on purpose — the importable corpus's frozen report must not move
because a damaged shard was added beside it.

Each `PROVENANCE` file carries the real ref shape with a token-free `{retired}`
marker. The golden loader expands that marker from split source literals before
classification, preserving the frozen bytes seen by the parser without storing
the retired token in this tree.
