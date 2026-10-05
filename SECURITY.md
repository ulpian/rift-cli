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
| Local encryption keys | OS keychain | full decrypt of sensitive objects |
| The hash chain | git + projection | undetected tampering |
| The recall projection | `~/.cache/rift` | injection/poisoning into agent context |

Modeled adversaries: a curious/compromised dependency; a prompt-injection author
(content crafted to steer a *future* agent turn); local malware / a nosy
coworker; the agent itself over-reaching; a careless future commit; exfiltration
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
  **Capture fetches, pins and executes no binary of its own.** Until
  2026-08-11 it wrapped one — fetched over HTTP, pinned by SHA-256, verified
  before exec, verb-screened and output-rebranded — and every part of that is
  now *deleted*, not disabled: the wrapper crate, its pin-cut tool and its
  route table are gone, and `xtask ci-meta`'s `deleted_paths_stay_deleted`
  reds if any of those paths is tracked again.

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

  **That third bullet was missing from an earlier revision of this document,
  and the reason it was missing is worth more than the correction.** The
  end-to-end test that should have caught it seeds sessions as journal files,
  so no session was ever *unbound*, so `bind_new_sessions` returned before it
  built an identity source and the spawn never happened. The test observed
  zero and the claim was written from it. A verifier found the spawn with a
  `PATH` shim over a real capture.

  **Then the bullet's scope was wrong in the same way, one revision later.**
  It said the spawn happens inside `condense` — because `condense` was the
  call site that had been read, while the run that found the spawn made no
  commit at all. `bind_new_sessions` has two callers, and the agent-hook one
  fires per hook. A reader budgeting this cost per commit would have been
  reasoning about a different program. An enumeration is worth exactly what
  the run behind it exercised, and twice now this one was worth less than it
  claimed.

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
| Local shadow refs (`<dialect>/<commit7>-<wt6>`) | raw, pre-redaction session data — whatever wrote them put there | whoever wrote them; `rift gc` prunes transients | Rift's native capture writes none of these: the local-raw writer (`NativeGitWrite::write_shadow_raw`) exists as a seam and has **no caller in product code** (measured 2026-08-24 — ARGUED, NOT GUARDED: no test reds if a caller appears). Rift's reads are local-only — `classify_shadow` refuses any `refs/remotes/*` name — and it counts and prunes these refs rather than serving their content. Egress: the `pre-push` backstop rejects `rift/*`, which is **Rift's namespace only**; a ref another tool wrote under its own namespace is that tool's containment problem, and this hook will not stop a push of one |
| Rift's durable refs (`rift/checkpoints/v1`) | sealed bytes only | permanent | compile-enforced: durable writes accept only `RedactedBytes`, whose sole constructor is reached from one audited call site, and a test walks every workspace source root for a second mint |
| …residually, *inside* those sealed bytes | whatever the detectors missed | permanent | layered detectors + the generated JSON differential (`walk_seals_at_least_as_much_as_flat_on_generated_json`) + the seeded PII corpus + the `redact`/`redact_jsonl` fuzz targets; every known miss is an open `Gap` row in `crates/rift-shadow-redact/tests/parity.rs`, counted by `OPEN_GAPS`, not a private worry |
| The shard's **structural** blobs — all fourteen, as of 2026-10-05: `content_hash.txt`, `metadata.json`, `redaction_report.json`, `full.jsonl`, `serve_audit.jsonl`, `tool_calls.jsonl`, `usage.jsonl`, `sessions.jsonl`, `sessions/<n>/metadata.json`, `tasks/<tool-use-id>/metadata.json`, `digest.failures.json`, `digest.intent.json`, `digest.log.json`, `digest.shape.json` | ids, counts and content-addresses — the one durable door that is not typed `RedactedBytes` | permanent | `write_structural_blob` takes `&[u8]`, because these documents are the byte-parity surface and the entropy redactor would mask their own hex digests. **Eleven of the fourteen** are Rift-authored from counts, ids and content-addresses, and the four `digest.*` ones carry a containment pin on top (`a_digest_never_quotes_the_prompt_it_locates`): the artifact whose job is to summarise a prompt is tested never to quote it. `serve_audit.jsonl` freezes serve envelopes, which can carry caller text; it is built at archive time by querying the recall projection for the session's serves, and every line rebuilt from those rows passes one canonicaliser (`bins/rift/src/hooks/audit/frozen.rs`) before it is frozen: only declared members survive, and every member that can hold typed text — an exact retrieval's reason, a refusal's target unless it is a citation, any handle or tag outside Rift's own grammar — is replaced by the placeholder `withheld`, pinned by a property test that no typed run longer than a hash survives (`no_typed_text_longer_than_a_hash_survives`). Its two attribution members, `session_id` and `window_key`, are identifiers — a session id safe to name a file with, a 64-hex one-way window key — and a line whose attribution fails either grammar is refused, never withheld. `usage.jsonl` freezes each settled model call from the capture projection's `model_call` and `token_usage` rows: integers, the native session id `metadata.json` already names, and three members from provider fields and rows — the model, the counting method and the call key — each frozen only when it is a bounded identifier and otherwise replaced by `withheld`, pinned by a property test that no member outside the declared set reaches a line (`no_member_outside_the_declared_set_reaches_a_usage_line`); it carries no price. `sessions.jsonl` names each archived session's agent: the native session id `metadata.json` already names and a token from the first-party agent vocabulary (`AgentId::FIRST_PARTY`), any other token frozen as `withheld`. The fourteenth, `full.jsonl`, serialises `ArchivedEvent::payload` — a `serde_json::Value` the type system does **not** force through the gate — and is emitted only when `events` is non-empty, which no product caller makes it: every construction site outside the fixture generator is `events: Vec::new()` (measured 2026-08-24 — ARGUED, NOT GUARDED: the invariant is a doc comment, and no test reds if a caller populates `events` with un-redacted bytes). **An earlier revision named four of these and read as a complete census; a verifier counted the call sites.** A census is a claim about a count, so this one carries its date and the fourteen names rather than a summary |
| The projection / recall cache (`~/.cache/rift`) | data derived from sealed bytes | until it is cleared | never holds raw bytes; sensitive columns are AEAD-encrypted with keys in the OS keychain; the whole cache is rebuildable from canonical git |
| The family's read-time basis snapshots (`<common_dir>/rift-family/basis/<content-sha256>.bin`) | **one revision of a file in the checkout, as the agent read it** — the newest place under Rift's control where a working file's bytes are at rest | `rift clean` at `[interception] basis_ttl_days` (30 days) | compile-enforced, the same way the durable refs are: the store's writer takes only `RedactedBytes`, whose sole constructor is `rift-shadow-redact::redact`, so a snapshot that skipped the gate is not spellable. On top of the gate: a path the repository's `denied_paths` names or its `sensitive_paths` list calls sensitive is never snapshotted at all, a revision past the 1 MiB inline-hash ceiling is not, and the file is owner-only. **What it is *not*** is a copy of a transcript, a prompt, a tool result or anything an agent authored: the only bytes that reach it are a file the checkout already holds, at a revision the read record names. The sidecar `.json` beside it carries line numbers, counts, hashes and pack versions — never a byte of what was masked or of what was not |
| Reflog and objects left unreachable by a ref update | whatever the write that orphaned them put there | until `git gc` | Rift never rewrites history, so an object *Rift* wrote was sealed or structural at the moment it was written and stays that way once orphaned — the two rows above are what "sealed or structural" is worth. An object another tool wrote into the same store is outside that guarantee. `rift gc` offers only transient shadow refs for pruning, never the canonical chain |
| Redactor pack files (`.rift/policies/redactors/**`) | a `samples[].input` its author typed, which is *shaped* like a secret | permanent if committed | the loader never echoes sample text into a diagnostic and warnings carry lengths only — but a real secret pasted into a sample is at rest by the author's own choice. Packs are the one place a user can write a secret into the repo through Rift |
| Logs, errors, reports | identifiers, counts, hashes | process/log lifetime | error variants carry ids and reasons only; `RedactionReport` is structurally counts-only; pack warnings report a sample's length, never its text |
| Legacy checkpoint artifacts a `--from-legacy` import reads | whatever the foreign tool wrote, including raw code snapshots | permanent, in the repository the user pointed Rift at — Rift neither wrote them nor can delete them | **nothing of Rift's until an import runs, and the import does not weaken the gate.** The reader is read-only and returns every byte as `SecretBytes` (`rift_shadow::legacy_import::untrusted_content`), so an imported byte reaches durable storage only through the same `rift-shadow-redact::redact` call a captured byte takes. A shard another tool already redacted is, to Rift, raw |

