# 19. Glossary

Every term this guide leans on, defined once, with the chapter that owns it.
Read it as an index: come here when a word in another chapter did not land, take
the two sentences, follow the arrow back to the chapter that unpacks it. Where a
definition and a chapter would disagree, the code decides and both were
rewritten. A code name in backticks is the type, constant, command or file that
embodies the term, so you can grep for the real thing.

> **Tip:** if you are reading front to back, skip this chapter. It is most useful
> *after* you have met a term in context.

---

## A

- **Accounting seam** — the single function every serve response leaves through:
  serialize once, count the real bytes with the real tokenizer, perform the one
  budget charge, seal the audit event from those same two figures.
  → [07 · Serving and injection](07-serving-and-injection.md)

- **Adoption** — provisioning a linked checkout for the agents its repository
  family has already enabled, using the family's recorded binary. It is explicit:
  a git hook, a verb, or the doctor's repair trigger it, and a failure leaves a
  marker rather than retrying forever. `rift worktree adopt`.
  → [09 · Worktrees and families](09-worktrees-and-families.md)

- **Agent hook** — a hook installed into a *coding assistant's* project
  configuration, invoked as `rift hooks <agent> <verb>` with one JSON object on
  standard input. Distinct from a git hook: it can never refuse anything, and it
  always exits successfully. → [03 · Capture](03-capture.md)

- **Amendment** — a change Rift makes to an agent's own tool call at the
  pre-tool boundary: only by adding or substituting a field the client's
  published schema documents for that purpose, only in a closed and pinned set
  of forms, never on a decline and never any other field. What the tool ran
  with is compared afterwards against what Rift sent, so a lost amendment is
  discovered rather than assumed.
  → [07 · Serving and injection](07-serving-and-injection.md)

- **Answered listing** — a decline that stands in for a name search: the ranked
  head of the matching paths, the total it is a head of, and the fact that
  repeating the call runs it in full. Made once per pattern and directory per
  context window, only from an index already on disk, and not at all when the
  frame would cost more than the listing it replaces.
  → [07 · Serving and injection](07-serving-and-injection.md)

- **Append-only** — the standing rule that canonical rows are never updated and
  never deleted; a correction is a new row superseding an old one. Enforced three
  times over: the store trait, a pair of triggers per table, a property test.
  → [13 · Storage and schema](13-storage-and-schema.md)

- **Artifact bridge** — the authoritative reconciler: it walks the redacted
  checkpoints in git, refuses any without a valid redaction report, re-derives
  canonical events from each transcript and projects them through the live
  path's writers. Git is the record, so the bridge's view wins. `rift rebuild`.
  → [03 · Capture](03-capture.md)

---

## B

- **Bar** — a declared performance limit a published measurement is judged
  against: a floor for a throughput row, a ceiling for a latency row. A bar is
  either derived from recorded medians by a stated convention or set outright by
  the project owner; the four standing bars today are owner-set.
  `benchmarks/floors.toml`. → [15 · Benchmarks](15-benchmarks.md)

- **Binary of record** — the absolute path to the `rift` executable a repository
  family's git hooks point at, recorded as a fact with a capped history.
  Re-pointing it is an explicit act, because a family silently re-pointed at a
  temporary build stops capturing without one error line. `binary.json`,
  `rift hooks install --repoint`.
  → [09 · Worktrees and families](09-worktrees-and-families.md)

- **Bitemporal fact** — a row carrying two time axes: when the fact was true in
  the world, and when this machine came to believe it. Prices and cost facts are
  bitemporal, so both questions stay answerable without editing a row.
  `valid_from` / `valid_to` / `recorded_at`.
  → [13 · Storage and schema](13-storage-and-schema.md)

- **Blob-id map** — the append-only table bridging a file's content hash to the
  object id git stores it under. The two digests frame the same bytes differently
  and never coincide, so without the map a durable lookup asks git for an id it
  has never heard of. → [06 · Recall](06-recall.md)

- **Block expansion** — the parser-free window rule for pulled hunk content:
  grow each changed range to the nearest blank or column-zero line on each
  side, up to a cap, else fall back to a fixed number of context lines. It is a
  layout heuristic and never a claim about syntax, which is why it can ship in
  a toolchain that runs no language parser.
  → [07 · Serving and injection](07-serving-and-injection.md)

- **Bundle** — the offline transfer unit: one self-contained file carrying a
  manifest, a chain segment and the git objects, framed so every claim is
  checkable before a single object moves. Import is all-or-nothing and writes no
  database rows. `rift bundle export|import`.
  → [14 · Sync readiness](14-sync-readiness.md)

