# Security Policy

Security is a **per-line property** in Rift, not a release-time phase. This
document summarizes the threat model, the controls baked into the code, and how
to report a vulnerability.

## Reporting a vulnerability

Please report security issues **privately** — do not open a public issue for an
unfixed vulnerability.

- Email: **ulpianmorina@gmail.com** with subject `SECURITY: rift`.
- Include affected version/commit, reproduction, and impact.
- We aim to acknowledge within 72 hours and to ship a fix or mitigation before
  any public disclosure. Coordinated disclosure is appreciated.

## Assets & adversaries

| Asset | Where it lives | Worst case |
|---|---|---|
| Raw prompts/transcripts | the agent's own files; local shadow refs, where some tool wrote them | secret/PII/IP exfiltration |
| Redacted checkpoints | `rift/checkpoints/v1` (durable) | residual sensitive-data leak |
| Token/cost data | the projection | competitive/financial signal leak |
| Local identity salt | OS keychain | identity linkage |
| Local encryption keys (one column key per repository family) | `~/.config/rift/keys/<recall-id>.key` (`0600`, in a `0700` directory; the user-profile ACL on Windows) — the config root, never the cache root | decrypt of the recall projection's summary and outline bodies and of the read gate's outline cache; `rift doctor` reports the file missing, malformed, unreadable or open to other users, and never mints it |
| The hash chain | git + projection | undetected tampering |
| The recall projection | `~/.cache/rift` | injection/poisoning into agent context |

Modeled adversaries: a curious/compromised dependency; a prompt-injection author
(content crafted to steer a later agent turn); local malware / a nosy
coworker; the agent itself over-reaching; a careless later commit; exfiltration
via the record.

## Controls built into the code

- **Redaction is a one-way, total gate (compile-time enforced).** Raw bytes are
  `rift_core::SecretBytes` (local-only, `zeroize`d, no `Debug`/`Display`).
  Durable-write APIs accept only `RedactedBytes`, whose buffer is private and
  whose only constructor (`RedactedBytes::seal`) is reached from a single audited
  call site in `rift-shadow-redact::redact`. Raw bytes therefore *cannot* be
  persisted — the compiler rejects it. The two-pass pipeline (vendor-secret /
  credential / JDBC / PEM detectors + entropy + PII) is in `rift-shadow-redact`;
  its report carries counts/ids/hashes only — never matched text.
- **Immutability is enforced in three layers** (type / SQL triggers / property
  test). A change to canonical data is a new superseding row, never a mutation.
- **Tamper-evidence:** SHA-256 Merkle roots per checkpoint, chained via
  `prev_hash`; `rift verify` re-derives roots from canonical git and cross-checks
  the projection, so tampering with only the cache is detected.
- **Secret hygiene:** keys are `rift_crypto::DataKey` — `Zeroize`d on drop, no
  `Debug`/`Display`/`Serialize`. AEAD failures fail closed and never embed bytes
  in the error.
- **Recall serve gate (policy-first):** the default response is a manifest, not
  content; exact retrieval requires a `reason`, passes policy, and is audited
  (`memd.exact_retrieved`). Untrusted tool output is never served as raw exact
  text. Per-turn budgets bound over-retrieval. Queries are scope-contained
  (current repo/branch/developer, last-N checkpoints) by default.
- **Honest cache semantics:** "file unchanged" never means "the model knows
  the file". The four-state envelope (`known_unchanged` / `summary_included` /
  `exact_included` / `available_by_handle`) is a contract; `known_unchanged`
  requires *both* same-hash-seen and a validity check that passes. Stale summaries
  are *structurally unreturnable* (`rift-recall-validity`).
- **Local-only identity:** a developer id is a salted hash of the normalized git
  email; the machine id is a salted hash; raw fingerprints are never persisted.
  No cloud account, no network in the identity path.
