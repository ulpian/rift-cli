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

Every store is an ordinary SQLite file, so you can look. Copy first: reading a
live projection while hooks write is fine, but a copy lets you poke at it.

```console
$ cp ~/.cache/rift/canopy/ui-state.db /tmp/ui.db
$ sqlite3 /tmp/ui.db "select version, name from _migrations;"
1|canopy_core … 5|canopy_profile_startup

$ sqlite3 /tmp/ui.db "update canopy_project_event set label = 'x';"
Error: stepping, rift: canopy_project_event is append-only (UPDATE forbidden) (19)

$ sqlite3 /tmp/ui.db "delete from canopy_toggle_event;"
Error: stepping, rift: canopy_toggle_event is append-only (DELETE forbidden) (19)
```

That is the dashboard store's whole schema history — five migrations, each
recorded with the digest of the SQL applied — then a refusal. Note *what*
refused: not Rift, since you did not go through it, but the database itself,
from a trigger created alongside the table (§13.4).

> **Tip:** `sqlite3 -readonly <file>` opens a projection with no chance of
> touching it, which is the right way to inspect a live one. Rift has a
> read-only open for the same reason (§13.11).

---

## 13.2 Three stores

| File | Keyed by | Holds | Tables | Migrations |
|---|---|---|---|---|
| `~/.cache/rift/<family-id>/projection.db` | the family's capture identity | sessions, turns, model and tool calls, file revisions, attribution, token usage and cost, hook audit, checkpoint integrity, worktree provenance, measured tool results | 47 | 17 |
| `~/.cache/rift/<recall-id>/recall.db` | the family's *recall* identity | the delta model's derived graph plus governance records: serve audit, serve calls and sessions, outcome labels, metric snapshots, evaluation runs, key metadata | 30 | 15 |
| `~/.cache/rift/canopy/ui-state.db` | nothing — machine-global | dashboard state spanning repositories: project catalogue, feature toggles, per-project layouts, profiles, membership, theme slots, startup pin | 8 | 5 |

**The two per-repository stores sit in different directories.** The capture and
recall identities are deliberately domain-separated digests over the same
repository, so `~/.cache/rift/4d9d222b…/` holds `projection.db` while
`~/.cache/rift/94b3809e…/` holds `recall.db`. That is not decoration: opening
one store under the other's key gives a different, empty file — a defect that
has happened, where the dashboard read a recall store under the capture id and
truthfully reported nothing. Each store now resolves through a family-scoped
function taking the *family*, not a path, so the wrong-key call is not spellable
([Chapter 9](09-worktrees-and-families.md)). Getting it wrong here is silent, so
this is where the types stop you.

**The dashboard store is repo-independent**, under a fixed `canopy/` prefix
rather than a repository directory, because a dashboard spans repositories and a
profile is a view *over* them.