---

## C

- **Canonical event** — the agent-independent record of one lifecycle fact:
  session start, turn end, tool call, file edit, and eight more. Adapters
  translate their client's verbs into it and have no authority over identity,
  redaction or the chain. → [03 · Capture](03-capture.md)

- **Canopy** — the terminal dashboard you get by typing `rift` with no arguments:
  three columns over one repository at a time, plus the context gauge, the
  command surface and the read-only viewer.
  → [11 · A tour of Canopy](11-canopy-tour.md)

- **Chain ref** — the git reference whose history *is* the order of the
  checkpoints: one empty-tree commit per checkpoint, each parented on the
  previous, the checkpoint's identifier riding a commit trailer.
  `refs/rift/chain/v1`, `rift chain status`.
  → [05 · The ledger and verification](05-ledger-and-verification.md)

- **Checkpoint** — what a session's work condenses into: a directory of small
  redacted files committed onto a reference of Rift's own, with a Merkle root and
  a chain entry behind it. Its identifier is derived from that root and *is* its
  path, so editing a byte moves the address rather than corrupting the record.
  → [05 · The ledger and verification](05-ledger-and-verification.md)

- **Checkpoint strategy** — the per-repository setting deciding *when* a live
  session condenses: on your commits (the default), on a push, on an interval, or
  on policy signals alone. A touched sensitive path archives immediately under
  any of them, which is why there is no "off". `checkpoint_strategy`.
  → [03 · Capture](03-capture.md), configured in
  [02 · Configuration](02-configuration.md)

- **Compounding** — recall work earned by one checkout accruing to every other
  checkout of the same repository: read facts, digests, hunk indexes and packs
  are computed once per content and served family-wide. Measured by a fixture and
  a published harness row, not asserted.
  → [09 · Worktrees and families](09-worktrees-and-families.md)

- **Condense** — the operation turning a live session's raw capture into a
  durable checkpoint: redact first, write the shard, append to the chain.
  Re-condensing an unchanged session is a no-op, because identical bytes give the
  same identifier and therefore the same path. → [03 · Capture](03-capture.md)

- **Containment** — the rule bounding what a recall query may see: a checkpoint
  reaches a checkout only when its **anchor commit** — the commit the checkpoint
  is about — is reachable from that checkout's current position. Unknown
  position, unreachable commit, or a walk that hit its cap all exclude and are
  counted. Never widen on uncertainty.
  → [09 · Worktrees and families](09-worktrees-and-families.md)

- **Context pack** — one ordered, budgeted evidence section compiled for a turn,
  whose placement the agent does not choose. It cites evidence by ordinal, and is
  written but never parsed back. `rift recall context-pack`.
  → [07 · Serving and injection](07-serving-and-injection.md)

- **Context pressure** — how full the model's context window is, as a percentage
  and one of five neutral bands: Clear below 50%, Moderate below 70%, High below
  85%, Very High below 95%, Critical at 95 and above. The mapping is provably
  monotonic, and an unknown model yields no percentage rather than an invented
  one. `rift-nic-gauge`. → [11 · A tour of Canopy](11-canopy-tour.md)

- **Coverage** — the merged set of line intervals of one file *revision* that a
  context window has actually been handed, recorded conservatively from pinned
  tool semantics and scoped to the content hash, so a changed file starts with
  none. It is what makes "you already hold this" a fact rather than a guess.
  → [06 · Recall](06-recall.md)

- **Coverage floor** — a minimum test-coverage percentage the build enforces: 85%
  workspace lines, 75% workspace branches, and 95% lines for each crate on the
  security-critical list. It fails closed — a listed crate missing from the
  report, or a report carrying no branch data, is a violation rather than a quiet
  pass. `ci/security-crates.txt`. → [16 · Developing Rift](16-development.md)

- **Custody manifest** — the fingerprint record proving which integration files
  Rift wrote, so a re-run tells "mine, refresh it" from "somebody else's, leave
  it alone". Where it cannot vouch — a fresh clone, a wiped state directory — a
  shape recogniser stands in. `.rift/state/native-integrations.json`.
  → [10 · Agent integrations](10-agent-integrations.md)

---

## D