- **Supply chain:** `#![forbid(unsafe_code)]` workspace-wide; CI runs
  `cargo deny` (licenses/advisories/source allowlist/wildcard ban) and
  `cargo audit`. `Cargo.lock` is committed; the toolchain is pinned.
  **Capture fetches, pins and executes no binary of its own.** There is no
  wrapper crate, pin tool or route table, and `xtask ci-meta`'s
  `deleted_paths_stay_deleted` reds if any of those paths is tracked.

  That is the claim, and it is narrower than "capture runs nothing". What a
  capture actually starts, enumerated rather than asserted — every one an
  argv array, never a shell:

  - **`git`**, throughout: the plumbing this whole record is built on.
  - **the `rift` binary itself**, post-commit: the detached capture worker.
  - **`ioreg -rd1 -c IOPlatformExpertDevice`** on macOS, **`reg query
    HKLM\SOFTWARE\Microsoft\Cryptography /v MachineGuid`** on Windows — to
    read the machine fingerprint that binds a session to a developer, once
    per batch of unbound sessions, **wherever one first appears**: the
    agent-hook ingest binds as soon as a hook names a session, and `condense`
    binds again for whatever is still unbound. Each binding builds its own
    identity source, so each spawns once for its own batch. **This is a
    per-hook cost, not a per-commit one.** Linux reads a file and starts
    nothing.

  The fingerprint deserves the second look, because it is a machine
  identifier read during capture. It is read **at most once per identity
  source** — a `OnceLock<Option<Zeroizing<String>>>`, which is also why three
  call paths through `snapshot`/`binding_basis` produce one spawn and not
  three — it is salted-hashed by `derive_machine`, and only that hash is
  persisted. The raw UUID/GUID reaches no durable store, no error and no log,
  and is wiped when the source drops at the end of the binding.

  An enumeration is worth exactly what the run behind it exercised: the
  spawn above happens in the agent-hook binding as well as in `condense`, so a
  reader budgeting it per commit would be reasoning about a different program.

  Outside capture, integration *detection* runs an already-installed agent
  CLI's `--version` (argv, `stdin` closed, time-bounded) to report what a repo
  has — Rift never installs, downloads or updates that binary.

## Where under-redacted bytes could persist

Redaction is a gate, not a guarantee that no detector ever misses. So this is
the honest inventory: every place a byte the gate should have caught could
still be found, who controls that place, and what stands between it and an
adversary. A control that is not in this table does not exist.

