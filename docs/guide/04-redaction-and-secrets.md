# 4. Redaction and secrets

Rift keeps a permanent record of what your agents did, which is only a good idea if the
record is safe to keep. This chapter is the wall that raw material ([03](03-capture.md))
crosses before it becomes durable. **There is exactly one door into durable storage, and it
is the redaction pipeline's output type** — a type whose buffer is private and whose only
constructor lives in one function. Persisting a raw byte is a compile error.

---

## 4.1 The one-way wall

| Type | Holds | Lifetime | Can |
|---|---|---|---|
| `SecretBytes` | raw, untrusted input: a prompt, tool output, a diff, an imported artifact | local-only, zeroized on drop | be read by the redaction pipeline, nothing else |
| `RedactedBytes` | bytes that provably passed the pipeline | durable | be written, hashed, chained, served |

`SecretBytes` has no `Debug`, `Display` or `Serialize`, so it cannot fall into a log
line, and no durable-write function accepts it. `RedactedBytes` has a **private
buffer**: no struct literal, no `From<Vec<u8>>`, no mutable accessor. The only
constructor is `RedactedBytes::seal(masked_bytes, provenance)`, called **once** in the
product, at the end of `redact_with` in `crates/rift-shadow-redact/src/lib.rs`, while
every durable write and sync path takes `&RedactedBytes`. "Raw bytes cannot be
persisted" is something the compiler refuses to let you express, not a rule to
remember.

### The two guards that keep the single mint single

A private field stops fabrication but not another crate *calling* the constructor. One test
walks every Rust file under all five workspace roots for `::seal(` and fails on any hit
outside the redaction crate; it anchors on `::` because a type-spelled needle is defeated by
an import alias or a fully-qualified form (both compile and mint), and it asserts it
*reached* each root so deleting one turns it red. A second pins `RedactedBytes::seal` as the
*only* function named `seal`: a second `seal` is renamed, never excepted by crate path,
since that crate could mint too. A `from_trusted` shortcut is the escape hatch this design refuses: it makes the guarantee the caller's carefulness. Every `RedactedBytes` carries provenance (rule-pack versions, the
SHA-256 of raw input and of cleared bytes — never a secret) whose output hash feeds the
chain of [05](05-ledger-and-verification.md).

---

## 4.2 Where the gate sits

Redaction is the **first** thing `condense` does. Nothing downstream sees raw bytes, and the
checkpoint id derives from the redacted content, so identity is a fact about what was kept.
**The commit trailer's predicted id redacts too**, which is why redaction dominates
`prepare-commit-msg` and why opted-in working-tree capture has a small byte ceiling
([03](03-capture.md)). **The report is frozen with the shard**, and **verification refuses a
shard without a valid one** (missing, malformed, no rule pack recorded, or no input
address): a shard that cannot say it was redacted does not count as verified.

Imports take the same door: a shard some other tool already redacted is, to Rift, raw `SecretBytes`.

---

## 4.3 The layers

Rift runs **no model anywhere in capture** — no network call, no inference. Redaction must
be deterministic and must finish before anything is durable, since a pass that ran later
would find the bytes already written; so every pass runs pre-storage, and history is never
rewritten ([17](17-invariants.md)).

A characterisation table, `crates/rift-shadow-redact/tests/parity.rs`, holds one row per
behaviour with a verdict (gap, over-redaction, refusal); closing a gap flips the asserted
outcome and fails the suite until the verdict is updated. A constant pins the open gaps; it
reads zero, which is **not "finished"**: several rows are deliberate over-redaction in the
safe direction, and relaxing one is a recorded decision.

### The six detector families

| Class | Placeholder | Detectors | Catches |
|---|---|---|---|
| vendor secret | `[REDACTED:vendor]` | 9 (six vendor shapes + three provider prefixes), plus every pack rule | provider tokens with a known prefix and charset |
| credentialed URI | `[REDACTED:uri]` | 2 | `scheme://user:pass@host`, including the empty-user form |
| connection string | `[REDACTED:jdbc]` | 5 | driver URLs, semicolon lists, quoted passwords, bounded credential key/value pairs |
| private key | `[REDACTED:key]` | 1 | a PEM/OpenSSH block, opener to closer |
| entropy | `[REDACTED:entropy]` | 1 | any token that looks random |
| personal data | `[REDACTED:pii.<detector>]` | 11 | email, dotted-quad address, taxpayer ids, four phone spellings, two card spellings, street address |

