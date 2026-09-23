# The Rift recall serving surface

`rift-total-recall` exposes a **bounded delta-retrieval** surface through
native hook injection and the `rift recall <verb>` CLI skills. The old stdio
transport is retired; `rift-memd` now manages and evaluates
the projection, while agents call a **closed set of nine skill verbs**. There
is no transcript search or open-ended session search.

The point of the surface is that an agent retrieves **deltas** — what changed
since it last looked — instead of re-reading the world. Every response is
budgeted, policy-gated, and audited.

| | |
|---|---|
| Entry points | `rift recall <verb>` skills plus native hook injection |
| Projection utility | `rift-memd` (`metrics`, `rebuild`, `status`, `doctor`, `eval`) |
| Capabilities | Nine closed retrieval verbs; no transcript/session search |
| Instructions | *"Start with `rift recall known-state`. Retrieval is budgeted and audited; exact retrieval requires a reason."* |
| Scope | One repository per invocation; exact reads use the read-only git substrate |
| Implementation | Transport-independent dispatcher in [`src/lib.rs`](src/lib.rs), called by [`bins/rift/src/recall/serve.rs`](../../bins/rift/src/recall/serve.rs) |

---

## Contents

- [Invocation & wiring](#invocation--wiring)
- [Request lifecycle](#request-lifecycle)
- [Common input schema](#common-input-schema)
- [Budgets](#budgets)
- [Handles](#handles)
- [Tools](#tools)
  - [`rift_known_state`](#rift_known_state)
  - [`rift_diff_since`](#rift_diff_since)
  - [`rift_file_if_changed`](#rift_file_if_changed)
  - [`rift_changed_hunks`](#rift_changed_hunks)
  - [`rift_read_summary`](#rift_read_summary)
  - [`rift_retrieve_exact`](#rift_retrieve_exact)
  - [`rift_cost_hotspots`](#rift_cost_hotspots)
  - [`rift_context_pack`](#rift_context_pack)
  - [`rift_doc_read`](#rift_doc_read)
- [Errors](#errors)
- [Audit events](#audit-events)
- [Security model](#security-model)

---

## Invocation & wiring

Build the CLI and projection utility, initialize native integrations once, and
inspect the live agent-facing command map:

```bash
cargo build -p rift -p rift-memd
target/debug/rift recall init
target/debug/rift agent-help
target/debug/rift recall known-state --task "inspect Recall" --file README.md --json
target/debug/rift-memd status
```

`rift recall init` installs only Rift-owned native hook/skill entries and
preserves unrelated agent configuration. `rift recall disable` removes only
fingerprint-matching Rift entries and retains Recall data. Each skill invocation
refreshes the projection before dispatch, then records a best-effort neutral
serve audit event. `RIFT_LOG` writes structured diagnostics to stderr only.

## Request lifecycle

Every skill dispatch passes through the same middleware before a handler runs:

1. **Anchor** — a `known-state` call resets the caller-held per-turn budget,
   so a turn's calls share one allowance.
2. **Call ceiling** — deny with `BudgetCallsExhausted` before any work.
3. **Query-aware params** — ranking-sensitive tools require non-empty `task`
   *and* `files_in_focus`, else `QueryParamsRequired`.
4. **Handler** — reads the recall projection (`recall.db`) and, for exact
   retrieval only, the read-only git substrate.
5. **Accounting seam** — the payload is serialized once, counted with the real
   tokenizer, charged against the turn budget in exactly one place, and the
   `memd.*` audit event is sealed from those same figures.
6. **Audit handoff** — exactly one `memd.*` event per call is piped to
   `rift hooks memd <verb>` (non-blocking, best effort).

With `--json`, the CLI emits one machine-readable document containing the
**payload JSON** described per verb below. The token figures (`estimated_tokens_returned` /
`estimated_tokens_avoided` / `tokenizer`) are *not* in the payload — they travel
in the audit event, so the agent's context is never charged for its own
accounting.

## Common input schema

All nine tools advertise the same property set; each tool reads only the fields
it needs, and ranking-sensitive tools add `"required": ["task","files_in_focus"]`.

```json
{
  "type": "object",
  "properties": {
    "task":             { "type": "string" },
    "files_in_focus":   { "type": "array", "items": { "type": "string" } },
    "error_signatures": { "type": "array", "items": { "type": "string" } },
    "token_budget":     { "type": "integer" },
    "known_hashes":     { "type": "array", "items": { "type": "string" } },
    "target":           { "type": "string" },
    "reason":           { "type": "string" },
    "top_n":            { "type": "integer" }
  }
}
```

| Field | Type | Meaning |
|---|---|---|
| `task` | string | The goal being worked on. Drives ranking. Required on ranking-sensitive tools. |
| `files_in_focus` | string[] | Repo-relative paths currently in focus. Drives ranking. Required on ranking-sensitive tools. |
| `error_signatures` | string[] | Error strings currently in play; drives the error-overlap ranking signal. |
| `token_budget` | integer | Requested response budget. `0`/absent ⇒ tool default; clamped to the tool max and to the remaining turn budget. |
| `known_hashes` | string[] | Content SHA-256 hashes the agent already holds. Anything matching comes back as `known_unchanged` with **zero** content. |
| `target` | string | A [handle](#handles) or repo-relative path, for the per-artifact tools. |
| `reason` | string | Mandatory, non-empty justification for `rift_retrieve_exact`. Recorded verbatim in the audit event. |
| `top_n` | integer | Result count for `rift_cost_hotspots` (default `5`). |

**Decoding is total.** Unknown fields are ignored; missing or ill-typed fields
fall back to defaults; oversized integers saturate. Only an unknown *tool name*
is a dispatcher error (`invalid params: unknown tool`).

## Budgets

| Constant | Value | Meaning |
|---|---|---|
| `DEFAULT_MAX_TOOL_CALLS` | `4` | Tool calls per turn |
| `DEFAULT_MAX_TOKENS_PER_TURN` | `2000` | Returned tokens per turn |
| `DEFAULT_MANIFEST_TOKENS` | `1200` | Manifest cap when `token_budget` is absent |
| `MAX_MANIFEST_TOKENS` | `4000` | Ceiling an explicit manifest `token_budget` clamps to |
| `DEFAULT_PACK_TOKENS` | `1200` | Pack cap when `token_budget` is absent |
| `MAX_PACK_TOKENS` | `4000` | Ceiling an explicit pack `token_budget` clamps to |
| `MAX_CHANGED_HUNKS` | `8` | Hunks per `rift_changed_hunks` response |

Effective cap for a call = `min(requested-or-default, tool max, turn remaining)`.

The budget is supplied by the caller and threaded through a dispatch sequence;
`known-state` anchors (zeroes) its draw-down. The idiomatic turn begins with
`rift recall known-state`, followed by only the retrievals the manifest names.

Truncation is always **honest**: list responses carry `truncated` and `omitted`
counts, and entries are dropped from the tail of a deliberately ordered list
(rank order for the manifest; unchanged-before-changed for the delta list).

## Handles

A handle is content-addressed — it resolves to an immutable git object or the
digest store, never a path on disk.

```
file:<repo-relative-path>@sha256:<64-hex>[:L<lo>-L<hi>]
log:<tool_use_id>
summary:<key>
```

Trust is derived from the handle's **provenance**, never from the caller:

| Handle | Trust class | Exact retrieval |
|---|---|---|
| `file:` | `CodeFact` | allowed (policy + reason) |
| `summary:` | `CodeFact` | allowed (policy + reason) |
| `log:` | `ToolOutputUntrusted` | always refused; neutral digest only |
| unparseable | fails closed to untrusted | always refused |

Paths inside handles are validated: non-empty, relative, no `..` segment, no
ASCII control characters (a newline in a path could otherwise forge a directive
line in a rendered manifest or pack).

---

## Tools

### `rift_known_state`

> Manifest of what changed vs what the agent already knows (ranked, four-state).

**The entry point.** Anchors the turn budget. Returns a ranked manifest of
artifacts as four-state envelopes — state tags and hashes, no file bodies.

- **Ranking-sensitive:** yes — `task` + `files_in_focus` required.
- **Reads:** `task`, `files_in_focus`, `error_signatures`, `known_hashes`, `token_budget`.
- **Cap:** `min(token_budget || 1200, 4000, turn remaining)`.
- **Event:** `memd.manifest_served`.

Envelope states:

| `state` | Meaning |
|---|---|
| `known_unchanged` | The agent's hash matches the current revision — costs zero content tokens. |
| `summary_included` | A valid hash-bound summary was inlined. |
| `exact_included` | Exact content was inlined (packs only; never a sensitive path). |
| `available_by_handle` | Retrievable, not inlined — fetch with `rift_retrieve_exact` if needed. |

**Request**

```json
{
  "name": "rift_known_state",
  "arguments": {
    "task": "fix the budget clamp in the recall dispatcher",
    "files_in_focus": ["crates/rift-recall-serve/src/lib.rs"],
    "error_signatures": ["PackBudgetTooSmall"],
    "known_hashes": ["bbbb...bbbb"],
    "token_budget": 1500
  }
}
```

**Response payload**

```json
{
  "tool": "known_state",
  "envelopes": [
    { "path": "crates/rift-recall-serve/src/lib.rs", "state": "available_by_handle", "basis_sha256": "aaaa…aaaa" },
    { "path": "crates/rift-recall-policy/src/lib.rs", "state": "known_unchanged", "basis_sha256": "bbbb…bbbb" }
  ],
  "truncated": false,
  "omitted": 0
}
```

---

### `rift_diff_since`

> Changed and unchanged files since a known point.

Partitions the scoped candidate set against `known_hashes`. Cheap, unranked,
manifest-only.

- **Ranking-sensitive:** no.
- **Reads:** `known_hashes`, `token_budget`.
- **Cap:** the manifest cap (`1200` / `4000` / turn remaining). Under pressure,
  `unchanged` entries are dropped before `changed` entries.
- **Event:** `memd.manifest_served`.

**Request**

```json
{
  "name": "rift_diff_since",
  "arguments": { "known_hashes": ["bbbb…bbbb", "cccc…cccc"] }
}
```

**Response payload**

```json
{
  "tool": "diff_since",
  "changed": [
    { "path": "crates/rift-recall-serve/src/lib.rs", "new_sha256": "aaaa…aaaa" }
  ],
  "unchanged": ["crates/rift-recall-policy/src/lib.rs"],
  "truncated": false,
  "omitted": 0
}
```

---

### `rift_file_if_changed`

> Conditional check: is a `(path, known_hash)` still current?

The cheapest call in the surface — one path, one verdict, no content.

- **Ranking-sensitive:** no.
- **Reads:** `target` (repo-relative path), `known_hashes`.
- **Event:** `memd.manifest_served`.

| `state` | Condition |
|---|---|
| `unchanged` | The current revision's hash is in `known_hashes`; echoes `known_hash`. |
| `changed` | Current revision differs; returns `new_hash`. |
| `invalid` | The path is not in scope; `reason: "path_not_found"`. |

**Request**

```json
{
  "name": "rift_file_if_changed",
  "arguments": {
    "target": "crates/rift-recall-serve/src/lib.rs",
    "known_hashes": ["aaaa…aaaa"]
  }
}
```

**Response payload**

```json
{
  "tool": "file_if_changed",
  "result": { "state": "unchanged", "path": "crates/rift-recall-serve/src/lib.rs", "known_hash": "aaaa…aaaa" }
}
```

---

### `rift_changed_hunks`

> The actual changed hunks, capped and token-budgeted.

Returns the persisted changed line-ranges for the **top-ranked** path in play —
the documented fallback when a context pack will not fit.

- **Ranking-sensitive:** yes — `task` + `files_in_focus` required.
- **Reads:** `task`, `files_in_focus`, `error_signatures`.
- **Caps:** at most `MAX_CHANGED_HUNKS` (8) hunks, and each hunk must fit the
  remaining turn allowance; the rest are counted in `omitted`.
- **Event:** `memd.manifest_served`.
- **Empty world:** no ranked candidate ⇒ `{"tool":"changed_hunks","hunks":[],"omitted":0}`.

**Request**

```json
{
  "name": "rift_changed_hunks",
  "arguments": {
    "task": "review the accounting seam",
    "files_in_focus": ["crates/rift-recall-serve/src/lib.rs"]
  }
}
```

**Response payload**

```json
{
  "tool": "changed_hunks",
  "path": "crates/rift-recall-serve/src/lib.rs",
  "hunks": [ { "lo": 1040, "hi": 1059 }, { "lo": 1098, "hi": 1121 } ],
  "omitted": 0,
  "source_hash": "aaaa…aaaa"
}
```

`lo`/`hi` are new-side line bounds (`hi = new_start + new_len`). Fetch text with
`rift_retrieve_exact` or `rift_context_pack`.

---

### `rift_read_summary`

> A hash-bound, validity-checked summary (stale source is never served).

Summaries are bound to the SHA-256 of the source they describe. On serve, the
binding is re-checked against the current revision: if the source moved, you get
`stale` — never the stale bytes. This is the false-memory guard.

- **Ranking-sensitive:** no.
- **Reads:** `target` (repo-relative path).
- **Events:** `memd.summary_served` (valid) · `memd.invalidated` (stale) ·
  `memd.manifest_served` (unavailable).

| `state` | Meaning | Fields |
|---|---|---|
| `summary` | Binding is current | `path`, `source_hash`, `summary` |
| `stale` | Source changed since the summary was bound | `path`, `current_hash` |
| `unavailable` | No binding exists | `path`, `reason: "not_bound"` |

**Request**

```json
{ "name": "rift_read_summary", "arguments": { "target": "crates/rift-recall-serve/src/lib.rs" } }
```

**Response payload**

```json
{
  "tool": "read_summary",
  "result": { "state": "stale", "path": "crates/rift-recall-serve/src/lib.rs", "current_hash": "aaaa…aaaa" }
}
```

---

### `rift_retrieve_exact`

> Exact bytes — reason required, policy-gated, always audited.

The only tool that returns raw content on demand, and the only one with a
mandatory `reason`.

- **Ranking-sensitive:** no. **Exact:** yes.
- **Reads:** `target` (a [handle](#handles)), `reason`.
- **Events:** `memd.exact_retrieved` (granted) · `memd.refused` (denied or downgraded).

Decision order (first match wins):

| # | Condition | Outcome |
|---|---|---|
| 0 | Serve policy unparseable (fail-closed) | `refused` / `policy_denied_exact` |
| 1 | `target` path matches a denied glob | `refused` / `policy_denied_exact` — not even a handle |
| 2 | Provenance is untrusted (`log:` or unparseable) | `refused` with a **neutral digest** (counts + handle, never payload text) |
| 3 | Transcript trust without an explicit allow-glob | `refused` / `policy_denied_exact` |
| 4 | `reason` missing or empty, sensitive path | `refused` / `sensitive_path_requires_reason` |
| 5 | `reason` missing or empty, ordinary path | `refused` / `policy_denied_exact` |
| 6 | Allowed, but a **sensitive** path whose bytes exceed the remaining budget | `available_by_handle` / `sensitive_over_budget` — whole or nothing, never a partial secret |
| 7 | Allowed and it fits | `exact` with `body` |

**Request**

```json
{
  "name": "rift_retrieve_exact",
  "arguments": {
    "target": "file:crates/rift-recall-serve/src/lib.rs@sha256:aaaa…aaaa",
    "reason": "need the respond() seam verbatim to verify the single-charge invariant"
  }
}
```

**Response payload — granted**

```json
{
  "tool": "retrieve_exact",
  "result": { "state": "exact", "path": "crates/rift-recall-serve/src/lib.rs", "body": "fn respond(…) { … }" }
}
```

**Response payload — refused (trusted path, no reason)**

```json
{ "tool": "retrieve_exact", "result": { "state": "refused", "reason": "policy_denied_exact" } }
```

**Response payload — refused (untrusted provenance)**

```json
{
  "tool": "retrieve_exact",
  "result": { "state": "refused", "neutral_digest": { "kind": "tool_output", "bytes": 0, "lines": 0, "reference": "log:…" } }
}
```

Retrieved bytes are **self-validating**: the fetched blob must re-hash to the
handle's `sha256` before it is served, so a stale blob-oid map can never yield
mismatched content.

---

### `rift_cost_hotspots`

> Awareness-only: areas with the most repeated read tokens.

Descriptive, never directive — the `note` comes from a closed vocabulary, so the
tool cannot emit an instruction to the agent.

- **Ranking-sensitive:** no.
- **Reads:** `top_n` (default `5`).
- **Event:** `memd.manifest_served`.

**Request**

```json
{ "name": "rift_cost_hotspots", "arguments": { "top_n": 3 } }
```

**Response payload**

```json
{
  "tool": "cost_hotspots",
  "hotspots": [
    { "area": "crates/rift-recall-serve/src/lib.rs", "est_tokens": 12000, "share": 0.41 },
    { "area": "crates/rift-store/src/lib.rs", "est_tokens": 8000, "share": 0.27 },
    { "area": "crates/rift-canopy/src/layout.rs", "est_tokens": 3000, "share": 0.10 }
  ],
  "note": "Informational only: these areas account for the most repeated read tokens."
}
```

`share` is the fraction of scoped estimated tokens (`0.0` when the scope is empty).

---

### `rift_context_pack`

> An ordered, budgeted, position-aware evidence pack.

The heavyweight tool: ranks candidates, compiles a section-ordered pack under a
token cap, and returns rendered lines plus a content-independent `pack_key`.

- **Ranking-sensitive:** yes — `task` + `files_in_focus` required.
- **Reads:** `task`, `files_in_focus`, `error_signatures`, `known_hashes`, `token_budget`.
- **Cap:** `min(token_budget || 1200, 4000, turn remaining)`; considers the top 8 ranked candidates.
- **Event:** `memd.pack_built`.
- **Fails loud:** if the effective cap cannot hold a coherent pack, the call errors
  with `PackBudgetTooSmall { needed, cap }` — the documented fallback is
  `rift_changed_hunks`.

Section order is fixed and position-aware:

| `kind` | Contents |
|---|---|
| `CriticalTop` | Critical facts at the head (primacy) |
| `Changed` | Changed files / inlined hunk text |
| `Unchanged` | Unchanged-but-relevant summaries and handles — **never** exact content |
| `Recommended` | Handles worth retrieving |
| `CostWarning` | The fixed awareness note |
| `CriticalTail` | Critical facts repeated at the tail (recency) |

Exclusions applied before anything is rendered: denied paths, unsafe paths,
untrusted provenance (entered as a neutral digest only), and sensitive paths
(handle or audited exact retrieval — never pack-inlined, never lossily
summarized).

**Request**

```json
{
  "name": "rift_context_pack",
  "arguments": {
    "task": "fix the reverted change in src/a.rs",
    "files_in_focus": ["src/a.rs"],
    "error_signatures": ["assertion failed: fits(kept)"],
    "token_budget": 1000
  }
}
```

**Response payload**

```json
{
  "tool": "context_pack",
  "sections": [
    { "kind": "CriticalTop", "lines": ["CRITICAL: src/a.rs's last provenance was reverted - verify before reuse"] },
    { "kind": "Changed",     "lines": ["src/a.rs changed @sha256:aaaa…aaaa:", "L2-L2:\nb\n"] },
    { "kind": "Unchanged",   "lines": ["src/b.rs available: file:src/b.rs@sha256:bbbb…bbbb (source_deleted_or_redacted)"] },
    { "kind": "Recommended", "lines": ["retrieve: file:src/a.rs@sha256:aaaa…aaaa"] },
    { "kind": "CostWarning", "lines": ["Informational only: these areas account for the most repeated read tokens."] },
    { "kind": "CriticalTail","lines": ["CRITICAL: src/a.rs's last provenance was reverted - verify before reuse"] }
  ],
  "total_tokens": 148,
  "pack_key": "…"
}
```

`pack_key` is **input-keyed**: derived from the query, the ranked
`(candidate_id, content_hash)` list, the serve-policy version, and the token
budget — computed before rendering, never from the output bytes. Identical
inputs ⇒ identical key. Every section is composed fresh; a caching layer
would have to disclose per-section provenance beside the key.

---

### `rift_doc_read`

> A document as Markdown: the sidecar Rift holds for a `.docx`, `.pdf`,
> spreadsheet or slide deck, instead of the file's own bytes.

The **odd verb out**, and deliberately. Its answer is neither in the projection
nor in the object database: it is a file in the family's sidecar store
(`<common_dir>/rift-family/doc/<key>.md`, seen through this member's
`.rift/state/doc/<key>.md`), which this crate cannot reach. The binary resolves
it — hashes the document, keys it, links the member's view — and hands it in on
the request. Nothing is converted on the asking path: a document the family has
not converted yet is answered `pending`, the work is journaled for the detached
worker, and the next call is answered from the store.

- **Ranking-sensitive:** no. **Scope-bound:** no — a sidecar is keyed by the
  document's own bytes and is the same file for every member of the family, so
  there is no branch axis to widen.
- **Reads:** `target` (the document's path), plus the resolved document input.
- **Event:** `memd.doc_served`, or `memd.doc_deferred` when the family holds
  nothing yet.

**Request**

```json
{ "name": "rift_doc_read", "arguments": { "target": "docs/plan.docx" } }
```

**Response payload**

```json
{
  "tool": "doc_read",
  "path": "docs/plan.docx",
  "state": "exact_included",
  "basis_sha256": "9f2c…",
  "sidecar": ".rift/state/doc/4b1e…md",
  "content": "<!-- rift: converted from docs/plan.docx@sha256:9f2c… -->\n# Quarterly plan\n…",
  "reason": "declared_baseline"
}
```

`state` is the sealed constructor's, against a ceiling that is the document's
own **declared** native cost: Markdown dearer than the container it stands in
for is not inlined, and the answer names `sidecar` so the caller reads it
itself. `refused` (a denied or sensitive path) and `pending`
(`sidecar_owed` / `sidecar_declined`) carry a null `content` and a null
`sidecar`.

`estimated_tokens_avoided` is the declared baseline net of what was returned,
and `reason: "declared_baseline"` is what says so: the read it prices never
happened, so there is nothing to measure. A PDF declares nothing — its pages
are rendered as images, and no pinned artifact gives a vision-token rate — so
its sidecar is served and no saving is claimed for it.

---

## Errors

Handler failures surface as typed dispatcher refusals and non-zero CLI exits,
not successful payloads.

| Variant | Cause | Recovery |
|---|---|---|
| `BudgetCallsExhausted` | Per-turn call ceiling (default 4) hit | Re-anchor with `rift recall known-state` |
| `QueryParamsRequired` | A ranking-sensitive tool called without non-empty `task` **and** `files_in_focus` | Supply both |
| `Denied(OmitReason)` | Policy refusal | See the `rift_retrieve_exact` decision table |
| `PackBudgetTooSmall { needed, cap }` | The effective pack cap cannot hold a coherent pack | Raise `token_budget`, re-anchor the turn, or call `rift_changed_hunks` |
| `Internal(String)` | Store/substrate read failure | Identifier and reason only — never key or transcript bytes |
| `DecodeError::UnknownTool` | Tool name outside the nine | Protocol error: `invalid params: unknown tool` |

Neutral omit tags used in refusal payloads: `budget_exhausted`,
`policy_denied_exact`, `sensitive_path_requires_reason`,
`summary_invalid_source_changed`, `source_deleted_or_redacted`, `not_requested`.

## Audit events

Exactly **one** terminal `memd.*` event per call — granted or refused, no
exceptions — piped to `rift hooks memd <verb>`, which redacts and hash-chains it.
This serve↔event bijection is what makes recall savings auditable.

`memd.manifest_injected` is the one event with no call behind it: the push tier
puts a manifest in the agent's context unasked, so it is one event per *turn*
that injects, and it is pure spend. It carries no `estimated_tokens_avoided`
field at all — not a zero — so an injection cannot claim credit, and injected
spend can be netted against served credit by verb instead of by inference.

| Event | Emitted by | Fields |
|---|---|---|
| `memd.manifest_served` | `known_state`, `diff_since`, `file_if_changed`, `changed_hunks`, `cost_hotspots`, `read_summary` (unavailable) | `estimated_tokens_returned`, `estimated_tokens_avoided` |
| `memd.manifest_injected` | no verb — the push tier (the session pointer and the per-turn manifest), via `injection_audit_envelope` | `estimated_tokens_returned` |
| `memd.summary_served` | `read_summary` (valid) | `handle`, `estimated_tokens_returned`, `estimated_tokens_avoided` |
| `memd.invalidated` | `read_summary` (stale) | `handle`, `estimated_tokens_returned` |
| `memd.exact_retrieved` | `retrieve_exact` (granted) | `handle`, `reason`, `estimated_tokens_returned` |
| `memd.refused` | `retrieve_exact` (denied or downgraded) | `target`, `reason` (neutral tag), `estimated_tokens_returned` |
| `memd.pack_built` | `context_pack` | `sections`, `total_tokens`, `estimated_tokens_returned`, `estimated_tokens_avoided` |

`estimated_tokens_returned` is the real tokenizer count of the serialized payload
the transport ships — measured, not asserted. `estimated_tokens_avoided` is
**net**: the gross ingest-time baseline the response stands in for, minus what was
returned (saturating at zero). Refusals and invalidations are measured and
charged like any other serve.

## Security model

- **Trust is provenance-derived, never caller-asserted.** Untrusted tool output
  is never inlined raw — it enters as a neutral digest (counts + handle). The
  single inline path for exact bytes is a type-level chokepoint.
- **Policy shapes exposure before content is touched.** A denied path is never
  served, not even as a handle; a sensitive path is served whole under an audited
  reason or by handle — never partially, never as a lossy summary.
- **Validity is hash-bound.** A summary whose source moved is unconstructible as
  a served artifact; you get `stale`, never stale bytes.
- **Anomalous paths are dropped before rendering** (absolute, `..`, or control
  characters), so a path can never forge a directive line in a manifest or pack.
- **Cost notes are a closed vocabulary** — the surface cannot emit an instruction
  to the agent, only a descriptive line.
- **Read-only, single-repo.** The store is opened read-only and one `repo_id` is
  pinned per process, so a query cannot cross repositories.
- **Containment is ancestral, and served-side.** A serve answers only from
  checkpoints contained in the caller's branch ancestry (one capped, memoized
  first-parent-inclusive walk per serve); worktree members of one repository
  family share the store but never see across an uncontained branch. The hook
  channel can never widen this — `--cross-branch` is a CLI-only, reason-bearing,
  audited grant, and the hook injector has no path to it.

See [SECURITY.md](../../SECURITY.md) for the whole-system posture.