**Two content caches sit outside all three**, in the git common directory
rather than the cache root, because they are the *family's* and not the
machine's: `<common_dir>/rift-family/doc/` holds one converted document per
version, and `<common_dir>/rift-family/basis/` holds the redacted read-time
bytes of every revision a window was handed, so a delta against an uncommitted
revision has something to diff against
([§7.4](07-serving-and-injection.md#74-tier-three-the-decision-at-the-read)).
Neither is a record and neither is in any migration: a file and a small meta
document apiece, content-addressed, collected by `rift clean` on their own
TTLs, and losing the whole of either costs a conversion or a delta frame and
never an answer. They are in the git common dir rather than under the cache
root for the reason [chapter 9](09-worktrees-and-families.md) gives: every
member of a family already shares that directory, and a linked worktree must
not re-take work its sibling has done.

**The v15 `tool_result_tokens` relation is a new table rather than a widened
old one**, for a reason worth stating because it looks like a column at first
glance. `file_read_tokens` is keyed by `tool_call_id` and reached through
`tool_reads_file`, so only a call that *resolved to a file* can have a row
there. A search returns repository content and resolves to no file; measuring
one under that key is not a widening, it is a different question. The row
names the channel it arrived on and the basis of its count, both
CHECK-constrained to closed vocabularies, so an unrecognised spelling cannot
reach storage and be summed as though it meant something
([§8.6](08-cost-and-savings.md#86-what-avoided-actually-measures)).

**The v17 `session_worktree_cd` relation is the same kind of answer.**
It holds session worktree observations under a basis the v14 `session_worktree` CHECK cannot admit — `shell_cd`, a read filed under the member its leading `cd` named.
Widening that CHECK would mean rebuilding the table, and a migration may not drop or rename one, so the wider vocabulary got its own table with its own guards; the writer sends each observation to exactly one of the two, and every reader reads their union ([Chapter 9](09-worktrees-and-families.md)).
Their `work_tree` column is a machine-local address and never enters a shard, but a checkpoint freezes the rest of each agent-hook observation in `session_worktrees.jsonl`, so a capture rebuild restores those states without their head (see [§13.12](#1312-rebuilding-and-what-a-rebuild-cannot-restore)); an `attach`, `import` or `commit` observation is written after its archive and is restored by nothing.

**The recall v14 `memd_audit_pull_claim` relation is a sparse companion of `memd_audit_event`**, keyed on the audit row's id like `memd_audit_citation` and `memd_audit_session`: for a pull whose caller named held revisions, how many it named, how many of them this family had recorded reading, how many it had not, and the rule the credit was booked under. A pull that named nothing writes no row, so a row's presence means a claim was made. The row is written just after its audit row; a stop between the two loses only the side row, and redelivering the serve writes it once under the same key.

**The cache root is resolvable, so tests never touch your home directory**,
chosen in order from `RIFT_CACHE_DIR`, `XDG_CACHE_HOME`, `$HOME/.cache` on unix
or the local app-data directory on Windows, and finally the working directory.
Every path function takes an environment lookup, so a hermetic test injects a
temporary directory and gets a complete isolated set of stores
([Chapter 2](02-configuration.md) has the state-roots table).

**They are owner-only on disk.** Opening a projection sets `0700` on the
directory and `0600` on the file, and SQLite's write-ahead-log and shared-memory
sidecars inherit it. It is the filesystem leg of the defence against a
local reader; unencrypted columns rely on it plus the out-of-tree location. On
Windows the local app-data directory is already scoped to the user profile, and
a tighter access list is a later hardening item.

---

## 13.3 Why a database at all

Git is the system of record: a checkpoint's bytes and the hash chain live in
git, and `rift verify` re-derives its answer from git rather than any database
([Chapter 5](05-ledger-and-verification.md)). The database exists because "which
files did this session read, and how many of those reads re-fetched bytes we
already had" is a question about *joins*, and walking git trees per query would
cost far more than the hook budget allows. It is a **projection**: a
query-shaped view of facts that already exist elsewhere, which carries three
consequences.

1. **Deleting it is not data loss.** It is the documented recovery step when a
   schema advances awkwardly, a key is lost, or you simply do not trust it. Git
   still holds everything canonical.
2. **Migrations never need to be destructive.** A rebuild exists, so there is no
   reason to rewrite rows in place and the migration engine can refuse
   destructive statements outright (§13.6).
3. **Re-ingest is a no-op.** Rows are content-addressed and written with
   `INSERT … ON CONFLICT DO NOTHING`, so ingesting the same checkpoint twice
   converges on the same set instead of duplicating it.

The one exception matters: **governance records are not derivable from git.**
What was served to an agent, what an evaluation concluded, which key generation
was in force are facts about *this machine's* behaviour. They share the file
with derived rows but are why it cannot be treated as pure cache (§13.12).

---

## 13.4 The registry is the schema

There is no SQL schema file in the repository. There is a registry: table
specifications in Rust from which the DDL and the guards are generated. A column
is one of six shapes — non-null text, nullable text, text constrained to a
closed value set, non-null integer, nullable integer, blob — and a table names
its columns, primary key and any table-level uniqueness constraints. From that,
one function renders `CREATE TABLE IF NOT EXISTS … STRICT` and another the pair
of append-only triggers:

```sql
CREATE TRIGGER canopy_profile_startup_event_no_update
BEFORE UPDATE ON canopy_profile_startup_event
BEGIN SELECT RAISE(ABORT,
  'rift: canopy_profile_startup_event is append-only (UPDATE forbidden)');
END
```

Nobody typed that, which is why the guarantee holds for *every* table rather
than the ones somebody remembered. The arithmetic is exact: 45 capture tables
and 90 triggers, 30 recall tables and 60, 8 dashboard tables and 16. Four
decisions inside the generator are worth knowing.

**`STRICT` mode** makes column types engine-enforced rather than advisory, so a
text value cannot land in an integer column, and the same DDL stays valid for
other SQLite-compatible engines — making §13.11's seam more than a hope.

**Closed value sets are real constraints**, rendering `CHECK (kind IN (…))`. The
delta model's node and edge kinds use this: mechanical kinds are declared, while
semantic kinds — "similar to", "relates to" — are not, so the database *refuses*
them. Semantic recall is not off by a flag; it is unrepresentable until a
migration widens the set ([Chapter 6](06-recall.md)).
The idempotent insert skips only a key conflict, never a constraint: a word outside a closed set, a missing required value or a value of the wrong type is a typed `ConstraintRefused` error naming the table and the column, so a refused row can never pass for one already on record.
That is why it is `ON CONFLICT DO NOTHING` and not `INSERT OR IGNORE`, which SQLite applies to CHECK and NOT NULL failures as well as to key conflicts.

**No floats reach the store.** Confidence is an integer in thousandths, bounded
zero to a thousand; money is integer micro-units. Comparing money as floating
point is a class of bug the schema declines to have.

**Foreign keys are documented, not emitted.** Relations are recorded in comments
beside the columns rather than as SQL `REFERENCES`, because the projection is
rebuilt by a single writer guaranteeing referential order and real constraints
would need global insert-ordering across the ingest — deferred deliberately
rather than half-done. Range and enum checks beyond the closed sets live at the
Rust writer layer.

---

## 13.5 Three layers of append-only, and the tests that prove them

**The type layer.** The store trait exposes `insert` and `insert_or_ignore` and
no `update` or `delete` — not private ones, none. That the integration test on
the public surface *cannot call* a mutating method is the compile-time half of
the guarantee.

It does a second job that is easy to miss: every insert validates its
identifiers against the registries before any SQL is built, so an unknown table
or an off-spec column is refused rather than concatenated in, and values always
bind as `?`. The identifier surface therefore cannot be an
injection vector even for careless future code, and a source-scanning test
(`no_value_is_interpolated_into_sql`) walks the crate for a quoted interpolation
in a value context. The migration module is the one exemption: it interpolates a
validated registry constant — a table *name* — never runtime data.

**The SQL layer.** The two generated triggers, as in §13.1: raw SQL bypassing
Rust still cannot mutate a row, and the store classifies those aborts into their
own error kind, distinguishing "guard fired" from "write failed".

**The test layer.** Four tests close the loop:

| Test | What it holds |
|---|---|
| `every_canonical_table_rejects_update_and_delete` (and its recall and dashboard twins) | insert a valid sample row into each registered table; assert `UPDATE` and `DELETE` are both refused and the row survives |
| `no_canonical_table_can_be_mutated` | the same property as randomized cases over (table, mutation kind) |
| `every_db_table_is_registered_or_infra` | after a full production migration, every real table is registered and every registered table carries **both** guards |
| `forgotten_registration_is_caught` | a canary: migrate a canonical-shaped table without registering it, and assert the keystone flags it |

That last pair makes the slogan true: the third would pass vacuously if it could
never fail, so the fourth commits the mistake and asserts it is caught. The two
infra tables — the migration ledger and the per-table schema version — are
underscore-prefixed and exempt by that prefix. Three further tests pin the DDL
itself, one committed snapshot per store holding every table, trigger and index,
so a schema change is a reviewable diff and an unintended one fails.
`cargo test -p rift-store` runs all of it.

---

## 13.6 Migrations: additive, ordered, checksum-locked

A migration is a version number, a name and a string of SQL. The engine applies
them in order, records each one's SHA-256 in the migration ledger, and on every
later open re-computes those digests. If an applied migration no longer hashes
to what was recorded, it refuses to run:

> migration 4 checksum changed since it was applied. If a release note names an
> authorized schema change, the projection is a rebuildable artifact: set it
> aside and run `rift rebuild`, then `rift verify`. Anything else here is
> tampering: stop and inspect.

That is a lock, not a warning. **A schema change is a new migration, never an
edit to an applied one.**

An additive guard also inspects the SQL first. It strips comments and quoted
string literals — so a comment mentioning `DROP TABLE` is harmless and a
`DROP/**/TABLE` split does not sneak through — then walks the tokens, refusing
`DROP TABLE`, `DROP COLUMN`, `DROP INDEX`, `DROP TRIGGER`, `DROP VIEW`;
`ALTER … DROP` in either spelling SQLite accepts; any `RENAME`; and a
statement-initial `UPDATE` or `DELETE` whose target resolves in *any* of the
three registries. That last clause is why the check is token-based: the
append-only triggers legitimately contain `BEFORE UPDATE` and `BEFORE DELETE` in
their bodies, and a body is never statement-initial. Versions must also be
gap-free and strictly increasing, so two branches claiming the next number
collide loudly at open time.

### The frozen prefix

The migration SQL is *generated from the registry* and its digest is locked once
applied, so a table appended to the middle of the registry would change the
generated SQL of an already-applied migration and every deployed projection
would refuse to open.

The answer is frozen boundaries: the capture store's core migration renders
exactly the first 30 registry entries, the identity migration that follows the
next two, and so on, each migration owning a bounded slice. New tables append
past the last boundary in a *new* migration, and a test pins the generated bytes
of the frozen slice, so moving or growing it fails immediately rather than on
somebody's machine three weeks later.

> **Tip:** Do not reorder the registry to group related tables. The order is
> load-bearing. Append at the end.

### Why new tables and not new columns

Four capture migrations exist because a fact needed a home and the obvious
column was unavailable. Take the **checkpoint-to-commit link**: every shard
already names the commit it is about and the projection was dropping it, but
widening the checkpoint table was impossible because it sits in the frozen
prefix — and even without the freeze a separate relation is right, because the
fact is *sparse*, and a nullable column spells "named no commit" and "we never
looked" with the same `NULL` while a missing row says one thing.

Three more followed. The **verification witness** needed a composite primary key
anyway, since one sweep runs a single verify witnessing many checkpoints. The **imported-checkpoint claim** cannot be a checkpoint row at
all: that table's id is a SHA-256 content address, a foreign id does not parse
as one, and minting a local id would put a row in the projection git cannot
produce, which the next verify would correctly read as tampering. **Worktree
provenance** became two tables — an append-only observation relation,
content-addressed over every observed field so a redelivered hook is an ignored
duplicate, and a rebuildable projection of a shard's metadata where a shard
naming no worktree projects no row.

Two migrations create no table. The price catalogue is seeded inside the
turn-boundary schema migration and extended by a later append-only one, because
prices are data and a price change is an append
([Chapter 8](08-cost-and-savings.md)). The other adds only an index, and shows
what the hook budget does to schema design: a backfill on every open scanned the
audit relation with `SELECT DISTINCT`, around 60 ms on a 52 MB store — inside
the one hook that cannot detach its work, pushing a real commit past the 100 ms
budget. The fix was a partial index plus a guard so the backfill runs only while
some session lacks its identity row.

### The stale-binary hazard

A projection's guards live in its file, but the *SQL* lives in a binary. Edit a
migration during development with an old binary still on disk and a git hook can
fire that binary, re-apply the previous bytes, and leave the recorded digest
disagreeing with your source — so the next open refuses, saying "tampering"
about your own afternoon. **Rebuild the binaries before touching a store**, and
read a development-time checksum refusal as "binary and source disagree" first.

---

## 13.7 Bitemporal facts

Some facts have two time axes: when the fact was true in the world, and when
this system learned it. Prices are the clearest case — a rate takes effect on a
date, Rift transcribes it on another, a correction arrives on a third — so a
price row carries `valid_from` and a nullable `valid_to`, the world's axis, plus
`recorded_at`, the belief axis. Resolving a price takes two timestamps: what was
in force *at* the call, according to what we believed *as of* some moment. The
query takes the latest row whose validity window contains the call, restricted
to rows recorded by the as-of moment, with a deterministic tie-break so the
answer never depends on row order.

Two rules keep it honest. **A partial catalogue produces no number**:
resolution asks for all four token classes, and if one has no price on file it
returns nothing rather than a partially-priced fact
([Chapter 17](17-invariants.md)). **A correction is a new row**, so "what does
this cost" and "what did we believe it cost in March" are both answerable from
the same table forever.

Current report amounts quote each captured call's token usage against the catalog at read time, with the call start as valid time and an explicit catalog belief cutoff.
An upgrade and a cold usage-shard rebuild therefore report the same current amount without summing old and corrected bookings.
The quote writes no rows; a call with any unpriced token class contributes no amount.
A mixed session sums its completely priced calls, while a session with none has no dollar figure.
Historical booked-cost reads select an unsuperseded fact visible at their belief cutoff; a future correction does not hide the old belief.
A same-instant correction wins over its predecessor through its link, while unrelated leaves retain a deterministic recorded-time and identity order.
A correction's identity includes its predecessor so reverting an amount at the same instant appends a distinct linked row.

Cost facts carry the identical shape, and key custody uses the idea one level
down: a rotation inserts a generation whose `supersedes` points at the previous
one, so data encrypted under an older generation stays decryptable and custody
history stays auditable.

---

## 13.8 Encryption at rest and key custody

Two layers of encryption are conceivable for a local store: the whole file, or
the highest-sensitivity columns inside it. The first is an engine feature,
unavailable on the intended production engine (§13.11), so the second is the
standing guarantee.

**The envelope.** A per-repository key-encryption key lives in the operating
system keychain and never touches disk. A per-repository data key is generated
once at random, sealed under it, and stored *sealed* in an insert-only custody
table beside a reference to the keychain entry — never the key itself. Opening
custody unwraps the newest generation; rotating re-wraps the *same* data key
under a fresh key-encryption key as a new generation, so no data is re-encrypted
and old rows stay readable. The keychain sits behind a seam with an in-memory
implementation for tests and headless environments.

**What gets encrypted.** A second registry, mirroring the schema registry, names
the `(table, column)` slots AEAD-encrypted before reaching durable storage.
Today it has one entry: the digest bodies the validity plane produces. Two tests
hold it honest — every registered column must exist in one of the three schema
registries and be declared `BLOB`, because ciphertext has to fit.

**Anti-relocation.** Each ciphertext is a nonce followed by the sealed bytes,
with table, column and row id bound in as associated data, so copying one into
another row fails authentication rather than decrypting into someone else's
record. The stored form is self-describing — a leading frame byte distinguishes
a plain value from an encrypted one — so rows written before custody existed
coexist with encrypted ones. **One column is deliberately *not* registered**:
the sealed data key, already ciphertext, since encrypting it under the key it
wraps would be circular.

**How much of this is live.** The mechanism is built and proven end to end
against the shipping engine, but on this repository's store the encrypted column
and the custody table both have no rows, because nothing in the shipped path
writes a summary yet ([Chapter 18](18-known-gaps.md)).
[Chapter 4](04-redaction-and-secrets.md) covers the guarantee that matters more
day to day: what never reaches durable storage at all.

---

## 13.9 Two hashes over the same bytes

Rift computes SHA-256 where byte-compatibility with git matters — the hash
chain, a checkpoint's content hash, its id — and blake3 for internal handles.
They are **distinct types with no conversion between them**, enforced by a
doc-test asserting the conversion does not compile, so a blake3 handle can never
masquerade as a chain hash.

- **SHA-256** — the chain, checkpoint ids, the content hash recorded for a file
  read, the derived ids of the recall graph's rows, and the digest locking a
  migration.
- **blake3** — the derived developer and machine identifiers (salted, so the raw
  input never appears), the policy hash fingerprinting an effective
  configuration, the fingerprints an integration writes to prove it owns an
  artifact, and the frozen ids inside the retired serve envelope of §13.10.
- **git's own blob id** — a third digest over a different framing of the same
  bytes, which is why a content hash never coincides with an object id and why
  an explicit bridge table exists
  ([Chapter 5](05-ledger-and-verification.md), [Chapter 6](06-recall.md)).

The worktree hasher surprises people: it computes a **plain SHA-256** of file
bytes, not a domain-separated chain leaf and not blake3, because it compares
against the content hash capture recorded at read time
(`crates/rift-recall-substrate/src/worktree.rs:4-25`).

---

## 13.10 Frozen names and read-compatible history

The recall registry holds tables whose names refer to a serving transport that
no longer exists, and a cache directory segment has the same flavour. Renaming
them would be a mistake three times over: the registry entry *is* the table's
name in every deployed file on disk; the migration bodies are checksum-locked,
so editing a byte refuses every existing store; and the tables are append-only,
so historical rows can never be rewritten under a new name.

They survive as read-compatible history, not live vocabulary. The live serving
axis is a pair of channel-discriminated relations added by a later migration,
and the superseded pair is untouched: the write path that produced their rows
was retired without deleting a table or a row, and no reader targets them
([Chapter 7](07-serving-and-injection.md)). The same applies to the cache
segment that once held connection leases — nothing writes it, but one probe
still has to find leases a previous install wrote.

> **Do not "tidy" a shipped name.** A banner in the schema source says exactly
> this, immediately above the affected block.

---

## 13.11 The engine seam

Everything above is implemented against a trait, not SQLite: the workspace
depends on the seam and a concrete engine plugs in behind it. The shipping
engine is a synchronous `rusqlite` reference implementation running the *same*
generated DDL and triggers any SQLite-compatible engine would, which makes the
immutability and migration guarantees provable offline, with no service and no
network, in a normal `cargo test`.

The seam is **async** even though that engine is synchronous, because the
intended production engine is async-native and the dashboard and serving readers
run on an async runtime — a synchronous seam would force a blocking call inside
an async context, a deadlock hazard. The reference implementation locks one
connection behind a mutex, runs synchronously and releases the guard before
returning, so no guard is held across a suspension point and the default build
needs no runtime.

The intended engine is **deferred, in writing**: its pinned pre-release lacked
trigger creation, strict typing and encryption, and pulled in advisory-bearing
transitive dependencies. A reference implementation of the seam for it exists in
local design notes. Nothing about that decision reaches callers, which is the
argument for a seam.

Engine details you may run into:

**A busy timeout, because hooks write concurrently.** Each captured event is a
short-lived process writing the same file, and without a timeout a colliding
writer fails instantly — which the never-fail hook contract turns into a
silently dropped row. The connection waits up to five seconds instead.

**Lifetime exclusion for live projections.** Ordinary file-backed live stores own a shared guard on their canonical path's stable concurrency marker before SQLite opens.
Outstanding connections and transactions retain that guard after the store owner closes.
Replacement owns the marker exclusively through relocation, recreation and ingest, and refuses promptly while any live guard remains.
Hardlink aliases are refused by ordinary live opens and replacement.
Read-only diagnostics never create the source, parent or marker.
An absent projection is omitted as before; an existing legacy projection without its marker returns an actionable unavailable error.
For recall, `rift-memd metrics` establishes the marker through its writable ingest path; for capture, `rift init` opens the capture projection.
These writable actions differ from read-only `rift doctor` and `rift status`.

**Historical imports use an explicit source route.** `rift backfill --from` and `--recall-from` select a historical artifact, refuse current family/destination aliases, and open SQLite READ_ONLY without creating or migrating its database.
One read transaction pins a consistent snapshot across all imported relations, including pages in the preserved WAL.
An existing source marker still participates in exclusion; a markerless source requires explicit historical selection.
This route does not assert physical immutability: SQLite may manage WAL/SHM files, and an inaccessible journal fails explicitly.

**Change notification is a capability, not an assumption.** The reference engine
reports that it must be polled, and the dashboard's poller keys on a
monotone-under-append watermark plus a rebuild-epoch row; an engine with native
notification overrides the capability without touching callers
([Chapter 12](12-canopy-architecture.md)).

There is also a registry fast-path creating every table and guard directly,
bypassing the migration ledger. It is **test and tooling only** — production
opens go through the migration engine so the ledger and per-table version rows
are populated — and a CI check greps the binaries to keep it so.

---

## 13.12 Rebuilding, and what a rebuild cannot restore

The two per-repository stores rebuild differently. **The recall projection is
set aside and re-derived**: `rift-memd rebuild` preserves the file and its
sidecars to dated siblings, re-opens through the migrations, re-ingests from
git — cold, complete, idempotent. **The capture projection is re-ingested in place**: `rift rebuild`
walks the redacted checkpoint branch through the ordinary append-only path, and
because rows are content-addressed and written with `ON CONFLICT DO NOTHING`, running
it over a healthy projection inserts nothing. It is a repair, not a reset.

A condense writes the shard and its chain entry but not the shard's own rows (`checkpoint`, `session_in_checkpoint` and their siblings), so the detached worker levels the projection once at the end of any drain that wrote a checkpoint, under the queue lock it already holds.
The walk opens only the shards no completed walk has projected: a shard is skipped when it sits at or before the recorded watermark and its `checkpoint` row exists, and that row is the last one a shard's projection writes, so a walk that stopped mid-shard re-projects that shard next time.
Position alone would not do, because the walk order is the chain's order followed by every shard the chain does not name, and a new checkpoint sorts before those.
A projection with no watermark, or one whose watermark names a shard no longer on the branch, is walked in full, which is what `rift rebuild` into an empty projection does; `rift rebuild --json` reports the skipped shards as `shards_skipped` beside `shards_projected`.
It is best-effort: a projection that is being replaced defers it to the next drain without costing any job an attempt, and no hook ever waits on it.

Every session a checkpoint covers is restorable under its own agent when the checkpoint carries `sessions.jsonl`.
The rebuild keys the session's checkpoint link, its session row and its native identity under the agent the line names, with the ids the live path mints, so a Codex or Antigravity session rebuilds onto its live row rather than under a Claude Code key that matches none.
A session the projection has never seen gets a row even when no usage line names it: no model, dated by its first usage line in that checkpoint, or by the checkpoint's own capture time when there is none.
A checkpoint written before the artifact keys every session under the walk's default agent, as it always did, and a session that was never condensed into any checkpoint cannot be rebuilt at all.

A session's worktree observations are restorable when its checkpoint carries `session_worktrees.jsonl`, and only for a state the projection does not already hold.
A line is skipped when either relation has an observation of the same session with the same token, name, linkage, branch, subagent and basis at any head, the rule the usage rows follow made per state, so levelling into the live projection writes nothing.
The artifact is cumulative, so a cold rebuild restores from each checkpoint the states its predecessors did not carry, a subagent's observations first frozen in a later checkpoint among them.
The rebuild writes every other line back into `session_worktree`, or into `session_worktree_cd` for a `shell_cd` line, under the session's own agent.
A restored row carries no head, because no shard does, and its id is the one the live writer would derive for the same state with no head; a session the live hooks saw at several heads of one member is restored as one row, first observed when the earliest of them was.
The path is not in the shard either: the rebuild resolves the token to this machine's checkout of the family member, and writes the empty string when no member here has that token, because no reader spells an unknown path otherwise and another machine's path would name nothing here.
A token is a digest of the checkout's path as it was spelled, so a checkout reached through another spelling also resolves to nothing.
A checkpoint written before the artifact restores no observation, and neither does a session known only through `rift session attach`, `rift session import` or a commit: those verbs' own observations are never frozen, because each is written after the archive it describes and would otherwise change the verb's next checkpoint.

The savings denominator is restorable for every read whose `tool_calls.jsonl` line carries its instant, turn, blob oid and tokenizer.
From such a line the rebuild writes the read's tool call, its file revision, the link between them and its token count, keyed as the live path keys them, so the rebuilt share equals the live one over checkpoints written after those members were added.
A line written before them is still the recall plane's read fact but restores none of these rows, so a read from an older checkpoint stays out of the rebuilt denominator.

Usage and cost are on the restorable side for every shard that carries the usage artifact.
A checkpoint's `usage.jsonl` freezes each settled model call of its session: the call's own key, its turn, its model, when it happened, the four token classes and how they were counted.
The rebuild writes the session, the turn, the model call and its token usage back from each line under the live keys, and a row the live path already wrote wins.
The rebuild may append a booking priced from the catalog as of its supplied clock, because the shard carries no dollar figure.
Current reports quote that same restored usage from the current catalog, independently of when a booking was made; an unpriced model retains its usage with no dollar figure.
What a line cannot say is when its session and turn opened: a session the projection has never seen is dated by its first recorded call.

Some capture facts a rebuild still cannot derive.
Per-commit diff facts come from git history rather than from a shard.
Sessions whose shards were written before the usage artifact existed carry no usage, so their session, usage and cost rows live only in a projection that holds them.
Those have their own verb, `rift backfill`, which is why §13.11's read-only open exists: the set-aside file must be readable without being created, migrated or modified.

And the standing exception: **governance records are not rebuildable.** Serve
audit rows, outcome labels, evaluation runs and key metadata record what this
machine did, not what git contains, so deleting a recall projection discards
them — the honest trade the split makes.

The serves are the part of that exception a shard can carry.
A checkpoint's `serve_audit.jsonl` holds the serves the projection attributes to its session, and the recall ingest hands each line back to the one appender the live path writes through.
A line names its session and, where it was known, its window, and the ingest writes the `memd_audit_session` row back from them, so a rebuilt projection attributes every serve a shard carries to the session the live one did; a line without a session writes no attribution.
The appender is keyed by the envelope's own idempotency key, so a rebuilt row lands on the row the live serve wrote, never beside it.
A rebuilt row must carry the instant the live row carries, never the rebuild's clock.
So the ingest reads every schema-4 line, whose instant is its `serve_call.recorded_at`, and a schema-1 line only when it carries a top-level `recorded_at`; a schema-1 line without one is reported as a parse issue and skipped.
The incremental ingest reads the artifact the first time it projects a checkpoint, so a live projection and a rebuilt one converge on the same rows.
What a rebuild cannot restore is a serve no shard carries — one no session made — and the typed words of an exact retrieval's reason: the rebuilt row carries the placeholder `withheld` there.
A pull's claim is restored too, for every shard written since a frozen line could carry it: the line's payload holds `pull_claim`, the three counts of `memd_audit_pull_claim` and the rule its credit was booked under, and the appender writes the claim row back under the serve's own key.
A line from an older shard carries no claim, so its serve is rebuilt without one.

That is why the rebuild sets the old projection aside and never deletes it.
It preserves `recall.db` as `recall.db.set-aside-<YYYYMMDDTHHMMSSZ>`, stamped once from the binary's clock, and prints the new name on the line before `rebuilt:`; a rebuild over no projection prints nothing there, and the fingerprint it prints is the one it always printed.
A sidecar keeps its SQLite suffix after the new name (`…Z-wal`), so SQLite pairs the set-aside file with its own journal when it is opened, and a name already taken gains a counter instead of being overwritten.
The complete DB/WAL/SHM set is linked without overwriting any existing path before any original path is removed.
A failure restores the complete original when possible; failed restoration retains the complete preserved set and names its full path with recovery steps.
Keep the preserved triple together while restoring the original or quarantining its incomplete paths.
Backfill refuses an artifact that still aliases the live destination; restore in place or create a distinct live projection before importing the preserved artifact.
`rift backfill --recall-from <file>` reads the serves back: it opens the file read-only, as the capture import opens its own, refuses a file that is not a recall projection, and replays `memd_audit_event`, `serve_call_event`, `serve_session_event`, `memd_audit_citation`, `serve_grant_event`, `memd_audit_session` and `memd_audit_pull_claim` into the family's recall projection.
A source set aside before claims were recorded has no claim relation and imports as it always did.
There is no second writer.
Each decision and call row is spelled back into the envelope that books exactly that row, a decision's claim included, and handed to the one appender, which writes the citation, first-served, claim and attribution rows as it does live; a grant goes through its own writer.
Every write is insert-or-ignore on its key, so a second run inserts nothing, and `--dry-run` inserts no canonical rows.
The set-aside files are records: `rift clean` never removes them, and `rift doctor` lists them by name, size and age.

> **Tip:** when a projection looks wrong: rebuild the binaries, rebuild the
> projection, run `rift verify`. If verify was already green, the projection was
> the only thing at risk, and it is derived.

---

## 13.13 The dashboard store

The third store holds no captured data, only what the dashboard remembers. It is
machine-global, repository-independent and modelled as **events rather than rows
you edit**: which repositories are listed, how panels are arranged, which
profile is active, what colour a slot is, which profile launch opens — all
superseding events reduced by a "latest valid per key" fold.

| Table | Folds to |
|---|---|
| `canopy_project_event` | the global catalogue of registered repositories |
| `canopy_toggle_event` | per-repository feature toggles |
| `canopy_layout_event` | per-repository panel arrangements (frozen for writes) |
| `canopy_profile_event` | which profiles exist, their names, and which is selected |
| `canopy_profile_project_event` | a profile's membership and ordering |
| `canopy_profile_layout_event` | the arrangement for one (profile, repository) pair |
| `canopy_profile_theme_event` | a profile's colour overrides, per slot |
| `canopy_profile_startup_event` | the startup pin (a `NULL` profile means "follow the last selected") |

Every mutation the reducer emits is an insert-only operation, and an
exhaustiveness test makes sure a new one cannot be added without deciding where
its event goes. Some refusals live in the reducer *and* again in the store:
removing the active profile, or the last remaining one, is turned away twice.
[Chapter 12](12-canopy-architecture.md) §12.10 covers how the folds behave —
seeded defaults, the frozen layout table read as a fallback, copying versus
duplicating, a launch that writes nothing.

One refusal has a type of its own: the catalogue holds **one row per repository
family**, so registering a second checkout of a repository that already has a
row is turned away, carrying the path that holds it. It is a rule, not a
failure, rendered as a finished sentence because the dashboard and the CLI print
the same words.

---

## 13.14 Adding a table

Append the column list and table entry to the appropriate registry — at the end,
past every frozen boundary, never in the middle — then add a **new** migration
version rendering exactly that slice, and register a highly sensitive column in
the sensitive-column registry as a `BLOB`. Decide the shape honestly: a sparse
fact wants a missing row rather than a nullable column, and derived versus
governance decides whether a rebuild restores it (§13.12). Then run the suite —
immutability tests exercise your table automatically, the DDL snapshots show it
as a reviewable diff, the additive guard rejects a destructive migration, and
the keystone fails loudly if you migrated a table without registering it. You
write no triggers, no `CREATE TABLE`, and no append-only test: all three are
generated.

---

## 13.15 What is designed and not built

Two things here are plans, not code. Serving the delta model from an in-memory
index built from git at session start — which would shrink the recall store to
governance facts and retire the derived graph tables — **was never built**: the
graph tables are live, written on every ingest, and validity is backed by recall
store tables. And the capture store's rebuild-epoch helper, which deletes a
projection file and writes the next epoch, is real and tested but has no caller:
the capture rebuild verb re-ingests in place
([Chapter 18](18-known-gaps.md)).

---

## Related chapters

[05-ledger-and-verification.md](05-ledger-and-verification.md), the git-side
record this projection derives from · [06-recall.md](06-recall.md) ·
[08-cost-and-savings.md](08-cost-and-savings.md) for the price catalogue on
§13.7 · [12-canopy-architecture.md](12-canopy-architecture.md) ·
[14-sync-readiness.md](14-sync-readiness.md) ·
[16-development.md](16-development.md) · [17-invariants.md](17-invariants.md).
