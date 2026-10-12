# CLAUDE.md

Project guidance for Claude Code (and other agents — see [AGENTS.md](AGENTS.md)).

## What this is

**Rift** is a Rust **TUI + CLI for developers working with AI agents and coding
assistants**. Typing `rift` launches the **Canopy** dashboard. Underneath it
captures every agent session into an immutable, tamper-evident, git-native ledger
and projects a memory-delta graph that lets agents retrieve *deltas* instead of
re-reading the world.

This repo implements the local Rift toolchain. Rift Cloud
(sync, web dashboard, forecasting) is a separately built surface, out of scope here;
every local decision is made *sync-ready*, so Cloud can be additive rather than
a rewrite.

The written guide to this toolchain is **tracked** at `docs/guide/` — start at
[`docs/guide/00-start-here.md`](docs/guide/00-start-here.md), which maps the
chapters. It explains what each plane does and, more importantly, why each
standing decision was made, so prefer it over re-deriving intent from the code.
The rest of `docs/` — a glossary, setup notes and research notes — stays local
and untracked: consult those files when they are present in your working tree,
but never rely on them existing in a fresh clone, and use the glossary's terms
with the glossary's meanings.

The three product planes and their storage substrate:

| Package | Role |
|---|---|
| **rift-shadow** | Immutable, git-native system of record for AI sessions (capture, redaction, attribution, cost, hash chain). Native Rust end to end: it runs no other binary. |
| **rift-total-recall** | Memory-delta validity plane projected over rift-shadow data; agents retrieve deltas via hook injection + CLI skills, not full re-reads. |
| **rift-canopy** | The terminal dashboard — the `rift` entrypoint (3-column grid, nic-gauge, slash palette, project-settings modal) behind a `RenderBackend` seam. |
| **rift-agent\*** | Native project integrations for Claude Code, Codex, Gemini CLI, Antigravity, Cursor, Copilot CLI, OpenCode, Factory Droid, and Ollama. |
| **rift-nic-gauge** | Read-only context-% data for Canopy; not a project setting. |
| **Turso** | SQLite-compatible async store for the rebuildable projection (behind the `CanonicalStore` seam; a `rusqlite` reference impl ships). |

## Two hard constraints (carried into every change)

1. **Immutable by construction.** Canonical tables are append-only — no `UPDATE`,
   no `DELETE`. Corrections are new, superseding rows; tamper-evidence is hash
   chains + Merkle roots. Enforced in three layers: the type layer (no mutating
   API), the SQL layer (`BEFORE UPDATE/DELETE … RAISE(ABORT)` triggers on every
   canonical table), and a registry-driven property test in `rift-store`. **Add a
   canonical table without its guard and CI goes red.**
2. **Security is a per-line property.** Secrets never reach durable storage
   un-redacted (the `RedactedBytes` type makes that a compile error); untrusted
   tool output is never served raw; exact retrieval is policy-gated + audited.
   See [SECURITY.md](SECURITY.md).

## Prerequisites

- Rust toolchain pinned in `rust-toolchain.toml` (**1.96.0**); MSRV is **1.94**
  (`Cargo.toml` `rust-version`, `clippy.toml`).
- `rusqlite` is built with the `bundled` feature, so a **C compiler** is required
  (it compiles the SQLite amalgamation).

## Commands

Prefer the task runner; it is the single source of truth for common workflows:

```bash
./skills.sh check       # fmt --check + clippy -D warnings + test  (pre-push gate)
./skills.sh test        # cargo test --workspace
./skills.sh lint        # cargo clippy --workspace --all-targets --all-features -D warnings
./skills.sh fmt         # cargo fmt --all
./skills.sh build       # cargo build --workspace
./skills.sh run -- status   # run the rift binary with args
```