- **Declared baseline** — the per-format figure a document's saving is measured
  against: what those bytes would have cost the reading tool, stated in the
  fixture with its formula and marked as declared. A PDF's is **zero**, because
  its native cost is a page count against a vision rate nobody here has pinned,
  so its savings are excluded rather than estimated.
  → [08 · Cost and savings](08-cost-and-savings.md)

- **Decline** — the pre-tool answer that cancels one read and gives the model a
  reason, on a channel that client documents. It is a decision, not a block: the
  hook still exits successfully, and the **identical repeat is allowed** — one
  decline per key, which bounds it to a single extra round trip and makes
  starving an agent structurally impossible.
  → [07 · Serving and injection](07-serving-and-injection.md)

- **Delta frame** — the decline for a file the window holds at an *older* hash,
  naming the changed sites in current numbering, the net line shift and a window
  to read instead. It is emitted only when the difference is known exactly; when
  no site survives the clamp against the current line count there is no frame
  and the read runs.
  → [07 · Serving and injection](07-serving-and-injection.md)

- **Delta manifest** — the short per-turn text Rift pushes into an agent's
  context naming the files it read that have changed since: a pointer at a delta,
  not the delta. What it reports is folded into the session's baseline so nothing
  is announced twice. → [07 · Serving and injection](07-serving-and-injection.md)

- **Detached worker** — the one-shot child process a hook spawns and does not
  wait for, which drains the durable job journal: token counts, then condenses,
  then queued commits. It is how work that cannot fit a hook's budget still
  happens without a resident service. `RIFT_CAPTURE_SYNC=1` runs it inline.
  → [03 · Capture](03-capture.md)

---

## E

- **Edit-derived coverage** — coverage an agent holds *because it wrote the
  bytes*: the whole file after a write whose result hashes to what the agent
  supplied, or after a replacement Rift can locate uniquely in the new bytes.
  Anything ambiguous grants none. → [06 · Recall](06-recall.md)

- **Era** — the canonicalization rules a checkpoint was minted under, declared in
  its own header and read from there rather than from a date. There are four;
  relabelling downward does not buy the looser rule, because two eras produce
  different identifiers for the same bytes.
  → [05 · The ledger and verification](05-ledger-and-verification.md)

- **Eviction boundary** — the point at which a context window is emptied — a
  compaction, a clear, a resume — after which earlier coverage no longer
  supports a decline. The lines before it are kept, because they are what the
  recovery map is built from.
  → [06 · Recall](06-recall.md)

- **Exact retrieval** — the one path by which stored bytes actually leave: it
  needs a handle or a pack ordinal, a stated reason, a policy pass, and it writes
  an audit row before anything returns. Raw transcripts are off by default.
  `rift recall retrieve-exact`. → [07 · Serving and injection](07-serving-and-injection.md)

- **Extractor** — a versioned, purely mechanical digest producer — failure
  signatures, log shape, diff counts, file shape. Same bytes in, same bytes out;
  no clock, no model. Determinism makes a digest rebuildable and stops a
  directive in a log file being paraphrased back at an agent.
  → [06 · Recall](06-recall.md)

---

## F

- **False-positive census** — the measured cost of the redaction pipeline firing
  on ordinary repository text, held per detector by a ceiling a test reads back
  out of the published artifact. Every numeric personal-data detector has a
  ceiling of exactly zero, and a new detector cannot land without a measured cost.
  → [04 · Redaction and secrets](04-redaction-and-secrets.md)

- **Family** — one repository plus every linked checkout of it: the set sharing a
  git *common directory*, and therefore sharing objects, references and hooks.
  Rift treats a family as one repository with one identity, one capture
  projection, one recall store, one binary of record and one registry row.
  → [09 · Worktrees and families](09-worktrees-and-families.md)

- **Family manifest** — the small machine-local file in the common directory
  holding a family's minted identity, never tracked, written once and rewritten
  only by an explicit repair. Nothing re-derives identity from a path afterwards,
  which is what makes moving a repository survivable. `family.json`,
  `family.history.jsonl`. → [09 · Worktrees and families](09-worktrees-and-families.md)

- **Formatter drift** — a whole-file write whose result does not hash to what
  the agent supplied, because a save hook or formatter changed the bytes. The
  agent does not hold what is now on disk, so nothing is recorded as coverage
  and the verification read runs. → [06 · Recall](06-recall.md)

- **Four-state envelope** — the closed set of states every served artifact is in:
  unchanged relative to what you named, a validity-checked summary inline, exact
  bytes inline, or nothing inline plus a handle and a reason. Two of the four
  carry bytes and charge budget; the other two cost a path and a hash.
  `known_unchanged`, `summary_included`, `exact_included`, `available_by_handle`.
  → [06 · Recall](06-recall.md)

