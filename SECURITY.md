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
| Raw prompts/transcripts | shadow branches (local, raw) | secret/PII/IP exfiltration |
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
- **Honest cache semantics (W2):** "file unchanged" never means "the model knows
  the file". The four-state envelope (`known_unchanged` / `summary_included` /
  `exact_included` / `available_by_handle`) is a contract; `known_unchanged`
  requires *both* same-hash-seen and a validity check that passes. Stale summaries
  are *structurally unreturnable* (`rift-recall-validity`).
- **Local-only identity:** a developer id is a salted hash of the normalized git
  email; the machine id is a salted hash; raw fingerprints are never persisted.
  No cloud account, no network in the identity path.
- **Supply chain:** `#![forbid(unsafe_code)]` workspace-wide; CI runs
  `cargo deny` (licenses/advisories/source allowlist/wildcard ban) and
  `cargo audit`. `Cargo.lock` is committed; the toolchain is pinned. The wrapped
  capture binary is pinned by SHA-256 and verified before execution; cloud/auth
  verbs are refused before spawn and its output is rebranded (fails closed).

## Tests are first-class security artifacts

Each trust boundary ships at least one adversarial test: redaction zero-false-
negatives on a seeded corpus; AEAD tamper/AAD-mismatch fails closed; the W2
invariant (`mutated_source_makes_summary_unreturnable`); exact-retrieval requires
a reason and audits; column ciphertext never contains the plaintext sentinel;
the migration checksum lock refuses an edited migration. Parsers of untrusted
input (handles, redaction, config) have property tests proving they never panic.

## Scope

This policy covers the local toolchain in this repository. Rift Cloud (sync,
dashboard, forecasting) is a future, separately-secured surface (Stage 7).
