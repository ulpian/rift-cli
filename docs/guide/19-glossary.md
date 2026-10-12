# 19. Glossary

Every term this guide leans on, defined once, with the chapter that owns it.
Read it as an index: take the definition, follow the arrow to the chapter that
unpacks it. A code name in backticks is the type, constant, command or file that
embodies the term, so you can grep for the real thing.

---

## A

- **Accounting seam** — the single function every serve response leaves through: serialize once, count the real bytes with the real tokenizer, perform the one budget charge, seal the audit event from those same two figures. → [07](07-serving-and-injection.md)
- **Adoption** — provisioning a linked checkout for the agents its repository family has already enabled, using the family's recorded binary. `rift worktree adopt`. → [09](09-worktrees-and-families.md)
- **Agent hook** — a hook installed into a *coding assistant's* project configuration, invoked as `rift hooks <agent> <verb>` with one JSON object on standard input. → [03](03-capture.md)
- **Amendment** — a change Rift makes to an agent's own tool call at the pre-tool boundary: only by adding or substituting a field the client's published schema documents for that purpose, only in a closed and pinned set of forms, never on a decline and never any other field. → [07](07-serving-and-injection.md)
- **Append-only** — the standing rule that canonical rows are never updated and never deleted; a correction is a new row superseding an old one. → [13](13-storage-and-schema.md)
- **Artifact bridge** — the authoritative reconciler: it walks the redacted checkpoints in git, refuses any without a valid redaction report, re-derives canonical events from each transcript and projects them through the live path's writers. `rift rebuild`. → [03](03-capture.md)

---

## B

- **Bar** — a declared performance limit a published measurement is judged against: a floor for a throughput row, a ceiling for a latency row. `benchmarks/floors.toml`. → [15](15-benchmarks.md)
- **Binary of record** — the absolute path to the `rift` executable a repository family's git hooks point at, recorded as a fact with a capped history. `binary.json`, `rift hooks install --repoint`. → [09](09-worktrees-and-families.md)
- **Bitemporal fact** — a row carrying two time axes: when the fact was true in the world, and when this machine came to believe it. `valid_from` / `valid_to` / `recorded_at`. → [13](13-storage-and-schema.md)
- **Bundle** — the offline transfer unit: one self-contained file carrying a manifest, a chain segment and the git objects, framed so every claim is checkable before a single object moves. `rift bundle export|import`. → [14](14-sync-readiness.md)

---

## C

- **Canonical event** — the agent-independent record of one lifecycle fact: session start, turn end, tool call, file edit, and eight more. → [03](03-capture.md)
- **Canopy** — the terminal dashboard you get by typing `rift` with no arguments: three columns over one repository at a time, plus the context gauge, the command surface and the read-only viewer. → [11](11-canopy-tour.md)
- **Chain ref** — the git reference whose history *is* the order of the checkpoints: one empty-tree commit per checkpoint, each parented on the previous, the checkpoint's identifier riding a commit trailer. `refs/rift/chain/v1`, `rift chain status`. → [05](05-ledger-and-verification.md)
- **Checkpoint** — what a session's work condenses into: a directory of small redacted files committed onto a reference of Rift's own, with a Merkle root and a chain entry behind it. → [05](05-ledger-and-verification.md)
- **Checkpoint strategy** — the per-repository setting deciding *when* a live session condenses: on your commits (the default), on a push, on an interval, or on policy signals alone. `checkpoint_strategy`. → [03](03-capture.md), configured in [02 · Configuration](02-configuration.md)
- **Compounding** — recall work earned by one checkout accruing to every other checkout of the same repository: read facts, digests, hunk indexes and packs are computed once per content and served family-wide. → [09](09-worktrees-and-families.md)
- **Condense** — the operation turning a live session's raw capture into a durable checkpoint: redact first, write the shard, append to the chain. → [03](03-capture.md)
- **Containment** — the rule bounding what a recall query may see: a checkpoint reaches a checkout only when its **anchor commit** — the commit the checkpoint is about — is reachable from that checkout's current position. → [09](09-worktrees-and-families.md)
- **Context pack** — one ordered, budgeted evidence section compiled for a turn, whose placement the agent does not choose. `rift recall context-pack`. → [07](07-serving-and-injection.md)
- **Context pressure** — how full the model's context window is, as a percentage and one of five neutral bands: Clear below 50%, Moderate below 70%, High below 85%, Very High below 95%, Critical at 95 and above. `rift-nic-gauge`. → [11](11-canopy-tour.md)
- **Coverage** — the merged set of line intervals of one file *revision* that a context window has actually been handed, recorded conservatively from pinned tool behaviour and scoped to the content hash, so a changed file starts with none. → [06](06-recall.md)
- **Custody manifest** — the fingerprint record proving which integration files Rift wrote, so a re-run tells "mine, refresh it" from "somebody else's, leave it alone". `.rift/state/native-integrations.json`. → [10](10-agent-integrations.md)