- **Frecency** — how warm a path is, as a score over facts Rift already
  records: how often and how recently it was read, how recently it changed, and
  what git says about it. There is no separate database behind it, which is why
  a command-line search can gather every member's reading while a search
  answered inside a hook uses the one ledger it can afford to open.
  → [06 · Recall](06-recall.md)

---

## G

- **Grant** — an explicit, reasoned widening of what one serve may see, today
  only across branches. The reason is required and audited twice: a fixed neutral
  tag in the frozen audit column so "which serves were widened" is a query, and
  your own words in a separate relation that stores them and never serves them
  back. `--cross-branch <REASON>`. → [07 · Serving and injection](07-serving-and-injection.md)

---

## H

- **Handle** — an unforgeable reference to content held outside the prompt: a
  file revision addressed by path *and* content hash, an immutable captured tool
  result, or a hash-bound summary key. Resolution re-hashes what it fetched, so a
  handle never resolves to "the current file at that path".
  → [06 · Recall](06-recall.md)

- **Hash families** — the two digests Rift computes, distinct types with no
  conversion between them: SHA-256 wherever byte-compatibility with the git
  substrate matters — the chain, a checkpoint's content address, a checkpoint's
  identifier, the content hash recorded for a file read — and blake3 for internal
  identifiers such as the salted developer and machine digests, the policy hash,
  and integration ownership fingerprints. A third digest, git's own object id,
  frames the same bytes differently again, which is why the blob-id map exists.
  `Sha256Id`, `Blake3Id`. → [13 · Storage and schema](13-storage-and-schema.md)

- **Held set** — the `(path, hash, coverage)` triples a context window
  currently holds. Manifest entries are drawn from it first and the session
  baseline second, which is what makes "files you have read changed" literally
  true where the record has lines. → [06 · Recall](06-recall.md)

- **Honest absence** — the standing rule that a fact which cannot be established
  is stated as absent rather than approximated. A missing ledger reads "no ledger
  here", never zero; an unpriced model shows no dollar figure; an unknown
  checkout prints `unknown`, never `main`. Absence and a measured zero are
  different facts and are drawn differently. → [17 · Invariants](17-invariants.md)

- **Honesty figure** — `avoided − returned` over a class's own audit rows: the
  standing test that a lever's frames, maps and sidecars cost less than the
  reads they displaced. A class that fails it is turned off by data rather than
  defended. → [08 · Cost and savings](08-cost-and-savings.md)

- **Hook receipt** — the row every agent hook writes whatever happened, with one
  of three outcomes: `recorded`, `skipped-unknown-verb` (counted, so the gap is
  visible) or `deferred-no-session` (no usable session identifier). It exists so a
  loss is countable instead of silent. → [03 · Capture](03-capture.md)

---

## I

- **Injection budget** — the token ceiling one turn's pushed manifest may cost,
  set per repository in the serving overlay. The default is small on purpose; `0`
  turns per-turn injection off outright, and absence means the default rather
  than zero. `injection_turn_budget`. → [02 · Configuration](02-configuration.md)

---

## K

- **Known-state anchor** — the manifest call opening a turn, which resets the
  turn's draw-down while keeping its ceilings. "What changed?" already precedes
  any retrieval, so anchoring imposes no new protocol, and a caller that never
  anchors runs under one un-reset budget rather than an unbounded one.
  `rift recall known-state`. → [06 · Recall](06-recall.md)

---

## L

- **Layout model** — the dashboard grid as a *value* rather than three coupled
  constants: an ordered list of columns, each a width weight and an ordered list
  of units. The default reproduces the fixed grid exactly, which is what let the
  panel editor land as a pure addition. → [12 · How Canopy is built](12-canopy-architecture.md)

---

## M

- **Manifest** — in the recall plane, a list-shaped answer naming what changed
  and what you already hold without carrying the content: the default response is
  a manifest, never bytes. (Two unrelated things share the word: the *custody*
  manifest that proves file ownership, and a *bundle's* manifest of what a
  transfer file claims. Both are named where they appear.)
  → [06 · Recall](06-recall.md)

- **Member** — one checkout in a repository family, either the main work tree or
  a linked one. A member is never a project of its own: it is listed under its
  family in the dashboard and the registry, and it carries its own session caches
  and capture queues while sharing the family's stores.
  → [09 · Worktrees and families](09-worktrees-and-families.md)