Twenty-nine built-in detectors: twenty-eight shapes plus the patternless entropy layer.
Personal data is **on by default**. Within a pass the tables run base shapes, provider
prefixes, credential shapes, **your packs**, entropy, personal shapes, and equal-strength
ties resolve in that order, so emission order is load-bearing.

---

## 4.4 Inside the detectors

### Shapes with a gate

A linear-time regex cannot check a checksum or an issuing authority's allocation rules, so
shapes a digit run can imitate carry a **gate**: the pattern proposes, the gate disposes.
Cards need Luhn plus the network's length and prefix rules (and uniform grouping when
grouped); taxpayer ids need the authority's never-issued ranges; phones need group count and
digit totals behind an evidence prefix; quoted values narrow past their quotes so a password
with a space goes whole. **A gate can only remove or narrow a candidate**, a test pins it,
so a bad gate can at worst under-redact its own detector, never grow another — and a
version string, UUID, commit hash, epoch timestamp or Luhn failure is left alone, each
pinned by a seeded-corpus row asserting both directions.

Two shapes knowingly over-redact, written in the crate docs so the trade can be overruled:
a bare space-separated 3-3-4 digit triple (indistinguishable from a phone), and a `+`
followed by 8–15 digits, which in a unified diff is an *added line* of digits. Over this
repository's diff history the second costs one hit, inside a binary blob; narrowing it
would trade near-zero over-redaction for a real under-redaction.

### The entropy layer

It walks token runs — alphanumerics and `+ / = _ -` — and masks any token of at least **20
characters** scoring at least **4.0 bits** of Shannon entropy per character. The token class
is generous so a base64 blob or a path is one token, which also makes the layer the dominant
false-positive source: a snake_case name or build path clears both thresholds (468 of the
554 spans masked over this repository's text, none a credential).

So the layer skips a **word-shaped** candidate: split on `_ - /`, every segment must read as
words — one-case runs up to twenty letters, Capitalised humps, digit runs, a single letter
only beside digits (`v7`, `p95`), letters meeting digits at most twice (`sha256`, not
`JBSWY3DPEHPK3PXP`). A token with `+` or `=` is never word-shaped, and a base64url token
fails on its first mixed-case segment. The rule is written so a random generator cannot
satisfy it, each clause pinned on both sides. The stated residual: a secret spelled as
same-case letter groups of twenty or fewer joined by separators or one digit run reads as
words; the shape detectors still see it, the entropy floor does not. It takes the layer from
468 hits to 63 here (§4.9).

Provider prefixes cover short structured keys too narrow to clear 4.0 bits.

Bytes are first decoded leniently and stripped of zero-width characters, since a zero-width
joiner spliced into a token would otherwise walk a secret past a shape detector.

---

## 4.5 Overlaps, placeholders, and the fixpoint

**Precedence decides attribution, not survival.** Private key outranks credential classes,
those outrank personal data, which outranks entropy; the winner takes the *label*, keeping
per-detector counts meaningful. **The loser keeps whatever the winner did not claim**: an
overlapping span is carved, not dropped, so the sealed set is the union of every layer's
proposals — otherwise a rule matching a *fragment* of a longer match would publish the
remainder. One exception keeps context: a span the input's own structure explains (a URL's
scheme and host around its credentials, a key name before its value) goes whole.
**Settling rounds** recover what carving costs: a boundary-anchored detector cannot see a
shape abutting another token, so winners are hidden and every layer re-asked, up to four
rounds, never taking a masked byte away.

### Placeholders

Five classes mask to one fixed string; personal data refines to the **firing detector**
(`[REDACTED:pii.email]`, `password=[REDACTED:jdbc]`) so a transcript says which shape was
removed. The placeholder is a compile-time constant chosen by the detector, never the
bytes, so masked length never reveals secret length. **Whole-token granularity**: the label replaces the whole span, never beside a remainder.

### Idempotence

Masking substitutes placeholder *text*, so its output is a document no detector has seen
(`password=[REDACTED:jdbc]` matches only after the first pass). Detect-and-mask repeats
until stable, so `redact(redact(x))` equals `redact(x)`; a placeholder already written
suppresses both count and rewrite, tested against the **whole** emitted set, since a
family-scoped check would let one detector relabel another's placeholder and make output
depend on firing order. The loop is bounded at eight passes and **fails closed**: if masking has not stabilized the
call errors and returns no partially-redacted bytes.

