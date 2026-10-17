# 13. Storage and schema

Capture writes one local database ([Chapter 3](03-capture.md)), the recall plane
another ([Chapter 6](06-recall.md)), the dashboard a third
([Chapter 12](12-canopy-architecture.md)): what they hold, why they may exist
next to a git-native system of record, and what keeps them honest.

One sentence carries the rest: **these databases are derived, append-only, and
generated from one registry in code.** Derived, so deleting one is a recovery
step, not a loss. Append-only, so nothing in them can be quietly edited.
Generated, so the guarantee cannot be forgotten on a table someone added last
Tuesday.

---

## 13.1 Try it first

Every store is an ordinary SQLite file (copy it, or open with `sqlite3 -readonly`):

```console
$ sqlite3 /tmp/ui.db "delete from canopy_toggle_event;"
Error: stepping, rift: canopy_toggle_event is append-only (DELETE forbidden) (19)
```

The database itself refused, from a trigger created alongside the table (§13.4).

---

## 13.2 Three stores

| File | Keyed by | Holds | Tables | Migrations |
|---|---|---|---|---|
| `~/.cache/rift/<family-id>/projection.db` | the family's capture identity | the capture tables ([Chapter 3](03-capture.md)) | 47 | 18 |
| `~/.cache/rift/<recall-id>/recall.db` | the family's *recall* identity | the delta graph and governance records ([§6.10](06-recall.md)) | 32 | 17 |
| `~/.cache/rift/canopy/ui-state.db` | nothing — machine-global | dashboard state | 8 | 5 |

**The two per-repository stores sit in different directories.** The capture and
recall identities are domain-separated digests over the same repository. Opening one
store under the other's key gives a different, empty file, so each store resolves through a family-scoped
function taking the *family*, not a path: the wrong-key call is not spellable
([Chapter 9](09-worktrees-and-families.md)). The dashboard store sits under a fixed
`canopy/` prefix because a dashboard spans repositories and a profile is a view
*over* them.

**Three file caches sit outside all three**, in the git common directory because
they are the *family's*: every member shares it and a linked worktree must not
re-take its sibling's work ([Chapter 9](09-worktrees-and-families.md)). None is a
record or in any migration; losing one costs a conversion, a delta frame or a first
contact, never an answer. `rift clean` collects each on its TTL.