- **Merkle root** — the single hash committing to all of a checkpoint's content
  files, built from per-file leaves in the shard's bytewise path order. Each layer
  carries a one-byte domain prefix so a leaf hash cannot masquerade as a node
  hash, and an inclusion proof shows one file belongs to a checkpoint without
  handing over the checkpoint.
  → [05 · The ledger and verification](05-ledger-and-verification.md)

- **Migration** — one numbered, named block of schema SQL whose digest is
  recorded when it is applied. Migrations are additive and checksum-locked: a
  schema change is a *new* migration, never an edit to an applied one, and
  destructive statements are refused before they run.
  → [13 · Storage and schema](13-storage-and-schema.md)

- **Modal-surface parity** — the contract binding anything drawn as a centred
  frame over the shared modal geometry: it carries the shared close control on
  every stage it can show, every act its hints teach is reachable by click on
  that surface, every clickable has a key path, and hints derive per stage so a
  dead key is never advertised. Held by three tests, not by memory.
  → [12 · How Canopy is built](12-canopy-architecture.md)

---

## P

- **Pack key** — a compiled pack's identity, computed from its *inputs* — the
  query, the ranked artifact identifiers and their content hashes in rank order,
  the policy version, the budget — and never from its rendered output. Keying on
  the output would be circular and uncomputable before rendering.
  → [06 · Recall](06-recall.md)

- **Path index** — the per-member file listing every path this checkout may
  answer a name search with, and the modification time each had, keyed by the
  commit HEAD points at and the newest modification time the walk saw. A key
  that does not match, a damaged file or one past its size bound all mean the
  same thing: no index, so the agent's own call runs and a rebuild is asked of
  the worker. Names and times only, never content. `.rift/state/search/index.bin`.
  → [06 · Recall](06-recall.md)

- **Pinned channel** — a `(client, verb)` pair Rift answers on because a
  committed fixture carries that client's published response schema for it:
  repository, tag, path, blob hash and a verbatim excerpt. No fixture, no
  response — which is how "no invented shapes" became a property of the tree
  rather than a promise.
  → [10 · Agent integrations](10-agent-integrations.md)

- **Policy hash** — a digest of a repository's *effective* configuration content,
  referenced by captured records so "which policy was in force when this was
  recorded" always has an answer. The checkpoint remote and the policy's
  provenance are deliberately excluded: both would make identical policies hash
  differently. → [02 · Configuration](02-configuration.md)

- **Price catalogue** — the append-only table of per-model, per-class prices, each
  row carrying both time axes. Resolution returns a cost only when all four token
  classes have a row on file; a partially-priced model produces no cost fact
  rather than a total that silently omits output tokens. Money is integer
  micro-dollars end to end. → [08 · Cost and savings](08-cost-and-savings.md)

- **Profile** — a named view of the dashboard: its own project membership over
  the machine-global catalogue, its own per-project arrangements, and its own
  colours. Exactly one profile is active, every profile act is a new entry in an
  append-only log, and launch reads the pin without writing anything.
  → [11 · A tour of Canopy](11-canopy-tour.md)

- **Projection** — a local database derived from the git record: fast to query,
  destroyable, rebuildable, and never the authority in a comparison. The exception
  is the **governance records** — what was served, what an evaluation concluded —
  facts about this machine that are not rebuildable.
  → [13 · Storage and schema](13-storage-and-schema.md)

- **Provenance** — the label saying which checkout a captured fact came from.
  There are two: a session's checkout is an append-only *observation*, while a
  checkpoint carries the checkout of the first archive that minted it — which is
  why every surface prints *archived in*, never "worked in". Absent means
  unknown, never main. → [09 · Worktrees and families](09-worktrees-and-families.md)

- **Push queue** — the family-wide directory of references waiting to be shipped,
  written at the chain append and drained by an explicit flush or a push to the
  configured remote. An entry names a reference and a stream position and carries
  no capture bytes: everything a shipper needs is already in git.
  → [14 · Sync readiness](14-sync-readiness.md)

---

## R

- **Read record** — the append-only file per context window under
  `.rift/state/reads/`, holding the ranges that window was handed, the declines
  issued, the hashes its writes were asked to produce, and its eviction
  boundaries. Paths, hashes and counts only, never content; local run state,
  never a canonical table. → [06 · Recall](06-recall.md)