---

## D

- **Decline** — the pre-tool answer that cancels one read and gives the model a reason, on a channel that client documents. → [07](07-serving-and-injection.md)
- **Delta frame** — the decline for a file the window holds at an *older* hash, naming the changed sites in current numbering, the net line shift and a window to read instead. → [07](07-serving-and-injection.md)
- **Delta manifest** — the short per-turn text Rift pushes into an agent's context naming the files it read that have changed since: a pointer at a delta, not the delta. → [07](07-serving-and-injection.md)
- **Detached worker** — the one-shot child process a hook spawns and does not wait for, which drains the durable job journal: token counts, then condenses, then queued commits. `RIFT_CAPTURE_SYNC=1` runs it inline. → [03](03-capture.md)

---

## E

- **Eviction boundary** — the point at which a context window is emptied — a compaction, a clear, a resume — after which earlier coverage stops supporting a decline. → [06](06-recall.md)
- **Exact retrieval** — the one path by which stored bytes actually leave: it needs a handle or a pack ordinal, a stated reason, a policy pass, and it writes an audit row before anything returns. `rift recall retrieve-exact`. → [07](07-serving-and-injection.md)
- **Extractor** — a versioned, purely mechanical digest producer — failure signatures, log shape, diff counts, file shape. → [06](06-recall.md)

---

## F

- **False-positive census** — the measured cost of the redaction pipeline firing on ordinary repository text, held per detector by a ceiling a test reads back out of the published artifact. → [04](04-redaction-and-secrets.md)
- **Family** — one repository plus every linked checkout of it: the set sharing a git *common directory*, and therefore sharing objects, references and hooks. → [09](09-worktrees-and-families.md)
- **Family manifest** — the small machine-local file in the common directory holding a family's minted identity, never tracked, written once and rewritten only by an explicit repair. `family.json`, `family.history.jsonl`. → [09](09-worktrees-and-families.md)
- **Four-state envelope** — the closed set of states every served artifact is in: unchanged relative to what you named, a validity-checked summary inline, exact bytes inline, or nothing inline plus a handle and a reason. `known_unchanged`, `summary_included`, `exact_included`, `available_by_handle`. → [06](06-recall.md)

---

## G

- **Grant** — an explicit, reasoned widening of what one serve may see, across branches. `--cross-branch <REASON>`. → [07](07-serving-and-injection.md)

---

## H

- **Handle** — an unforgeable reference to content held outside the prompt: a file revision addressed by path *and* content hash, an immutable captured tool result, or a hash-bound summary key. → [06](06-recall.md)
- **Held set** — the `(path, hash, coverage)` triples a context window holds. → [06](06-recall.md)
- **Honesty figure** — `avoided − returned` over a class's own audit rows: the standing test that a lever's frames, maps and sidecars cost less than the reads they displaced. → [08](08-cost-and-savings.md)

---

## I

- **Integration health** — whether each wired agent's capture is actually arriving, as findings with a closed set of reason codes: `codex-hooks-skipped`, `lifecycle-hooks-missing`, `hook-binary-missing` and `session-behind-events` are errors. `health::Reason`. → [10](10-agent-integrations.md)

---

## K

- **Known-state anchor** — the manifest call opening a turn, which resets the turn's draw-down while keeping its ceilings. "What changed?" already precedes any retrieval, so anchoring imposes no new protocol, and a caller that never anchors runs under one un-reset budget rather than an unbounded one. `rift recall known-state`. → [06](06-recall.md)

---

## M

- **Manifest** — in the recall plane, a list-shaped answer naming what changed and what you already hold without carrying the content: the default response is a manifest, never bytes. The word is also used for the custody manifest and a bundle's manifest, which are unrelated. → [06](06-recall.md)
- **Member** — one checkout in a repository family, either the main work tree or a linked one. → [09](09-worktrees-and-families.md)
- **Merkle root** — the single hash committing to all of a checkpoint's content files, built from per-file leaves in the shard's bytewise path order. → [05](05-ledger-and-verification.md)
- **Modal-surface parity** — the contract binding anything drawn as a centred frame over the shared modal geometry: it carries the shared close control on every stage it can show, every act its hints teach is reachable by click on that surface, every clickable has a key path, and hints derive per stage so a dead key is never advertised. → [12](12-canopy-architecture.md)

---

## P