| Place | What can be there | Who controls its lifetime | What stands in the way |
|---|---|---|---|
| Process memory during capture | raw prompts, tool output, secrets | the process | `SecretBytes`: zeroized on drop, no `Debug`/`Display`/`Serialize`, never handed to a durable API |
| The agent's own transcript files (`~/.claude/projects/**.jsonl` and the Codex/Gemini equivalents) | everything, unredacted | **the agent, not Rift** | Rift only reads them; it never copies them anywhere un-sealed. These bytes exist whether or not Rift is installed |
| Local shadow refs (`<dialect>/<commit7>-<wt6>`) | raw, pre-redaction session data — whatever wrote them put there | whoever wrote them; `rift gc` prunes transients | Rift's native capture writes none of these: the local-raw writer (`NativeGitWrite::write_shadow_raw`) exists as a seam and has **no caller in product code** (ARGUED, NOT GUARDED: no test reds if a caller appears). Rift's reads are local-only — `classify_shadow` refuses any `refs/remotes/*` name — and it counts and prunes these refs rather than serving their content. Egress: the `pre-push` backstop rejects `rift/*`, which is **Rift's namespace only**; a ref another tool wrote under its own namespace is that tool's containment problem, and this hook will not stop a push of one |
| Rift's durable refs (`rift/checkpoints/v1`) | sealed bytes only | permanent | compile-enforced: durable writes accept only `RedactedBytes`, whose sole constructor is reached from one audited call site, and a test walks every workspace source root for a second mint |
| …residually, *inside* those sealed bytes | whatever the detectors missed | permanent | layered detectors + the generated JSON differential (`walk_seals_at_least_as_much_as_flat_on_generated_json`) + the seeded PII corpus + the `redact`/`redact_jsonl`/`redact_fallback` fuzz targets; every known miss is an open `Gap` row in `crates/rift-shadow-redact/tests/parity.rs`, counted by `OPEN_GAPS`, not a private worry |
| The shard's **structural** blobs — all seventeen: `content_hash.txt`, `metadata.json`, `redaction_report.json`, `full.jsonl`, `serve_audit.jsonl`, `tool_calls.jsonl`, `tool_results.jsonl`, `usage.jsonl`, `sessions.jsonl`, `window_journal.jsonl`, `session_worktrees.jsonl`, `sessions/<n>/metadata.json`, `tasks/<tool-use-id>/metadata.json`, `digest.failures.json`, `digest.intent.json`, `digest.log.json`, `digest.shape.json` | ids, counts and content-addresses — the one durable door that is not typed `RedactedBytes` | permanent | `write_structural_blob` takes `&[u8]`, because these documents are the byte-parity surface and the entropy redactor would mask their own hex digests. **Eleven of the seventeen** are Rift-authored from counts, ids and content-addresses, and the four `digest.*` ones carry a containment pin on top (`a_digest_never_quotes_the_prompt_it_locates`): the artifact whose job is to summarise a prompt is tested never to quote it. `serve_audit.jsonl` freezes serve envelopes, which can carry caller text; it is built at archive time by querying the recall projection for the session's serves, and every line rebuilt from those rows passes one canonicaliser (`bins/rift/src/hooks/audit/frozen.rs`) before it is frozen: only declared members survive, and every member that can hold typed text — an exact retrieval's reason, a refusal's target unless it is a citation, any handle or tag outside Rift's own grammar — is replaced by the placeholder `withheld`, pinned by a property test that no typed run longer than a hash survives (`no_typed_text_longer_than_a_hash_survives`). Its two attribution members, `session_id` and `window_key`, are identifiers — a session id safe to name a file with, a 64-hex one-way window key — and a line whose attribution fails either grammar is refused, never withheld. `usage.jsonl` freezes each settled model call from the capture projection's `model_call` and `token_usage` rows: integers, the native session id `metadata.json` already names, and three members from provider fields and rows — the model, the counting method and the call key — each frozen only when it is a bounded identifier and otherwise replaced by `withheld`, pinned by a property test that no member outside the declared set reaches a line (`no_member_outside_the_declared_set_reaches_a_usage_line`); it carries no price. `sessions.jsonl` names each archived session's agent: the native session id `metadata.json` already names and a token from the first-party agent vocabulary (`AgentId::FIRST_PARTY`), any other token frozen as `withheld`. `window_journal.jsonl` freezes the rows of the session's context-window journals, which sit in an agent-writable directory: each row passes the journal's own parser and then one writer (`window_journal_jsonl_bytes`) that admits only a 64-hex window key and revisions, a `/`-spelled repository-relative path, integers, a flag and closed kind and eviction-reason tokens, refusing a row whole otherwise and withholding a subagent id that is not an identifier, pinned by a property test that every member of every line has a declared shape (`every_window_line_member_is_one_of_the_declared_shapes`). `session_worktrees.jsonl` freezes every observation of the family member a session was seen in, read from the capture projection's two observation relations: a 12-hex worktree token, a member name that is an identifier with no `/`, a branch under the archiving label's own screen, an identifier subagent id, a basis an agent hook observes under (never a verb's own `commit`, `attach` or `import`), a flag and a time; one writer (`session_worktrees_jsonl_bytes`) refuses an observation whole otherwise, and neither the checkout's absolute path, a local address, nor the head is ever a member, pinned by a property test (`no_line_carries_a_path_a_head_or_an_undeclared_member`). `tool_results.jsonl` freezes each measured tool result from the capture projection's `tool_result_tokens` rows, the search half of the savings denominator: a 64-hex result id, the native session id `metadata.json` already names, integers, an instant and the closed channel and basis vocabularies, refused whole otherwise; the tool's name, which an agent's payload supplies, is frozen only when it is a bounded identifier and the tokenizer only when it is one of the two Rift records, each `withheld` otherwise, and nothing the tool returned is ever a member, pinned by a property test (`no_line_carries_an_undeclared_member_or_typed_text`). The seventeenth, `full.jsonl`, serialises `ArchivedEvent::payload` — a `serde_json::Value` the type system does **not** force through the gate — and is emitted only when `events` is non-empty, which no product caller makes it: every construction site outside the fixture generator is `events: Vec::new()` (ARGUED, NOT GUARDED: the invariant is a doc comment, and no test reds if a caller populates `events` with un-redacted bytes). A census is a claim about a count, so this one names all seventeen rather than summarising |
| The projection / recall cache (`~/.cache/rift`) | data derived from sealed bytes | until it is cleared | never holds raw bytes; protected by owner-only permissions (`0700` directories, `0600` files). The registered sensitive column — `digest_blob.bytes`, the bound summary and outline bodies — is **AEAD-encrypted** (ChaCha20-Poly1305, bound to its table/column/row) under the family's column key, which lives under the *config* root (`~/.config/rift/keys/`), not here. A copy, backup or sync of the cache without the config directory is therefore unreadable for those bodies. It does **not** protect against anything running as the user, which can read both directories; nor are the projection's other columns encrypted (paths, hashes, counts, costs rely on the permissions above). A missing or unreadable key is a miss, never a plaintext write: a lost key file is minted afresh and rows sealed under the old one stop serving until a rebuild; rows written plain-framed by an earlier build still read, and `rift doctor` counts them. They cannot be sealed in place — `digest_blob` is append-only and addressed by the plaintext digest — so `rift-memd rebuild` is what removes them: it sets the projection aside and binds every summary afresh, sealed; the set-aside file keeps the old plaintext until it is deleted. The whole cache is rebuildable from canonical git |
| The family's outline cache (`<common_dir>/rift-family/outline/<content-sha256>-e…p…r….outline`) | one bound outline per revision: definition names or headings, line spans, token counts | rebuilt by the next bind; `rift clean` sweeps it by age | **AEAD-sealed** under the same family column key, bound to the file's own name, so a copy or backup of `.git` without the config directory carries no names, and a file moved under another revision's name does not open. The read gate opens it with one key-file read per hook process, paid only when a file is found — no store, no tokenizer. A file that does not open (a lost key, or the plaintext one an earlier build wrote) is a miss, never served, and the next bind replaces it; a key that cannot seal writes nothing rather than plaintext |
| The family's read-time basis snapshots (`<common_dir>/rift-family/basis/<content-sha256>.bin`) | **one revision of a file in the checkout, as the agent read it** — the newest place under Rift's control where a working file's bytes are at rest | `rift clean` at `[interception] basis_ttl_days` (30 days) | compile-enforced, the same way the durable refs are: the store's writer takes only `RedactedBytes`, whose sole constructor is `rift-shadow-redact::redact`, so a snapshot that skipped the gate is not spellable. On top of the gate: a path the repository's `denied_paths` names or its `sensitive_paths` list calls sensitive is never snapshotted at all, a revision past the 1 MiB inline-hash ceiling is not, and the file is owner-only. **What it is *not*** is a copy of a transcript, a prompt, a tool result or anything an agent authored: the only bytes that reach it are a file the checkout already holds, at a revision the read record names. The sidecar `.json` beside it carries line numbers, counts, hashes and pack versions — never a byte of what was masked or of what was not |
| The family's spool of rewritten results' originals (`<common_dir>/rift-family/spool/<session>/<tool-use-id>.original`) | **one Bash result's stdout and stderr as the tool produced them, before a hook delivered a rewrite in their place** — redacted, at most 30,000 characters | the archive reads every record at every checkpoint of the session and never consumes one; removal at the session's final archive is the session lifecycle's to do; until then `rift doctor` names a spool whose session has no live state | compile-enforced, as for the basis snapshots: the record's one writer (`SpoolHeader::record`) takes the body only as `RedactedBytes`, so raw tool output is not spellable as a record (ruled: raw `SecretBytes` on disk is refused). The file is owner-only (`0o600`) in owner-only directories (`0o700`) under the git common dir, never committed; it is published by a hard link that refuses an existing name, and no link on its path is followed. Its one reader is the archive, which passes every body through the gate again before it is sealed as `tool-originals/<id>.json`; nothing serves it. The header carries ids, a closed form word, line ranges and a digest — never a byte of the output |
| Reflog and objects left unreachable by a ref update | whatever the write that orphaned them put there | until `git gc` | Rift never rewrites history, so an object *Rift* wrote was sealed or structural at the moment it was written and stays that way once orphaned — the two rows above are what "sealed or structural" is worth. An object another tool wrote into the same store is outside that guarantee. `rift gc` offers only transient shadow refs for pruning, never the canonical chain |
| Redactor pack files (`.rift/policies/redactors/**`) | a `samples[].input` its author typed, which is *shaped* like a secret | permanent if committed | the loader never echoes sample text into a diagnostic and warnings carry lengths only — but a real secret pasted into a sample is at rest by the author's own choice. Packs are the one place a user can write a secret into the repo through Rift |
| Logs, errors, reports | identifiers, counts, hashes | process/log lifetime | error variants carry ids and reasons only; `RedactionReport` is structurally counts-only; pack warnings report a sample's length, never its text |
| Legacy checkpoint artifacts a `--from-legacy` import reads | whatever the foreign tool wrote, including raw code snapshots | permanent, in the repository the user pointed Rift at — Rift neither wrote them nor can delete them | **nothing of Rift's until an import runs, and the import does not weaken the gate.** The reader is read-only and returns every byte as `SecretBytes` (`rift_shadow::legacy_import::untrusted_content`), so an imported byte reaches durable storage only through the same `rift-shadow-redact::redact` call a captured byte takes. A shard another tool already redacted is, to Rift, raw |