One row above is new, and it is worth saying plainly what it changed and what
it did not. Until 2026-09-15 no bytes of a *working file* were ever at rest
under Rift's control, and the serving guide said so: no read-time bytes were
persisted to manufacture a delta basis. They are now, because a revision an
agent edited after reading exists nowhere else and every re-read of it was
running at full price. The rule that was not relaxed is the gate: the
snapshot's writer accepts only the sealed type, so the new artifact is
protected by exactly the control the durable refs are and by three narrower
ones on top. What it costs is the delta itself — a changed place computed
against a masked span is not a difference the repository made, so it is served
as a count and never as lines, and the whole answer degrades where one
appears. That is a weaker answer bought with no weaker rule, which is the
trade this table exists to make visible.

Three things this table does **not** say. It does not say the detectors do not
miss — the `Gap` rows are where the known misses are written down, and the
count is pinned so closing or opening one is a decision somebody records. It
does not say a redacted transcript replays: no key name may narrow redaction
scope, so an extended-thinking `signature` is masked like any other
high-entropy token. And where a cell says **ARGUED, NOT GUARDED**, that is the
table refusing to launder an argument into a control:
the statement was measured on the tree at the date given, nothing reddens when
it stops being true, and closing that gap is a tracked item rather than a
sentence here.

Rift's durable column is deliberately stronger than the retired reference
implementation's on three
counts — read at the pinned tag the parity suite reads, and each a design
decision rather than a tuning. That original ships a public
`AlreadyRedacted(data)` wrapper that lets any caller declare bytes clean, where
Rift has one sealed constructor and no escape hatch; it snapshots raw code onto
shadow branches, where Rift writes none (the local-shadow-refs row above); and
its optional model pass *rewrites pre-push commits*, which means those bytes
were durable before the pass ran — Rift runs every pass pre-storage and
rewrites no history, ever. The shipped product runs no such binary at all: it
reads what that tool wrote, and only that.

## Tests are first-class security artifacts

Each trust boundary ships at least one adversarial test: redaction zero-false-
negatives on a seeded corpus; AEAD tamper/AAD-mismatch fails closed; the
false-memory invariant (`mutated_source_makes_summary_unreturnable`); exact-retrieval requires
a reason and audits; column ciphertext never contains the plaintext sentinel;
the migration checksum lock refuses an edited migration. Parsers of untrusted
input (handles, redaction, config) have property tests proving they never panic.

## Scope

This policy covers the local toolchain in this repository. Rift Cloud (sync,
dashboard, forecasting) is a future, separately-secured cloud surface.