- **Reason frame** — the sealed text of a decline: fixed phrases authored in
  code, variable spans through the same allowlist a manifest entry passes, no
  file content, no placeholder brackets, no error word. It states the fact, the
  repeat escape and one cheaper route.
  → [07 · Serving and injection](07-serving-and-injection.md)

- **Record (a measurement)** — a benchmark artifact with no commit provenance, or
  one taken over a tree with uncommitted changes. A record is kept, quotable as
  history with its date and conditions, and barred from deriving a bar. Every
  tracked artifact is a record today, which is why the standing bars are
  owner-set. → [15 · Benchmarks](15-benchmarks.md)

- **Re-query rate** — the share of bounded content searches a context window
  asked again in the same turn. It is what the bounding lever publishes instead
  of a saving, because a bound the model immediately works around moved the cost
  rather than removing it; defined in one place, the window's own record, so no
  surface can recompute it more kindly.
  → [08 · Cost and savings](08-cost-and-savings.md)

- **Recovery map** — the boundary-only injection after a context is emptied:
  the pre-eviction held set as path, hash and ranges, each labelled unchanged
  or changed at named lines, ranked by recency and fitted to its own budget. It
  is never injected on an ordinary turn, and it does not re-arm declining.
  → [07 · Serving and injection](07-serving-and-injection.md)

- **Redaction gate** — the one-way, total boundary between raw and durable bytes.
  Raw material has a type that cannot be persisted and is wiped from memory when
  dropped; the durable write path accepts only the sealed type, whose buffer is
  private and whose sole constructor is called in exactly one function.
  Persisting a raw byte is a compile error. `SecretBytes`, `RedactedBytes`,
  `RedactedBytes::seal`. → [04 · Redaction and secrets](04-redaction-and-secrets.md)

- **Redaction report** — the counts-only artifact frozen into every checkpoint:
  totals per detector and per class, bytes masked, which rule packs ran, the
  address of the raw input. It is structurally incapable of carrying a secret, and
  verification refuses a checkpoint whose report is missing or invalid.
  `redaction_report.json`. → [04 · Redaction and secrets](04-redaction-and-secrets.md)

- **Redirect** — the document channel for a client that cannot have a read
  amended, or for one whose amendment was dropped: the read is declined once
  and the reason names the sidecar to read instead.
  → [07 · Serving and injection](07-serving-and-injection.md)

- **Render seam** — the single trait every cell the dashboard draws passes
  through, with two implementations: a plain-text floor always compiled in, and a
  richer one behind an opt-in build feature. Panels see neither; they draw into a
  frame the backend flushes. The **render tier** — full colour, 256 colour,
  monochrome, plain text — is chosen from a capability set rather than from the
  terminal. `RenderBackend`. → [12 · How Canopy is built](12-canopy-architecture.md)

- **Rule pack** — a versioned file under the repository's redactor directory
  adding detectors for shapes only your team knows. Packs are additive only —
  nothing in one can disable a built-in detector — a rule that disagrees with its
  own sample is kept with a warning, and a broken pack can never fail a commit.
  → [04 · Redaction and secrets](04-redaction-and-secrets.md)

---

## S

- **Savings share** — `avoided ÷ (avoided + read)` since the last window
  boundary, the numerator from the audit relation and the denominator from
  captured read facts, computed once for every surface that prints it. A second
  variant drops families that decided but were never observed reading, and names
  them. `rift savings`. → [08 · Cost and savings](08-cost-and-savings.md)

- **Savings window boundary** — the family-wide marker saying where the savings
  share starts counting, written by an explicit reset and absent by default,
  where absent means the whole record. Every surface prints the boundary it
  used, so a share on the command line and a share on the dashboard cannot be
  two different periods; deleting the file widens the window back and loses
  nothing measured. `savings-window.json`.
  → [08 · Cost and savings](08-cost-and-savings.md)

- **Scope** — the bound on what one recall query may read: repository, branch
  ancestry, checkout, developer, and a checkpoint window. Enforced at the query
  layer and property-tested, because it is a security property; widening past the
  branch axis needs a grant. → [06 · Recall](06-recall.md)

- **Sealed constructor** — the single function producing a given served artifact
  — the four-state envelope, the validity verdict, the pack sections — with
  handlers calling it rather than rebuilding a result inline. A guarantee
  re-implemented at the call site is not a guarantee; it is a comment.
  `ValidSummary`, `envelope_for`. → [06 · Recall](06-recall.md)