- `rift-family/doc/` — one converted document per version.
- `rift-family/basis/` — the redacted read-time bytes of every revision a window was
  handed, for deltas against an uncommitted revision
  ([§7.4](07-serving-and-injection.md#74-tier-three-the-decision-at-the-read)).
- `rift-family/outline/` — `<sha256>-e<E>p<P>r<R>.outline`, a copy of a bound
  outline so the read gate learns with no store open that it is bound
  ([§7.4](07-serving-and-injection.md#a-first-read-of-a-large-file)). Its grammar is
  closed, under 64 KiB, and a moved file names another key, so a stale entry cannot
  be read. TTL: `basis_ttl_days`.
- `rift-family/linehash/` — `<sha256>.lh`, one file per revision: a magic, the line
  and byte counts and a 64-bit hash per line, so the output-matching step
  ([§7.4](07-serving-and-injection.md#what-a-shell-result-showed-the-window)) compares a
  shell result with no store open. It rebuilds from any copy of the bytes, and an
  idle sweep collects entries older than seven days and any past 8,192 files.

**New relations are tables, not widened columns**, because a migration may not drop
or rename: `tool_result_tokens` is not a column of `file_read_tokens` (keyed
through `tool_reads_file`, so only calls that *resolved to a file*; a search
resolves to none, a different question — [§8.6](08-cost-and-savings.md#86-what-avoided-actually-measures));
`session_worktree_cd` holds a basis (`shell_cd`) the `session_worktree` CHECK
cannot admit, and widening a CHECK means rebuilding the table, so readers read the
union of the two ([Chapter 9](09-worktrees-and-families.md)).

**The cache root** resolves from `RIFT_CACHE_DIR`, `XDG_CACHE_HOME`, `$HOME/.cache`
(unix) or the local app-data directory (Windows), then the working directory; path
functions take an environment lookup, so a hermetic test gets isolated stores
([Chapter 2](02-configuration.md)). **Stores are owner-only**: `0700` on the
directory, `0600` on the file, inherited by the WAL and shared-memory sidecars — the
filesystem leg of the defence against a local reader, which unencrypted columns rely
on with the out-of-tree location.

---

## 13.3 Why a database at all

Git is the record: a checkpoint's bytes and the chain live in git, and `rift
verify` re-derives its answer from git ([Chapter 5](05-ledger-and-verification.md)).
The database exists because "which files did this session read, and how many reads
re-fetched bytes we already had" is a question about *joins*, and walking git trees
per query would blow the hook budget. It is a **projection**, which means: deleting
it is not data loss (the documented recovery step); migrations never need to be
destructive, since a rebuild exists and the engine can refuse destructive
statements (§13.6); and re-ingest is a no-op, since row ids are hashes of content
and writes are `INSERT … ON CONFLICT DO NOTHING`.

The exception: **governance records are not derivable from git.** What was served,
what an evaluation concluded, which key generation was in force are facts about
*this machine*. They share the file with derived rows and are why it is not pure
cache (§13.12).

---

## 13.4 The registry is the schema

There is no SQL schema file. There is a registry: table specifications in Rust
from which the DDL and the guards are generated. A column is one of six shapes
(non-null or nullable text, text constrained to a closed value set, non-null or
nullable integer, blob); a table names its columns, primary key and uniqueness
constraints. One function renders `CREATE TABLE IF NOT EXISTS … STRICT`, another
the append-only trigger pair:

```sql
CREATE TRIGGER canopy_profile_startup_event_no_update
BEFORE UPDATE ON canopy_profile_startup_event
BEGIN SELECT RAISE(ABORT,
  'rift: canopy_profile_startup_event is append-only (UPDATE forbidden)');
END
```

Nobody typed that, so the guarantee holds for *every* table: 47 capture tables and
94 triggers, 32 recall and 64, 8 dashboard and 16. Four generator decisions matter.

**`STRICT`** makes column types engine-enforced, and keeps the DDL valid for other
SQLite-compatible engines (§13.11). **Closed value sets are real `CHECK
(kind IN (…))` constraints**: the delta model declares mechanical node and edge
kinds and not fuzzy ones ("similar to"), so the database *refuses* them
([Chapter 6](06-recall.md)). The idempotent insert skips only a key conflict: a
word outside a closed set, a missing required value or a wrong type is a typed
`ConstraintRefused` naming table and column, so a refused row never passes for one
already recorded. That is why it is `ON CONFLICT DO NOTHING`, not `INSERT OR
IGNORE`, which SQLite also applies to CHECK and NOT NULL failures. **No floats
reach the store**: confidence is an integer in thousandths, money integer
micro-units, because comparing money as floating point is a bug class the schema
declines. **Foreign keys are documented, not emitted**: a single writer guarantees referential
order, and real constraints would need global insert-ordering across the ingest.

---

## 13.5 Three layers of append-only, and the tests that prove them

**Type layer.** The store trait exposes `insert` and `insert_or_ignore` and no
`update` or `delete` — none, not private ones; the integration test on the public
surface *cannot call* a mutating method. Every insert also validates its
identifiers against the registries before SQL is built and binds values as `?`, so
the identifier surface is no injection vector; a source-scanning test
(`no_value_is_interpolated_into_sql`) walks the crate for a quoted interpolation
in a value context. The migration module is the one exemption, interpolating a
validated registry constant (a table *name*), never runtime data.

**SQL layer.** The two generated triggers (§13.1): raw SQL still cannot mutate a
row, and the store classifies the aborts as their own error kind.

**Test layer.**

| Test | What it holds |
|---|---|
| `every_canonical_table_rejects_update_and_delete` (and recall and dashboard twins) | a valid sample row per registered table; `UPDATE` and `DELETE` both refused and the row survives |
| `no_canonical_table_can_be_mutated` | the same as randomized (table, mutation) cases |
| `every_db_table_is_registered_or_infra` | after a full production migration every real table is registered and carries **both** guards |
| `forgotten_registration_is_caught` | a canary: migrate a canonical-shaped table unregistered and assert the keystone flags it |

The fourth exists because the third would pass vacuously if it could never fail.
The infra tables (migration ledger, per-table version) are exempt by underscore
prefix. One committed DDL snapshot per store makes a schema change a reviewable
diff. 

---

## 13.6 Migrations: additive, ordered, checksum-locked

A migration is a version, a name and SQL. The engine applies them in order, records
each one's SHA-256, and re-computes the digests on every open; a mismatch refuses to
run ("migration 4 checksum changed since it was applied … the projection is a
rebuildable artifact: set it aside and run `rift rebuild`, then `rift verify`.
Anything else here is tampering"). That is a lock, not a warning: **a schema change
is a new migration, never an edit to an applied one.** An additive guard also
inspects the SQL: it strips comments and string literals (so `DROP/**/TABLE` does
not sneak through), then refuses `DROP` of a table, column, index, trigger or view,
`ALTER … DROP`, any `RENAME`, and a statement-initial `UPDATE` or `DELETE` whose
target is in any of the three registries. It is token-based because the append-only
triggers legitimately contain `BEFORE UPDATE`, and a body is never
statement-initial. Versions must be gap-free and strictly increasing, so two
branches claiming the next number collide at open.

**The frozen prefix.** Migration SQL is *generated from the registry* and its
digest locked once applied, so a table inserted mid-registry would change an
applied migration's SQL and every deployed projection would refuse to open. Each
migration therefore owns a bounded, frozen slice (the capture core renders exactly
the first 30 entries), new tables append past the last boundary in a *new*
migration, and a test pins the frozen slice's bytes so drift fails at once.

> **Tip:** Never reorder the registry; append at the end.

**Why new tables and not new columns.** A fact that needs a home but whose obvious
column is in the frozen prefix gets a relation, and a relation is right anyway when
the fact is *sparse*: a nullable column spells "named no commit" and "never looked"
the same way, a missing row says one thing. The imported-checkpoint claim cannot be
a checkpoint row: that id is a SHA-256 address, a foreign id does not parse as one,
and a locally minted id would put a row in the projection git cannot produce, which
verify would read as tampering. The price catalogue is seeded in a migration and
extended by later append-only ones, since a price change is an append
([Chapter 8](08-cost-and-savings.md)). One migration only adds an index: a backfill
on every open scanned the audit relation (~60 ms on a 52 MB store) inside the one
hook that cannot detach, pushing a commit past the 100 ms budget.

**The stale-binary hazard.** A projection's guards live in its file but the *SQL*
lives in a binary. Edit a migration with an old binary still on disk and a git hook
can re-apply the previous bytes, so the next open refuses and says "tampering"
about your own afternoon. **Rebuild the binaries before touching a store**, and
read a development-time checksum refusal as "binary and source disagree" first.

---

## 13.7 Bitemporal facts

Facts with two time axes — true in the world, and learned by this system — carry
`valid_from`/`valid_to` and `recorded_at`, and a correction is a new row, so "what
does this cost" and "what did we believe in March" are both answerable forever
([Chapter 8](08-cost-and-savings.md) owns the price rules). Key custody uses the
same shape: a rotation inserts a generation whose `supersedes` points at the
previous one.

---

## 13.8 Encryption at rest and key custody

Two layers are conceivable: the whole file, or the highest-sensitivity columns. The
first is an engine feature unavailable on the intended production engine (§13.11),
so the second is the standing guarantee.

**The envelope.** A per-repository key-encryption key lives in the OS keychain and
never touches disk. A per-repository data key, generated once at random, is stored
*sealed* under it in an insert-only custody table beside a reference to the
keychain entry. Rotating re-wraps the *same* data key under a fresh key as a new
generation, so no data is re-encrypted and old rows stay readable. The keychain
sits behind a seam with an in-memory implementation for tests and headless hosts.

**What gets encrypted.** A second registry, mirroring the schema registry, names the
`(table, column)` slots AEAD-encrypted before durable storage. It has one entry: the
validity plane's digest bodies. Tests require each registered column to exist in a
schema registry and be `BLOB`, because ciphertext has to fit. No production path
holds a recall data key, so outline digests go through the plain frame, an encrypted
row fails closed to unavailable, and the outline writer takes only redacted bytes
and refuses any path the serve policy denies or marks sensitive.

**Anti-relocation.** Each ciphertext is a nonce plus sealed bytes with table, column
and row id bound in as associated data, so copying it into another row fails
authentication. **The sealed data key is deliberately not registered**: encrypting
it under the key it wraps would be circular.
[Chapter 4](04-redaction-and-secrets.md) covers what never reaches durable storage.

---

## 13.9 Two hashes over the same bytes

SHA-256 where byte-compatibility with git matters (the chain, a checkpoint's
content hash and id, the content hash recorded for a read, the derived ids of the
recall graph's rows, the digest locking a migration); blake3 for internal handles
(salted developer and machine identifiers, the policy fingerprint of an effective
configuration, integration-ownership fingerprints, the frozen ids inside serve
envelopes). They are **distinct types with no conversion**, enforced by a doc-test
asserting the conversion does not compile ([Chapter 5](05-ledger-and-verification.md)
§5.10). Git's own blob id is a third digest over a different framing of the same
bytes, hence an explicit bridge table ([Chapter 6](06-recall.md)). The worktree
hasher computes a **plain SHA-256** of file bytes, not a chain leaf and not blake3,
because it compares against the content hash capture recorded at read time.

---

## 13.10 Frozen names and read-compatible history

The recall registry holds tables named for a retired serving transport, and a cache directory segment has the same flavour. Renaming them would be
wrong three times over: the registry entry *is* the table's name in every deployed
file; migration bodies are checksum-locked, so editing a byte refuses every
existing store; and the tables are append-only, so historical rows cannot be
rewritten under a new name. They survive as read-compatible history: the live
serving axis is the channel-discriminated pair `serve_call_event` and
`serve_session_event` ([Chapter 7](07-serving-and-injection.md)); nothing writes the
superseded pair and no reader targets it, and the cache segment that held
connection leases is written by nothing but still probed for leases an old install
wrote. **Do not "tidy" a shipped name** — a banner in the schema source says so.

---

## 13.11 The engine seam

Everything above is implemented against a trait, not SQLite. The shipping engine
is a synchronous `rusqlite` reference implementation running the *same* generated
DDL and triggers any SQLite-compatible engine would, so the immutability and
migration guarantees are provable offline in a normal `cargo test`. The seam is
**async** because the intended production engine (Turso) is async-native and the
dashboard and serving readers run on an async runtime; a synchronous seam would
force a blocking call inside one, a deadlock hazard. The reference engine holds one
connection behind a mutex and releases it before returning, so no guard crosses a
suspension point. The reference engine ships because Turso's pinned pre-release lacks trigger
creation, strict typing and encryption and pulls in advisory-bearing
dependencies; nothing about that reaches callers, which is the argument for a seam.

**A busy timeout (five seconds), because hooks write concurrently:** without one a
colliding writer fails instantly, which the never-fail hook contract turns into a
dropped row.

**Lifetime exclusion.** A live store holds a shared guard on its path's concurrency
marker; replacement takes it exclusively and refuses while a live guard remains.
Hardlink aliases are refused, and read-only diagnostics (`rift doctor`, `rift
status`) never create the source or marker.

**Change notification is a capability.** The reference engine is polled (a
monotone-under-append watermark plus a rebuild-epoch row); an engine with native
notification overrides it without touching callers ([Chapter 12](12-canopy-architecture.md)).

A registry fast-path creates every table and guard bypassing the migration ledger.
It is **test and tooling only**, and a CI check greps the binaries to keep it so.

---

## 13.12 Rebuilding, and what a rebuild cannot restore

**The recall projection is set aside and re-derived**: `rift-memd rebuild` keeps
the file and sidecars as `recall.db.set-aside-<YYYYMMDDTHHMMSSZ>`, re-opens through
the migrations and re-ingests from git. **The capture projection is re-ingested in
place**: `rift rebuild` walks the checkpoint branch through the ordinary
append-only path, and since rows have content-hash ids and `ON CONFLICT DO NOTHING`,
a healthy projection gains nothing — a repair, not a reset. Set-asides are never
deleted by a rebuild (the projection holds governance records git does not) or by
`rift clean`; `rift doctor` lists them.

A condense writes the shard and its chain entry but not the shard's own rows, so
the detached worker levels the projection after any drain that wrote a checkpoint.
The walk skips a shard at or before the recorded watermark whose `checkpoint` row
exists (the last row a shard writes, so a walk stopped mid-shard redoes it);
position alone would not do, since walk order is the chain's order followed by every
shard the chain does not name. `rift rebuild --json` reports `shards_skipped` beside
`shards_projected`.

**What a shard restores** ([Chapter 5](05-ledger-and-verification.md)): sessions
under their own agent, worktree observations (without head or path, which no shard
holds), the savings denominator's reads and tool results, and usage with a booking
priced from the catalog as of the rebuild's supplied clock. Every write is
insert-or-ignore under the live keys, so a live row wins. **What it cannot
derive:** per-commit diff facts (they come from git history); sessions condensed
before the usage artifact (their rows live only in a projection that holds them;
`rift backfill --from` and `--recall-from` import them from a set-aside file through
a READ_ONLY open); outline bindings until the next counted read or condense binds them again; and a window's
inherited runs, family-held declines and matched lines, which are counted
(`window_rows_inherited`, `window_rows_family_held`, `window_rows_matched`) but written to no
`n_window_row`, whose kinds are sealed by its migration ([§6.12](06-recall.md#612-what-a-context-window-already-holds)).

**Governance records are not rebuildable.** Serve audit rows, outcome labels,
evaluation runs and key metadata record what this machine did, not what git
contains, so deleting a recall projection discards them — the honest trade the split
makes. The serves a shard carries return through the one live appender with their
live instant, never the rebuild's clock; a serve no session made, and the typed
words of an exact retrieval's reason (`withheld`), are not restored.

> **Tip:** when a projection looks wrong, rebuild the binaries, rebuild the
> projection, run `rift verify`.

---

## 13.13 The dashboard store

The third store holds no captured data, only what the dashboard remembers. It is
machine-global and modelled as **events rather than rows you edit** — superseding
events reduced by a "latest valid per key" fold.

Eight `canopy_*_event` tables — project catalogue, toggles, layouts (frozen for
writes), profiles, profile membership, per-profile layouts, theme slots and the
startup pin — are each reduced by that fold.

Every reducer mutation is insert-only; removing the active or last profile is
refused in both reducer and store. [Chapter 12](12-canopy-architecture.md)
§12.10 covers the folds. The catalogue holds **one row per repository family**, so
registering a second checkout of a repository that already has a row is turned away
with the path that holds it — a rule rendered as a finished sentence, because the
dashboard and CLI print the same words.

---

## 13.14 Adding a table

Append the entry to the registry at the end, past every frozen boundary; add a
**new** migration rendering exactly that slice; register a highly sensitive column
as a `BLOB`. Prefer a missing row to a nullable column for a sparse fact, and decide
whether a rebuild restores it (§13.12). The immutability tests, DDL snapshots,
additive guard and keystone then exercise it; you write no triggers, no `CREATE
TABLE` and no append-only test.

---

## Related chapters

[05-ledger-and-verification.md](05-ledger-and-verification.md), the git-side
record this projection derives from · [06-recall.md](06-recall.md) ·
[08-cost-and-savings.md](08-cost-and-savings.md) for the price catalogue on
§13.7 · [12-canopy-architecture.md](12-canopy-architecture.md) ·
[14-sync-readiness.md](14-sync-readiness.md) ·
[16-development.md](16-development.md) · [17-invariants.md](17-invariants.md).