---

## 4.6 The JSON-aware walker

Transcripts are newline-delimited JSON, and flat redaction can drop a placeholder across a
quote or escape and leave a line that no longer parses, costing replay the whole record.
So content is parsed, each string leaf **and each object key** is redacted individually and
substituted back into the raw bytes, preserving formatting and key order. Unparsable input
falls back to flat redaction: a *narrower* path, never a bypass.

**The walker does not reduce scope.** No skip-field list, no payload-type test, and the
entry point has no parameter that could ask for fewer layers: a key name is
attacker-written content, so nothing it says may change what runs. The cost is stated: an
extended-thinking signature is high-entropy base64 and **is** masked, so thinking blocks in
a redacted transcript do not replay.

Two rules keep the structural path from being a wider door. **Substitution is verified**: a leaf's replacement is searched for as JSON-encoded bytes,
since valid JSON encodes a string several ways and a crafted line can make naive
substitution silently no-op; any miss sends the whole line flat. **The output must be a
fixpoint of the flat pipeline**: judging a leaf alone removes context some detectors *are*
(a labelled id needs its key and digits together), so the assembled output is re-scanned
flat and accepted only if nothing more would be masked. Without that check, 1,500 generated documents leaked 142 of nearly
6,000 seeded needles; with it, zero. A line that cannot be modelled faithfully (repeated
key, non-canonical escape) goes flat.

---

## 4.7 The redaction report

Each redaction produces a counts-only `redaction_report.json`, frozen into the checkpoint:
`total_spans`, `by_detector` (`pii.email`, `<pack>.<rule>`), `by_class`, `bytes_masked`
(a size), `allowlisted`, `rule_pack_versions` and `input_sha256`. It is **structurally
incapable** of carrying a secret — keys are compile-time detector ids or pack identifiers
held to a conservative character set, values are numbers and hashes — so it is safe to
freeze and print. `allowlisted` counts values serve policy deliberately keeps (a published
example key): **subtract-only**, it can keep a found value and never add a leak, and the
kept value is still **counted**, proving it was seen rather than missed. `rift verify`
refuses a shard whose report is missing or invalid (§4.2).

---

## 4.8 Rule packs

Built-in detectors cannot know your internal tokens look like `ACME-` plus eight
characters; a pack is a versioned TOML or JSON file under `.rift/policies/redactors/`:

```toml
# .rift/policies/redactors/acme.toml
schema_version = 1
name = "acme"                    # must equal the filename stem
version = "1.0.0"
description = "internal token shapes"
[[rules]]
id = "internal_token"
description = "ACME service tokens"
regex = "ACME-[A-Z0-9]{8}"
[[rules.samples]]
input = "ACME-AB12CD34"
redacted = true
```

Commit it and teammates get the rule; personal rules go under a git-ignored `local/`
subdirectory (Rift writes no ignore rule) and load by the same walk.

### The rules about rules

**The pack name equals the file stem**, so a report's `<pack>.<rule>` names a file you can
open. **Packs are additive only**: nothing disables a built-in detector, and a rule may
not take a built-in's id because a report count must attribute to one detector. **Every
pack rule joins the vendor class** (`[REDACTED:vendor]`, counted under its own id).
**Samples are self-tests and never fatal**: a disagreeing rule is *kept* with a warning
carrying the sample's *length*, never text. **A rule that will not compile, or matches the
empty string, is dropped alone** (`x*` matches everywhere and floods the document; `x+` is
the fix). **YAML is refused**: no YAML parser is pinned.

### Nothing here can fail a commit