- **Session** — the unit capture is organised around, driven by a pure state
  machine: given a state and a trigger it yields the next state and a list of
  *requested* effects, and the caller performs or journals them. An illegal
  transition is refused, logged and swallowed at the hook boundary.
  `rift session list|info|show`. → [03 · Capture](03-capture.md)

- **Shard** — a checkpoint on disk: the directory of small files under a path
  derived from the checkpoint's identifier — the redacted capture document, the
  redaction report, the transcripts, the tool calls, the four mechanical digests,
  the content address. The file list is a registry in code, and a test asserts the
  tree git holds is exactly the declared set.
  → [05 · The ledger and verification](05-ledger-and-verification.md)

- **Sidecar** — the Markdown conversion of a document, keyed by the document's
  bytes together with the converter's version and the caps that shape its
  output, written once per repository family and viewed per member as a
  hardlink. A cache, never a record: deleting the store costs one reconversion.
  → [07 · Serving and injection](07-serving-and-injection.md)

- **Stream cursor** — one consumer's position in the append-only capture
  sequence, as a single text token: a contract version, a one-based position in
  the chain's linkage, and the checkpoint identifier at that position. Position
  orders; identity binds the cursor to one chain, so a resume against a diverged
  chain fails closed. Not the change-detecting watermark.
  → [14 · Sync readiness](14-sync-readiness.md)

- **Strict shell grammar** — the narrow shape a shell command must match to
  count as a read: one simple command, one work-tree path, no pipes, redirects,
  globs, variables or chaining, and only the spellings a client's own
  read-before-edit list uses. A non-match is never touched and never guessed at;
  the recogniser is fuzzed. → [07 · Serving and injection](07-serving-and-injection.md)

---

## T

- **Theme slot** — one of five colour roles a profile may repaint: background,
  border, button, graph and text. Meaning-bearing colours — health, severity, the
  gauge ramp, the token-class map — are never slots: a theme can restyle the
  dashboard, never repaint meaning. The picker offers ten named presets per slot,
  and what is stored is the resolved colour value rather than the preset's name.
  → [12 · How Canopy is built](12-canopy-architecture.md)

- **Token classes** — the four separately-billed kinds of token recorded per
  model call and priced independently: fresh input, input served from the
  provider's prompt cache, tokens written into that cache, and output. A single
  blended rate is not a simplification but a wrong answer — on this repository the
  cached-read column is three orders of magnitude larger than the fresh one.
  `uncached_input`, `cached_input`, `cache_creation`, `output`.
  → [08 · Cost and savings](08-cost-and-savings.md)

- **Tokens returned / tokens avoided** — the two figures every serve reports.
  *Returned* is a measurement: the token count of the payload that shipped.
  *Avoided* is an estimate and specifically an upper bound: the baseline the
  response stood in for, minus what the response itself cost, saturating at zero.
  It is counterfactual-free — it does not know whether a re-read would have
  happened — and no surface may promote it to a claim.
  → [08 · Cost and savings](08-cost-and-savings.md)

- **Trust class** — the class every payload carries, derived from where the bytes
  came from in the ledger and never from the caller: code fact, git fact, test
  output, untrusted tool output, untrusted transcript. The last two may never be
  inlined raw; anything that does not parse fails closed to untrusted. `Trust`.
  → [07 · Serving and injection](07-serving-and-injection.md)

- **Turn budget** — the two ceilings a turn's retrieval runs under: four tool
  calls and two thousand returned tokens, held by the caller and reset by the
  anchoring manifest call. The token ceiling uses saturating arithmetic, so an
  enormous figure cannot wrap the counter back under it. `TurnBudget`.
  → [07 · Serving and injection](07-serving-and-injection.md)

---

## V

- **Validity binding** — a summary keyed by a hash over the bytes it describes
  plus the extractor, policy and redaction versions. Change any one and the key
  changes, so a stale summary is not "invalid" but *unlookupable* — nothing has
  to remember to delete it and nobody can accidentally find it.
  → [06 · Recall](06-recall.md)

- **Vocabulary screen** — the build check scanning every tracked file's path
  bytes and content bytes for retired planning shorthand: numbered identifiers
  and campaign labels older design notes used instead of saying what a thing
  does. There are no exclusions — this guide is in the corpus — and a scan that
  examined nothing is itself a failure. The fix for a hit is never an exclusion;
  it is to say the thing in words. → [16 · Developing Rift](16-development.md)

---

## W