- **Path index** — the per-member file listing every path this checkout may answer a name search with, and the modification time each had, keyed by the commit HEAD points at and the newest modification time the walk saw. `.rift/state/search/index.bin`. → [06](06-recall.md)
- **Pinned channel** — a `(client, verb)` pair Rift answers on because a committed fixture carries that client's published response schema for it: repository, tag, path, blob hash and a verbatim excerpt. → [10](10-agent-integrations.md)
- **Policy hash** — a digest of a repository's *effective* configuration content, referenced by captured records so "which policy was in force when this was recorded" always has an answer. → [02](02-configuration.md)
- **Profile** — a named view of the dashboard: its own project membership over the machine-global catalogue, its own per-project arrangements, and its own colours. → [11](11-canopy-tour.md)
- **Projection** — a local database derived from the git record: fast to query, destroyable, rebuildable, and never the authority in a comparison. → [13](13-storage-and-schema.md)
- **Push queue** — the family-wide directory of references waiting to be shipped, written at the chain append and drained by an explicit flush or a push to the configured remote. → [14](14-sync-readiness.md)

---

## R

- **Read record** — the append-only file per context window under `.rift/state/reads/`, holding the ranges that window was handed, the declines issued, the hashes its writes were asked to produce, and its eviction boundaries. → [06](06-recall.md)
- **Reason frame** — the sealed text of a decline: fixed phrases authored in code, variable spans through the same allowlist a manifest entry passes, no file content, no placeholder brackets, no error word. → [07](07-serving-and-injection.md)
- **Recovery map** — the boundary-only injection after a context is emptied: the pre-eviction held set as path, hash and ranges, each labelled unchanged or changed at named lines, ranked by recency and fitted to its own budget. → [07](07-serving-and-injection.md)
- **Redaction gate** — the one-way, total boundary between raw and durable bytes. `SecretBytes`, `RedactedBytes`, `RedactedBytes::seal`. → [04](04-redaction-and-secrets.md)
- **Redaction report** — the counts-only artifact frozen into every checkpoint: totals per detector and per class, bytes masked, which rule packs ran, the address of the raw input. `redaction_report.json`. → [04](04-redaction-and-secrets.md)
- **Render seam** — the single trait every cell the dashboard draws passes through, with two implementations: a plain-text floor always compiled in, and a richer one behind an opt-in build feature. `RenderBackend`. → [12](12-canopy-architecture.md)

---

## S

- **Savings share** — `avoided ÷ (avoided + read)` since the last window boundary, the numerator from the audit relation and the denominator from captured read facts, computed once for every surface that prints it. `rift savings`. → [08](08-cost-and-savings.md)
- **Scope** — the bound on what one recall query may read: repository, branch ancestry, checkout, developer, and a checkpoint window. → [06](06-recall.md)
- **Session** — the unit capture is organised around, driven by a pure state machine: given a state and a trigger it yields the next state and a list of *requested* effects, and the caller performs or journals them. `rift session list|info|show`. → [03](03-capture.md)
- **Shard** — a checkpoint on disk: the directory of small files under a path derived from the checkpoint's identifier — the redacted capture document, the redaction report, the transcripts, the tool calls, the four mechanical digests, the content address. → [05](05-ledger-and-verification.md)
- **Sidecar** — the Markdown conversion of a document, keyed by the document's bytes together with the converter's version and the caps that shape its output, written once per repository family and viewed per member as a hardlink. → [07](07-serving-and-injection.md)
- **Strict shell grammar** — the narrow shape a shell command must match to count as a read: one simple command, one work-tree path, no pipes, redirects, globs, variables or chaining, and only the spellings a client's own read-before-edit list uses. → [07](07-serving-and-injection.md)

---

## T

- **Token classes** — the four separately-billed kinds of token recorded per model call and priced independently: fresh input, input served from the provider's prompt cache, tokens written into that cache, and output. `uncached_input`, `cached_input`, `cache_creation`, `output`. → [08](08-cost-and-savings.md)
- **Tokens returned / tokens avoided** — the two figures every serve reports. → [08](08-cost-and-savings.md)
- **Trust class** — the class every payload carries, derived from where the bytes came from in the ledger and never from the caller: code fact, git fact, test output, untrusted tool output, untrusted transcript. `Trust`. → [07](07-serving-and-injection.md)
- **Turn budget** — the two ceilings a turn's retrieval runs under: four tool calls and two thousand returned tokens, held by the caller and reset by the anchoring manifest call. `TurnBudget`. → [07](07-serving-and-injection.md)

---

## V

- **Validity binding** — a summary keyed by a hash over the bytes it describes plus the extractor, policy and redaction versions. → [06](06-recall.md)

---

## W

- **Watermark** — the dashboard's change detector, read as a pair: an append cursor moving whenever capture writes, and a rebuild epoch moving only when a projection is rebuilt. → [12](12-canopy-architecture.md)

---

## Where to go next

[00 · Start here](00-start-here.md) (what Rift is, and which chapter answers which
question) · [17 · Invariants](17-invariants.md) (the standing rules several
definitions above are consequences of) · [18 · What Rift does not do](18-what-rift-does-not-do.md)
(the permanent limits).