The read-time basis snapshot row deserves a plain statement of what it changed
and what it did not. A revision an agent edited after reading exists nowhere
else, and every re-read of it would run at full price, so working-file bytes are
at rest under Rift's control. The rule is not relaxed: the snapshot's writer
accepts only the sealed type, so the artifact is protected by exactly the
control the durable refs are and by three narrower ones on top. What it costs
is the delta itself — a changed place computed against a masked span is not a
difference the repository made, so it is served as a count and never as lines,
and the whole answer degrades where one appears. That is a weaker answer bought
with no weaker rule, which is the trade this table exists to make visible.

Three things this table does **not** say. It does not say the detectors do not
miss — the `Gap` rows are where the known misses are written down, and the
count is pinned so closing or opening one is a deliberate change. It
does not say a redacted transcript replays: no key name may narrow redaction
scope, so an extended-thinking `signature` is masked like any other
high-entropy token. And where a cell says **ARGUED, NOT GUARDED**, that is the
table refusing to launder an argument into a control:
the statement was measured on the tree, and nothing reddens when it stops being
true.

Rift's durable column is deliberately strict on three counts, each a design
decision rather than a tuning. There is one sealed constructor and no public
wrapper that lets any caller declare bytes clean; no raw code is written onto
shadow branches (the local-shadow-refs row above); and every redaction pass
runs pre-storage, so no history is ever rewritten after bytes were durable.

## Tests are first-class security artifacts

Each trust boundary ships at least one adversarial test: redaction zero-false-
negatives on a seeded corpus; AEAD tamper/AAD-mismatch fails closed; the
false-memory invariant (`mutated_source_makes_summary_unreturnable`); exact-retrieval requires
a reason and audits; column ciphertext never contains the plaintext sentinel;
the migration checksum lock refuses an edited migration. Parsers of untrusted
input (handles, redaction, config) have property tests proving they never panic.

## Scope

This policy covers the local toolchain in this repository. Rift Cloud (sync,
dashboard, forecasting) is a separately secured cloud surface.