A policy file you are mid-edit must never stop a commit, so the loader is infallible by
signature: a malformed file, bad pattern or duplicate name becomes a diagnostic and is
skipped — *fewer extra rules*, the built-in pipeline untouched. Four caps bound a runaway
directory: **256 pack files** (counted over *candidates*, else malformed files are parsed in
full and the guard never fires), **1 MiB per file**, **8 levels deep**, **64 characters** per
name and rule id. Candidates are **sorted** first so the same 256 load everywhere; symlinks are never followed. Diagnostics never quote a sample (the parser's message may; a pack is your own policy).

`rift-config` reads and validates files and compiles no patterns; `rift-shadow-redact`
compiles patterns, reads no files and runs each sample **against the real pipeline**;
neither depends on the other. To check a pack, commit and read the checkpoint's report: a
rule that loaded but never matched is silent, which tells "wrong regex" from "pack did not
load" (that prints a diagnostic).

---

## 4.9 The false-positive census

A redactor that masks everything is safe and useless, so **false-positive cost is a
measured, recorded number**, a regression baseline. It lives in
`benchmarks/redaction-fp-baseline.md`, and the enforcing test reads its numbers back *out of
that file*. The whole pipeline runs over this repository's tracked text, excluding the
redaction crate and fuzz tree (they *are* the seeded-secret corpus) and `docs/`; a hit is
one masked span attributed to its detector — what a developer would lose from a transcript.

The artifact holds one ceiling per detector. The test fails when a count exceeds it, when a
detector fires that the table omits, **and** when the table lists a non-detector id — so a
new detector cannot land without a recorded cost. Ceilings are the measured value plus
headroom, except **every numeric personal-data detector at exactly zero**: that is the
claim that numeric shapes do not fire on ordinary numeric text, so widening one is argued
against zero. The entropy layer is the dominant cost: 63 hits, ceiling 80, the remainder
secret-shaped by construction (ULIDs, hex-bearing paths, `NAME=value` pairs, fixtures).

Irreducible hits (a token cited by a test, or inside a frozen import fixture) can be
*classified* and subtracted before the ceiling, in a vocabulary **closed at exactly two
classes**; a third, like a raised ceiling, is a reviewed decision with its reason. Each row asserts *exactly*
what the file measures, so an extra hit is a violation at the row; an **absent** fence is
refused, since a reader that returns nothing when its data is deleted cannot fire. **The
standing policy is shed-first**: a change that adds hits sheds them. A seeded corpus and
three fuzz targets (flat redaction, the walker, the pack loader) sit beside the census; run
`cargo test -p rift-shadow-redact`.

---

## 4.10 What redaction does not claim

Redaction is a gate, not a proof that no detector misses. **Known misses are written down**
in the parity table. **Your agent's own transcripts exist unredacted** with or without Rift,
which only reads them. **A real secret pasted into a pack sample is at rest by your choice.**
**A shard's structural blobs** (report, metadata, digests, content addresses) are plain
bytes because entropy would mask their own hashes; Rift authors them from counts, ids and
addresses, and a containment test keeps the digest artifact from quoting a prompt. Where
under-redacted bytes could persist is inventoried in [SECURITY.md](../../SECURITY.md).

**Redaction is the first of two walls**; what may be *served back* is [07](07-serving-and-injection.md)'s.

---

## 4.11 A converted document is derived, durable text

Reading a document as Markdown ([07](07-serving-and-injection.md)) writes a conversion of
repository bytes into local state, so it answers to this chapter. A path the repository
declared sensitive, or its serving policy never serves, is **never converted** (checked on
the lexically normalised path): a sidecar is durable text derived from the file, so a file
that must not be served must not be re-encoded beside the checkout. **The converter is a
parser surface** (containers, markup, office files, PDFs), so it carries a security crate's
coverage floor, a required fuzz target and byte-for-byte deterministic output; it runs no
OCR, makes no network call and refuses oversized input. **Nothing about the conversion
enters a pushed channel**: hook responses carry a path, a hash and counts, and the
adversarial corpora include documents whose text tries to become instructions. **A sidecar
is a cache, not a record**: git-ignored, collected on its own clock, never a captured
artifact, so it does not pass §4.2's gate — its bytes were never `SecretBytes`.

---

## 4.12 One thing to know about configuration

`.rift/config.toml`'s `[redaction]` table takes no settings: **a config that sets
`entropy_threshold` or `extra_patterns` does not load**, and the error points at
`.rift/policies/redactors/`. The floor is compiled in at 20 characters and 4.0 bits, and
patterns are a rule pack (§4.8). Neither key ever reached the redactor, so accepting them
would change your policy hash and not what gets masked ([02](02-configuration.md)).

---

## Related chapters

[05](05-ledger-and-verification.md) · [07](07-serving-and-injection.md) ·
[13](13-storage-and-schema.md) · [17](17-invariants.md) ·
[SECURITY.md](../../SECURITY.md) for the threat model and persistence inventory.