- **Watermark** — the dashboard's change detector, read as a pair: an append
  cursor moving whenever capture writes, and a rebuild epoch moving only when a
  projection is rebuilt. It answers "has anything landed since I looked", never
  "which events", and may legally reset backwards — which is why a resume uses a
  stream cursor. (The capture ingest keeps its own watermark, the last checkpoint
  projected per branch, so re-ingest is a no-op.)
  → [12 · How Canopy is built](12-canopy-architecture.md)

- **Worktree** — a linked git checkout: a second door into one repository, not a
  second repository. It shares the common directory and therefore objects,
  references and hooks, keeping its own git directory for private progress. In
  Rift's vocabulary a worktree is a *member* of a family.
  `rift worktree list|status|adopt|prune`.
  → [09 · Worktrees and families](09-worktrees-and-families.md)

---

## Terms deliberately retired

These words appear in older material about this toolchain. They are listed so
that meeting one tells you something useful — that you are reading something out
of date. **None of them describes anything Rift does today.**

- **Wrapper backend, route table, promotion ladder, kill-switch file.** Rift once
  ran another capture tool underneath itself, dispatching per capability while
  replacing it. Capture, condensation, redaction and archival are native end to
  end now, and Rift runs no other binary to do its own work; what remains is a
  read-only importer for the older on-disk shapes, permanently.
  See [05 · The ledger and verification](05-ledger-and-verification.md).

- **The recall server and its long-running transport.** The recall verbs
  were once reached by starting a server and connecting to it. That surface is
  retired: the verbs live on as a command-line skill and as hook injection,
  costing the agent no decision and no extra turn, and the dispatcher behind them
  was always transport-independent. Table names and one cache directory segment
  still carry the old flavour — read-compatible history, not live vocabulary,
  since renaming them would refuse every deployed store.
  See [07 · Serving and injection](07-serving-and-injection.md) and
  [13 · Storage and schema](13-storage-and-schema.md).

- **The editor launcher.** The dashboard once had a seam for opening a file in
  your own editor — its single route to spawning a process. It was removed, "the
  dashboard spawns nothing" became exception-free and is checked by a source
  scan, and the key it used was reclaimed by the panel editor. See
  [12 · How Canopy is built](12-canopy-architecture.md).

- **The semantic code graph, and the engine designed with it.** Both were
  reversed by decision, permanently: recall is about *what an agent already knows
  and whether it is still true*, not about indexing a repository semantically,
  and the delta model's node and edge kinds are a declared mechanical set, so
  semantic kinds are unrepresentable rather than switched off. The two scaffold
  crates stay deleted and guarded. File search returned later and much narrower —
  path names and modification times behind a seam, in a differently named crate —
  which is why the search configuration key survives under a new spelling, with
  the old one accepted for one more release, while the code graph's key is
  refused outright and deleting the line is the whole fix.
  See [00 · Start here](00-start-here.md), [06 · Recall](06-recall.md) and
  [02 · Configuration](02-configuration.md).

- **Per-checkout identity.** Stores used to be keyed by a path, so every linked
  checkout became a separate, empty project. Identity is now minted once per
  family in the common directory and read thereafter; strays left by the old rule
  are reported as orphan projections, never merged, never silently deleted. See
  [09 · Worktrees and families](09-worktrees-and-families.md).

- **The earlier context-gauge band names.** They editorialized about the model's
  state on an evenly-spaced ladder that called most healthy sessions alarming, and
  were replaced by the five neutral bands under *context pressure* above, whose
  ordering is pinned by a property test. See
  [11 · A tour of Canopy](11-canopy-tour.md).

- **The trailing one-hour metrics window** and the older dashboard graph forms —
  grouped weekly activity bars, per-class token bars, a calls-ranked model list, a
  three-dimensional token prism. The window is event-anchored now, so an idle
  repository keeps its last real reading; the graph forms were superseded by the
  heat strips and treemaps in [11 · A tour of Canopy](11-canopy-tour.md).

- **The comparison leg in the benchmark harness.** Numbers were once published as
  ratios against another implementation. The slot is empty and defaults to off:
  every figure is a single-leg measurement judged against a stated bar, because a
  ratio against a binary nobody can obtain is not reproducible. See
  [15 · Benchmarks](15-benchmarks.md).

---

## Where to go next

[00 · Start here](00-start-here.md) (what Rift is, and which chapter answers
which question) · [17 · Invariants](17-invariants.md) (the standing rules several
definitions above are consequences of) · [18 · Known gaps](18-known-gaps.md)
(where a term is defined for machinery built but not yet wired).