Raw cargo equivalents work too. CI declares fifteen required jobs, the release
build among them: single-leg `fmt` and `clippy -D warnings`; `test` across
macOS/Linux/Windows (fail-fast off); an `msrv` build; `coverage` (≥85% line +
≥75% branch workspace-wide, ≥95% line for the crates listed in
`ci/security-crates.txt`, enforced by `xtask check-coverage`); `security`
(`cargo deny` + `cargo audit`); advisory `miri`; a required 60s-per-target
`fuzz-smoke` over `fuzz/`; `red-team`, which replays the adversarial serving
corpora; `dialect-conformance` (the legacy-import golden suites, held to a
pinned suite list); `docs` (rustdoc at a zero-warning baseline,
`RUSTDOCFLAGS=-D warnings`, plus `xtask rustdoc-baseline` — the shrink-only
site list `ci/rustdoc-baseline.txt`, both arms); `bench-harness`, the only run
the nested benchmark workspace gets, since the three-OS `test` job never
reaches it; and `xtask ci-meta`, the guard on the guards (the required-job
list itself, read-only workflow permissions, the `ci/lint-baseline.txt`
lint-table lock, no `unsafe`, no crate-root blanket allows, no schema
fast-path in bins, deleted paths staying deleted,
and no network crate in the shipped binaries' resolved graph); and
`gate-intercept`, the read-only guard on the read-interception surface (one
file composes an agent's hook response, the channels answered on match
`ci/intercept-channels.txt` and every pinned output artifact, nothing a hook
decides with opens a store or loads the tokenizer, one module writes the
sentences a model reads, an amended tool call names only `file_path` or
`head_limit`, and every cache is keyed by the repository family). A scheduled
`fuzz-nightly.yml` runs 10-minute budgets daily.

## Workspace layout

```
crates/
  rift-core         shared: RiftError, ids (Sha256Id/Blake3Id/Ulid + semantic),
                    identity, Timestamp, RedactedBytes/SecretBytes, Immutable
  rift-config       policy-as-code, safe TOML edits, redactor packs, glob matcher
  rift-crypto       ChaCha20-Poly1305 AEAD, HKDF envelope keys, zeroized secret types
  rift-hashchain    SHA-256 leaves, Merkle roots, chain + inclusion proofs
  rift-store        append-only store (rusqlite ref impl), migrations, integrity
                    chain, bitemporal cost facts, column encryption (Turso = target)
  rift-shadow*      capture: native checkpoint archive, redaction gate, hooks/events +
                    state machine, attribution math, four-class cost model
  rift-recall-*     recall plane: types (handles, four-state envelope), validity
                    (hash-bound summaries), ledger metrics, ranking, pack compiler,
                    policy, events, bounded hook/skill serving, substrate reader
                    (gix seam)
  rift-canopy       dashboard: layout, Project Settings, command palette, splash,
                    nic-gauge, render seam
  rift-canopy-data  the dashboard's data plane: bus + worker, watermark
                    poller, policy/native-adapter projections, status/detail builders,
                    feeds
  rift-git          native git plumbing: worktree discovery, blob-read seam,
                    argv-safe shell-outs, the production IdentitySource
  rift-registry     the machine-global project registry (config-dir resolver)
  rift-agent*       the agent SDK + nine native per-agent adapters
  rift-nic-gauge    read-only context-window utilization data for Canopy
  rift-tokens       the one token-counting authority
bins/
  rift              the `rift` entrypoint (Canopy + subcommands)
  rift-memd         rift-total-recall projection binary (metrics | rebuild |
                    status | doctor | eval)
```

Dependency direction is acyclic: everything depends on `rift-core`;
`rift-store` → `rift-hashchain` + `rift-crypto`; `rift-shadow` → store + the
shadow sub-crates; recall crates layer over `rift-recall-types`/`rift-store`
(the recall ledger additionally backs validity and ranking durably, so
neither plane keeps a store of its own); `rift-canopy` → `rift-core` +
`rift-registry` (config-dir resolver only). The committed snapshot
`xtask/src/snapshots/xtask__tests__crate_graph_adjacency.snap` is the
reviewable adjacency list.

## Architecture conventions (don't break these)

- **Seams over hard deps for pre-1.0 / heavy externals.** Turso → `CanonicalStore`
  (rusqlite reference impl); `gix` →
  `SubstrateReader`; `xre-rs` →
  `RenderBackend` (the ASCII fallback is always available; the first-party
  terminal backend plugs in behind the `xre` feature). The *logic* is
  implemented and tested behind the seam; the external engine plugs in without
  touching callers. The `ServeDispatcher` is transport-independent and feeds the
  two serving channels, hook injection and CLI skills.
- **The redaction gate is one-way and total.** Raw bytes are `SecretBytes`
  (local-only, zeroized, no `Debug`). Durable writes accept only `RedactedBytes`,
  whose buffer is private and whose sole constructor (`RedactedBytes::seal`) is
  called in exactly one place: `rift-shadow-redact::redact`. Never add a
  `From<Vec<u8>>` or a second constructor.
- **Hashing split:** SHA-256 (`sha2`) wherever a hash must match something
  outside Rift or something Rift already wrote — the chain and `content_hash.txt`
  in `rift-hashchain`, and worktree content hashing in `rift-recall-substrate`,
  which compares against the plain `content_sha256` capture recorded at read
  time. blake3 is the fast internal digest: salted local identities, config and
  policy fingerprints, integration-ownership fingerprints, and the frozen ids of
  served envelopes. They are distinct id types and never interchange.
- **Errors:** libraries use `thiserror` via `rift_core::RiftError`; bins use
  `anyhow` at the edge. Error variants carry identifiers/reasons only — never key
  or transcript bytes.
- **Config format:** `.rift/config.toml` (TOML, not YAML — the dossier pins
  `toml`; `serde_yaml` is unmaintained). `deny_unknown_fields` everywhere; secure
  defaults when absent; built-in `sensitive_paths` are unioned in and non-droppable.
- **Project settings lifecycle:** repository policy files are authoritative.
  Canopy and the CLI call the same native init/Recall/integration services. Patch
  TOML with the shared conflict-checked, symlink-refusing, same-directory atomic
  editor; never overwrite or delete an integration artifact Rift does not own.
  The Canopy surface opens from the Repository card, `s`, or `/settings`; it
  contains Recall, Capture, Security/Policy, and every adapter in registry order.
  Adding a project in Canopy **is** `rift init` for a repository not set up on
  this machine: the first status read emits the `Init` act, which runs the
  detected default plan (`ProjectSetupPlan::from_view`, the page's own
  reading) through the manager's progress-reporting method and shows every
  step in the `running` dialog; removing a project only hides the row.
  `features.total_recall` is on unless written `false`.
  Agent presence is **one predicate** over four evidence classes — a `PATH`
  command (bare, `.exe` or `.cmd`), the vendor's install location, a verified
  extension manifest under a VS Code-compatible root, a JetBrains plugin —
  read from an injected `DetectionEnvironment` in `rift-agent-install::detect`
  and spelled by one sentence (`present via …`) on every surface; `init`'s
  auto-wire, Canopy's pre-tick and doctor must never grow a scanner of their
  own. Codex's `.codex/hooks.json` groups are a separate, independently
  fingerprinted fact; preserve unrelated groups and refuse conflicts/symlinks.
- **"Initialized" is per machine.** A repository is set up where its family's
  projection exists under the machine's cache root **and** its hook install is
  on record (the family's `binary.json` naming a binary that is still there).
  The hook gates ask only for the projection (`family::provisioned_here` in the
  bin): a running hook is its own proof. The dashboard and Project Settings ask
  for both, because an agent hook creates the projection on first contact.
  `.rift/` is committed configuration that travels with a clone and proves
  nothing; never reintroduce it as the test. A config-only clone is `NotProvisioned`: the
  wizard stages machine-local steps only and never writes the tracked policy.
- **Worktrees are family members.** A linked git worktree is a shadow member
  of its repository family: one minted family identity in the git common dir
  keys the projection, the recall store, the binary of record and the registry
  row — never a per-worktree path. Every session, transcript
  and checkpoint carries worktree provenance (`session_worktree`,
  `checkpoint_worktree`); recall serves members through branch containment, so
  savings compound across members — including subagents in temporary
  worktrees. Adoption is explicit (`rift worktree adopt`, or the
  `post-checkout` hook); nothing re-mints a family identity automatically.
  Minting stays in one chokepoint so a second call site cannot fork a family,
  and every durable state root is keyed by family rather than by checkout path —
  key new state the same way. See
  [`docs/guide/09-worktrees-and-families.md`](docs/guide/09-worktrees-and-families.md)
  for the walkthrough and
  [`docs/guide/02-configuration.md`](docs/guide/02-configuration.md) for the
  state-roots table.
- **Determinism in tests:** inject the clock/RNG/identity (`UlidSource`,
  `CryptoRng` via explicit nonces, `IdentitySource`). No test reads the network,
  and no assertion depends on the wall clock: a test may read it only to bound a
  wait or to seed a fixture, never to decide its verdict.
- **Modal-surface parity (Canopy).** Anything drawn as a centered modal frame
  — an `Overlay` *or* a mode stage over the shared modal geometry —
  carries the shared close X (`modal_close_rect`, top-right; a mode's X closes
  the mode, never reuse `Target::OverlayClose`), and every act its hints teach
  is also reachable by click on that surface (and every clickable by key).
  Hints derive per stage so they never advertise a dead key. One solver feeds
  draw + hit-test; a new modal surface joins the parity fixtures or CI goes red.

## Style

Code follows the rust-analyzer style guide
(<https://rust-analyzer.github.io/book/contributing/style.html>). What
reviewers enforce:

- **Ceilings, enforced by `xtask ci-meta`:** no `.rs` file over 1,500 lines
  and no function over 80 lines, tests, benches and `tests/` files included.
  The screen parses item spans, so a comment or a string cannot shrink a
  function. Split a file by concern before it nears the line; name a helper
  before a function nears its.
- **Control flow:** guard clauses and early returns first, the happy path
  unindented; `let … else` for a precondition whose failure leaves the
  function; `match` over `if let … else`.
- **Functions:** small, named after the step they perform; many parameters
  become one context struct passed first; a `bool` every caller passes as a
  literal becomes two functions.
- **Visibility:** private by default, `pub(super)` inside a module tree,
  `pub(crate)` for a sibling, `pub` only for a crate's real API; re-exports
  only where a split keeps callers unchanged.
- **Errors:** no `unwrap`, `expect`, `panic!` or indexing outside test code;
  variants carry identifiers and reasons, never key or transcript bytes.
- **Order within a file:** module docs, `mod` and `use`, types, constructors,
  methods, free functions, then `#[cfg(test)] mod tests` last at column zero.
- **Comments say why:** the decision, the invariant, the alternative rejected;
  a doc comment never restates the signature. An `#[ignore]` carries its
  reason.
- **Tests** are held to every rule above, inject the clock, RNG and identity,
  and are named as a sentence about the behaviour.

## Lints & tests

- Workspace `#![forbid(unsafe_code)]`; CI denies warnings (`clippy -D warnings`,
  pedantic on; `unwrap`/`expect`/`panic`/`indexing_slicing` flagged). Crate-level
  `#![cfg_attr(test, allow(clippy::unwrap_used, …))]` permits them in tests only.
- Security-critical crates (`rift-shadow-redact`, `rift-crypto`, `rift-hashchain`,
  `rift-recall-policy`, `rift-recall-validity`, `rift-store`) target ≥95% coverage;
  others ≥85%.
- Every crate carries property tests for its invariants (immutability,
  tamper-evidence, validity-never-stale, budget ceilings, scope containment).

## Gotchas

- Commit `Cargo.lock` (this workspace produces binaries).
- `rustfmt.toml` sets `imports_granularity`/`group_imports`, which are
  nightly-only; on stable they warn-and-skip (`fmt --check` still passes).
- The false-memory guard lives in `rift-recall-validity`: a stale summary has a
  different key and `ValidSummary` is unconstructible outside the crate — keep it
  that way; `envelope_for` is the single way to build a served artifact.
- `rift-canopy` pulls in no `xre-rs` layers by default; the `xre` feature turns
  on the standard terminal backend, and the `rift` binary enables it by default
  through its own `tui` feature. The 3D tier is a separate opt-in: the crate's
  `xre-3d` feature, reached from the binary as `tui-hires`.
