# 4. Redaction and secrets

Rift keeps a permanent record of what your coding agents did, which is only a
good idea if the record is safe to keep. Chapter [03](03-capture.md) showed how
raw material arrives — prompts, tool output, diffs, transcripts. This chapter is
the wall that material crosses before any of it becomes durable, and what that
wall does and does not promise.

The short version: **there is exactly one door into durable storage, and it is
the redaction pipeline's output type** — not a convention or a review
checklist, but a type whose buffer is private and whose only constructor lives
in one function. Persisting a raw byte is a compile error.

---

## 4.1 The one-way wall

Two types in `rift-core` carry every byte that matters.

| Type | What it holds | Lifetime | What it can do |
|---|---|---|---|
| `SecretBytes` | raw, untrusted input: a prompt, tool output, a diff, an imported artifact | local-only, zeroized on drop | be read by the redaction pipeline, and nothing else |
| `RedactedBytes` | bytes that provably passed the pipeline | durable | be written to the ledger, hashed, chained, served |

`SecretBytes` deliberately has no `Debug`, `Display` or `Serialize`, so it
cannot fall into a log line by accident, and it is wiped from memory when it
drops; no durable-write function anywhere in the workspace accepts it.

`RedactedBytes` is the interesting half. Its byte buffer is a **private field**,
so it cannot be fabricated with a struct literal — no `From<Vec<u8>>`, no
`From<&str>`, no mutable accessor. The only way to make one is:

```rust
RedactedBytes::seal(masked_bytes, provenance)
```

…and that function is called **once** in the whole product, at the end of
`redact_with` in `crates/rift-shadow-redact/src/lib.rs`, while every durable
write and every sync path takes `&RedactedBytes`. So "raw bytes cannot be
persisted" is not a rule somebody has to remember; it is something the compiler
refuses to let you express.

> Tip: if you are ever tempted to add a second constructor — a
> `RedactedBytes::from_trusted`, an `already_redacted` shortcut — that is the
> exact shape of escape hatch this design exists to refuse. A caller who can
> declare bytes clean makes the guarantee worth nothing, because the guarantee
> is now the caller's carefulness rather than the type's.

### The two guards that keep the single mint single

A private field stops fabrication but not another crate from *calling* the
constructor — `RedactedBytes` is public, so any crate depending on `rift-core`
could mint one. Two tests close that. The first walks every Rust file under all
five workspace source roots for the call pattern `::seal(`, and anything
outside the redaction crate is a failure; it anchors on the `::` rather than
the type's name, because a type-spelled needle is defeated by an import alias,
a fully-qualified form or a local type alias — all three were demonstrated to
compile and mint while the older needle matched none of them. The second makes
the first exact by pinning `RedactedBytes::seal` as the *only* function in the
workspace named `seal`. When a second `seal` once existed, the fix was to
rename it, never to except its crate by path: that crate also depends on
`rift-core` and could mint too, so a path exception would have been a real hole
dressed up as a false-positive fix. The first guard also asserts it *reached*
each source root, so deleting or renaming a root turns it red rather than
quietly hollow.

### What travels with sealed bytes

Every `RedactedBytes` carries a small provenance record: which rule-pack
versions ran, the SHA-256 of the raw input, and the SHA-256 of the cleared
bytes. Versions and hashes only — never a secret — which is why it is safe to
print. The input hash makes a future recompute-and-compare possible; the output
hash feeds the tamper chain of chapter
[05](05-ledger-and-verification.md).

---

## 4.2 Where the gate sits

Redaction runs inside `condense` — the function that turns a session's raw
capture into a durable checkpoint. It is the **first** thing that happens.

Nothing downstream of the gate ever sees the raw bytes, and the checkpoint
identifier derives from the redacted content — so a checkpoint's identity is a
fact about what was kept, not about what was seen. Three consequences worth
holding on to. **The commit-message trailer's predicted identifier redacts
too**: `prepare-commit-msg` predicts a commit's checkpoint id by assembling the
same archive the post-commit path will write, and since the id is over the
redacted form the prediction has to redact — which is why redaction dominates
that hook's cost and why the opted-in working-tree capture has a small byte
ceiling ([03](03-capture.md) has the budgets). **The report is frozen with the
shard**, as one of its structural blobs. And **verification refuses a shard
without one**: `rift verify` raises a redaction verdict when the report is
missing, malformed or structurally invalid — no rule pack recorded as having
run, or no input address set. A shard that cannot say it was redacted does not
count as verified.

Imports take the same door: a read-only importer reading another tool's
checkpoint artifacts returns every byte as `SecretBytes`, so an imported byte
reaches storage only through the same `redact` call a captured byte takes. A
shard some other tool already redacted is, to Rift, raw.

---

## 4.3 The layers

There are two ways to describe this pipeline, and they do not line up
one-to-one, so it is worth being explicit about which is which.

### The ten-layer frame (historical, and still the shape of the sweep)

Rift's redaction was originally built to match — then beat — a reference
implementation Rift no longer runs: nine regex layers inside a JSON-aware
walker. The comparison lives on as an executable characterisation table at
`crates/rift-shadow-redact/tests/parity.rs`, one row per behaviour, asserting
what the native pipeline does *today* next to the sweep's verdict. Nine of the
ten frames run here — entropy over token runs, the vendor token corpus,
deterministic provider prefixes, credentialed URIs, connection strings, user
rules and rule packs, bounded credential key/value pairs, personally
identifying shapes (**on by default**, where the reference had them opt-in and
off), and the JSON-aware walker they all run inside.

The tenth, a batched heavy-model privacy pass, was **never ported, by
decision**. Rift runs **no model anywhere in capture** — no network call, no
inference, no second pass. The reference's model pass also rewrote commits
*before a push*, so those bytes were already durable when it ran; Rift's answer
is structural — every pass runs pre-storage, and history is never rewritten,
which chapter [17](17-invariants.md) states as a standing rule.

Because it is a characterisation test, no row drifts silently: closing a gap
flips the asserted outcome and the suite fails until the verdict is updated. A
constant pins how many rows are still behaviours the reference had and Rift
lacks; it reads zero. **Zero is not "the sweep is finished"** — several rows
record deliberate divergences where Rift over-redacts in the safe direction,
and relaxing one is a recorded decision, not a passing tidy-up.

### What actually runs (the six detector families)

Independently of that frame, today's pipeline is six classes of detector, and
this is the list to reason from:

| Class | Placeholder | Detectors | What it catches |
|---|---|---|---|
| vendor secret | `[REDACTED:vendor]` | 9 (six vendor shapes + three provider prefixes) — plus every rule your packs contribute | provider tokens with a known prefix and charset |
| credentialed URI | `[REDACTED:uri]` | 2 | `scheme://user:pass@host`, including the empty-user form a cache or queue URL uses |
| connection string | `[REDACTED:jdbc]` | 5 | driver URLs, semicolon key/value lists, quoted password values, and bounded credential key/value pairs |
| private key | `[REDACTED:key]` | 1 | a PEM/OpenSSH private-key block, opener to closer |
| entropy | `[REDACTED:entropy]` | 1 | any token that simply looks random |
| personal data | `[REDACTED:pii.<detector>]` | 11 | email, dotted-quad address, taxpayer ids, four phone spellings, two card-number spellings, street address |

Twenty-nine built-in detectors in total: twenty-eight shape detectors across
four internal tables, plus the entropy layer, which has no pattern at all.

Their proposal order matters and is not alphabetical. Within a single detection
pass the tables run base shapes, then provider prefixes, then credential shapes,
then **your packs**, then entropy, then the personal shapes; equal-strength ties
resolve in exactly that order, so emission order is load-bearing rather than a
listing convenience.

---

## 4.4 Inside the detectors

### Shapes with a gate

A linear-time regular expression can describe a shape, but it cannot check a
checksum, consult an issuing authority's allocation rules, or tell a whole
number from a fragment of a longer one. So shapes a plain digit run can imitate
carry a **gate**: the pattern proposes, the gate disposes.

- Payment card numbers are gated on the Luhn checksum *and* on the network's
  own length and prefix rules — and the grouped spelling additionally requires
  uniform grouping, since a coordinate list that happens to pass Luhn is not
  written in uniform groups.
- Taxpayer ids are gated on the issuing authority's published never-issued
  areas and excluded group and serial ranges.
- Phone numbers are gated on group count and digit totals behind whichever
  prefix is the evidence — the international `+`, a trunk `0`, the exit code.
- Quoted values are gated to narrow past their own quotes, so a password with a
  space in it goes whole rather than stopping at the first space.

**A gate can only ever remove or narrow a candidate.** The range it returns is
always inside the range it was handed, and a test pins that a gate never widens.
That asymmetry makes gates safe to add: the worst a bad gate can do is
under-redact its own detector, never quietly grow another one. It is also why a
version string, a UUID, a commit hash, an epoch timestamp, an unissued
identifier range and a Luhn failure are all left alone — each pinned by a row in
a seeded corpus asserting both directions at once: every seeded secret removed,
every seeded look-alike kept.

### The two shapes that knowingly over-redact

Two are written down in the crate's own module documentation, because a trade a
reader can see is a trade someone can overrule: a bare space-separated
three-three-four digit triple, indistinguishable from a phone number written
the same way; and a `+` followed by eight to fifteen digits, which in a unified
diff is an *added line* of digits, and Rift captures diffs. Measured over this
repository's own diff history the second costs one hit, inside a binary blob.
Narrowing it — a `+` at column zero is a diff marker, not a country code —
would trade a measured near-zero over-redaction for a real under-redaction, so
the behaviour is kept and a test pins it.

### The entropy layer

The entropy layer has no shape at all: it walks token runs — a token character
being alphanumeric or one of `+ / = _ -` — and masks any token at least **20
characters** long scoring at least **4.0 bits** of Shannon entropy per
character. That token class is generous on purpose, since it is what lets a
base64 blob or a slash-separated path be seen as one token rather than
fragments. It was also, by a wide margin, the pipeline's dominant
false-positive cost, because an ordinary long identifier — a snake_case
function name, a CamelCase type, an absolute build path — is a single token
clearing both thresholds: 468 of the 554 spans the pipeline masked over this
repository's own text, and not one of them a credential.

Since 2026-09-11 the layer skips a candidate that is **word-shaped**. The
token is split on `_ - /`, and every segment must read as words: runs of one
letter case no longer than twenty letters, Capitalised humps, digit runs, at
most one single letter and only beside digits (`v7`, `p95`, `0x0011`), and
letters meeting digits at most twice (`sha256`, `full3d` — but not
`JBSWY3DPEHPK3PXP`). A token holding `+` or `=` is never word-shaped, so
base64 and `NAME=value` pairs are judged as before, and a base64url token
fails on its first mixed-case segment because its `-` and `_` are letters
rather than separators. The rule is written so a random generator cannot
satisfy it, and each of those clauses is pinned by a test on both sides. What
it accepts, stated plainly because it is the residual: a secret spelled as
same-case letter groups of twenty or fewer joined by separators or by a single
digit run reads as words. The shape detectors still see it; the entropy floor
does not. On this repository the rule took the layer from 468 hits to 63, and
§4.9 covers how that cost is measured and bounded rather than argued about.

> Tip: the entropy floor is what catches the secret nobody wrote a rule for.
> Before adding a vendor shape, ask whether entropy already covers it: the
> provider-prefix table exists precisely for the credentials it *doesn't* —
> short, structured keys whose charset is too narrow to clear 4.0 bits.

Before anything runs, raw bytes are normalized: decoded as UTF-8 leniently and
stripped of zero-width characters. The second step is not cosmetic — a
zero-width joiner spliced into a token is exactly how a secret would otherwise
walk past a shape detector, and a test plants one to prove it does not.

---

## 4.5 Overlaps, placeholders, and the fixpoint

Real text makes detectors collide: a credentialed URL contains an email-shaped
substring, a vendor token abuts an address, a connection string contains a
password that is also high-entropy. How the collision resolves decides whether
bytes get published by accident.

**Precedence decides attribution, not survival.** Classes rank — private key
above credential-shaped classes, those above personal data, personal data above
entropy — and the higher-precedence span wins the *label*, which is what keeps
the report's per-detector counts meaningful.

**The loser keeps whatever the winner did not claim.** When two spans overlap
the loser is carved rather than dropped, surviving on exactly the bytes the
winner did not take, so the sealed byte set is the union of everything every
layer proposed. Without it, a rule matching a *fragment* of a longer match
would carve it and publish the remainder — masking part of an address and
shipping the rest. One named exception keeps useful context: a span the input's
own structure already explains — a URL's scheme and host around its
credentials, a key name in front of its value, a label in front of a labelled
id — goes whole, so the parse's context is kept rather than half-masked into
nonsense. That exception applies only to a reading of the *input*, never to a
reading of an intermediate blanked copy.

**Settling rounds recover what carving would have cost.** A detector anchored
on a word boundary cannot see a shape abutting another token, so within a
single pass the current winners are hidden and every layer is asked again, and
a shape found only in the blanked copy joins the same pass's proposals. Up to
four rounds run, the cap being a fail-safe, and two properties are pinned: that
a settling round finds shapes the first pass could not, and that settling never
takes a masked byte away.

### Placeholders

Five of the six classes mask to one fixed string; the personal-data family
refines it to the **firing detector**, so a redacted transcript says which
shape was removed:

```
contact [REDACTED:pii.email] at [REDACTED:pii.ipv4] please
password=[REDACTED:jdbc]
token=[REDACTED:vendor]
```

Two properties are load-bearing. **The placeholder is chosen by the detector,
never by the bytes it matched** — it is a compile-time constant, so masked
length never reveals secret length and no byte of any document can reach a
placeholder, which is what keeps a placeholder from becoming a channel for the
text it replaced. And **whole-token granularity**: the label replaces the
*whole* matched span, so an email becomes one `[REDACTED:pii.email]`, never a
label sitting beside a published remainder. The per-detector labels come from
the same table the pipeline reads, so a twelfth personal-data detector gets its
own label by existing and cannot silently inherit the family's fallback.

### Idempotence

Masking substitutes placeholder *text*, not holes, so its output is a document
no detector has been shown yet — `password=[REDACTED:jdbc]` is a
connection-string match that only exists after the first pass. Detect-and-mask
therefore repeats until the text stops changing, which makes the output closed
under its own pipeline: `redact(redact(x))` equals `redact(x)`. A placeholder
the pipeline already wrote suppresses both the count and the rewrite, and
membership is tested against the **whole** emitted set rather than one class's
own family, because a family-scoped check once let the connection-string
detector relabel a personal-data placeholder and made the output depend on
which detector fired last. The loop is bounded at eight passes and **fails
closed**: if masking has not stabilized the call returns an error and no
partially-redacted bytes come back. With a regex engine that cannot compile a
backreference at all, no hostile input can wedge the pipeline into unbounded
work — one property enforced by a bound, the other by the compiler.

---

## 4.6 The JSON-aware walker

Agent transcripts are newline-delimited JSON, and redacting them as flat text
can drop a placeholder across a quote, colon or escape and hand the store a
line that no longer parses — costing replay the whole record rather than the
secret inside it. So the content is parsed, each string leaf **and each object
key** is redacted individually, and the results are substituted back into the
raw bytes; formatting, key order and whitespace survive untouched, and anything
that is not parsable JSON falls back to flat-text redaction, so malformed input
is a *narrower* path, never a bypass.

**The walker does not reduce scope, and that is the whole point.** No
skip-field list, no payload-type test, and the fragment entry point has no
parameter that could ask for fewer layers — a key name is content an attacker
writes, so nothing a key name says may change what runs. The absence is a
**compile-time** property: re-adding it means adding an argument to a function
signature. The cost is real and stated rather than hidden: an extended-thinking
signature is high-entropy base64 and **is** masked, so thinking blocks in a
redacted transcript no longer replay, while transcript identifiers survive
because they sit below the entropy floor on their own merits, not because a
rule exempts them.

Two rules keep the structural path from being a *wider* door than the flat one.
**Substitution is verified**: a leaf's replacement is looked for in the raw
text as JSON-encoded bytes, because valid JSON can encode the same string more
than one way and an attacker-shaped line can make a naive substitution silently
no-op; if any replacement is not found, the whole line falls back to flat
redaction. And **the structural output must be a fixpoint of the flat
pipeline**: judging a leaf alone takes its context away, and some detectors
*are* context — a labelled identifier needs its key and its digits in one text,
and the entropy layer's token boundaries move when an escape stops being a
letter — so the assembled output is re-scanned flat and accepted only if
nothing further would be masked.

That second rule is not decoration. Measured over a corpus of fifteen hundred
generated documents, turning the closure check off leaked one hundred and
forty-two of nearly six thousand seeded needles, in exactly those two classes;
with it on, zero. A generated differential guard — total, no key excluded —
holds the property that the walk never seals less than a flat scan. Underneath
both rules sits a checked precondition: every byte the raw line carries must be
reachable in the parsed model, spelled the way the model spells it, and a line
that cannot be modelled faithfully — a repeated key, a non-canonical escape —
goes flat.

---

## 4.7 The redaction report

Every redaction produces a counts-only report, frozen into the checkpoint as
`redaction_report.json`:

| Field | Meaning |
|---|---|
| `total_spans` | how many spans were masked |
| `by_detector` | count per detector id — `pii.email`, `vendor.aws.access_key_id`, `<pack>.<rule>` |
| `by_class` | count per detector class |
| `bytes_masked` | aggregate masked length — a size, never content |
| `allowlisted` | spans a detector found and policy deliberately kept |
| `rule_pack_versions` | exactly which packs and versions ran |
| `input_sha256` | the address of the raw input, for recompute-and-compare |

The report is **structurally incapable** of carrying a secret: its keys are
detector ids — compile-time constants or pack-declared identifiers held to a
conservative character set — and its values are numbers and hashes, which is why
it is safe to freeze beside the sealed bytes and safe to print.

`allowlisted` deserves a note. Serve policy can mark a specific value as
deliberately kept — a published example key in your own docs, say. An entry is
**subtract-only**: it can keep a value a detector found and can never add a
leak. The kept value is still **counted**, which is the point — the report
proves the value was seen and kept rather than missed.

You meet the report in two places: `rift verify` refuses a shard whose report
is missing or invalid (§4.2), and the dashboard's Recall Viewer lists the
per-detector and per-class counts on a checkpoint's detail.

---

## 4.8 Rule packs

Built-in detectors cover public secret shapes; they cannot know your company's
internal tokens look like `ACME-` followed by eight characters. Rule packs are
how you tell Rift. A pack is a versioned file under
`.rift/policies/redactors/`, in TOML or JSON, loaded recursively:

```toml
# .rift/policies/redactors/acme.toml
schema_version = 1
name = "acme"                    # must equal the filename stem
version = "2026.09.0"
description = "internal token shapes"

[[rules]]
id = "internal_token"
description = "ACME service tokens"
regex = "ACME-[A-Z0-9]{8}"

[[rules.samples]]
input = "ACME-AB12CD34"
redacted = true
```

Commit that file and your teammates get the rule. Personal rules that should
not be shared go in the same tree under a `local/` subdirectory, which you
git-ignore yourself (Rift writes no ignore rule for it) and which loads by the
same walk.

### The rules about rules

**The pack name must equal the file's stem**, so a rule id in a report names a
file you can open; rules are reported as `<pack>.<rule>`. **Packs are additive
only**: a pack adds detectors, nothing it can contain disables a built-in one —
the built-in pipeline is not addressable from a pack at all — and a pack rule
may not take a built-in detector's id, because a report count has to attribute
to exactly one detector. **Every pack rule joins the vendor-secret class**, so
it masks to `[REDACTED:vendor]` while its count appears under its own id.
**Samples are self-tests, and never fatal**: a rule that disagrees with its own
sample is *kept*, with a warning carrying the sample's *length* and never its
text, because a wrong self-test is not a reason to stop redacting and a sample
is written to look exactly like a secret. **A rule that will not compile is
dropped on its own**, not the pack around it, as is a rule matching the empty
string — it matches at every position, seals nothing, and would fill the
document with placeholders (`x*` is the whole class of that mistake; `x+` is
the fix). And **YAML is recognised and refused**, not silently ignored: the
workspace pins no YAML parser, and adding a dependency is not a decision the
loader gets to make on your behalf.

### Nothing here can fail a commit

Redaction runs on the capture path, and a policy file you are mid-edit must
never be able to stop a commit, so the loader is infallible by signature: a
missing directory is "no packs configured", and a malformed file, an unreadable
directory, a bad pattern or a duplicate name each becomes a diagnostic and is
skipped. The failure mode is *fewer extra rules* with the built-in pipeline
untouched underneath.

Four caps bound the work, so a runaway directory cannot become a runaway hook:
**256 pack files per tree**, counted over *candidate* files rather than
successful loads — otherwise a directory of malformed files gets parsed in full
and the guard never fires; **1 MiB per file**, keeping an accidentally huge
file off the capture path; **8 levels of directory depth** below the root; and
**64 characters** for names and rule ids, which flow into detector ids and
diagnostics. Candidate paths are gathered and **sorted** before the cap
applies, so *which* 256 files load is the same answer on two machines, and
symlinks are never followed: a pack tree is repository content, and a symlink
out of it is a surprise rather than a feature. Diagnostics never quote a rule's
sample text, though they carry the parser's own message, which may name a value
it choked on — accepted deliberately, since a pack is policy you authored and
its bytes already sit in your working tree unredacted.

Two crates do the halves and neither can do the whole: `rift-config` reads and
validates the files and compiles no patterns; `rift-shadow-redact` compiles the
patterns, reads no files, and runs each rule's samples **against the real
pipeline** rather than a second copy. Neither depends on the other, and the
`rift` binary is the only place depending on both.

> Tip: to check a pack, make a commit and look at the resulting checkpoint's
> report. Your rule's id appears in `by_detector` with a count. A rule that
> loaded but never matched is silent — which is how you tell "my regex is
> wrong" apart from "my pack did not load", since the latter prints a
> diagnostic.

---

## 4.9 The false-positive census

A redactor that masks everything is safe and useless, so Rift treats its
**false-positive cost as a measured, recorded number** rather than an argument
— a regression baseline, not a comparison against anything. The measurement is
in `benchmarks/redaction-fp-baseline.md`, and the test that enforces it reads
its numbers back *out of that file*, which makes the artifact load-bearing
rather than prose.

**What is measured.** The whole public pipeline — not a hand-picked subset —
over this repository's own tracked and unignored text, filtered to eleven text
file extensions. Three regions are excluded: the redaction crate and the fuzz
tree, because they *are* the seeded-secret corpus, and the documentation tree
under `docs/`, which is prose about the pipeline rather than material it would
ever protect. A hit is one masked span attributed to the detector that claimed
it, so the number is what a developer would actually lose out of a captured
transcript.

**The ceilings.** The artifact carries one per detector, and the test fails
when a detector's count exceeds its ceiling, when a detector fires that the
table does not list, **and** when the table lists an id that is not a real
detector — that third arm means a new detector cannot be added without a
measured false-positive cost recorded for it. Ceilings sit at the measured
value plus headroom for ordinary growth, with one deliberate exception:
**every numeric personal-data detector has a ceiling of exactly zero**, because
they are the claim that Rift's numeric shapes do not fire on ordinary numeric
text, so widening one has to be argued against that zero. The dominant cost
sat in one layer — the entropy detector's ceiling was the largest in the file
(470 until 2026-09-11) because long identifiers and build paths are single
tokens that clear the thresholds, and a hand classification of a sample of
those candidates found not one credential among them. The word-shape rule in
§4.4 took that layer from 468 hits to 63 on the same tree, and the ceiling was
re-derived to 80 by owner decision; what remains under it is secret-shaped by
construction — ULIDs, hex-bearing paths, `NAME=value` pairs, the seeded
fixtures.

**The subtraction fence.** Some hits are irreducible: a token chosen outside
this repository and cited by a running test, or one inside a frozen import
fixture that may not be edited to make a test pass. Those can be *classified*
and subtracted from a detector's count before it meets its ceiling, and the
vocabulary is **closed at exactly two classes** — adding a third is an owner
escalation, exactly as raising a ceiling is. Each row asserts *exactly* what
the whole file measures for that detector, so a hit added beyond a row's
recorded total is a violation at the row rather than shared headroom spent
quietly. An **empty** fence is the identity and the correct starting state; an
**absent** fence is refused, because a reader that silently returns nothing
when its data has been deleted is a check that cannot fire.

**The standing policy is shed-first**: a change that adds hits sheds them
rather than raising a ceiling, and a raise is an owner decision recorded in the
artifact with its date and reason. Classification blesses a *count in a file*,
never a file — an entry whose path has left the tree is a violation, not a
tidy-up owed later.

Beside the census sit a seeded corpus asserting both directions at once (every
seeded secret removed, every seeded look-alike kept) and three fuzz targets:
flat redaction, the JSON walker and the pack loader. Run the whole thing with
`cargo test -p rift-shadow-redact`; chapter [15](15-benchmarks.md) covers the
measurement discipline this shares with the rest of the repository's recorded
numbers.

---

## 4.10 What redaction does not claim

Redaction is a gate, not a proof that no detector ever misses. Four honest
limits, each with a home you can check. **Known misses are written down**: the
parity table records behaviours as gaps, over-redactions and refusals and pins
the count, so opening or closing one is a recorded decision rather than a
drift. **Your agent's own files are not Rift's**: the transcripts your coding
assistant writes exist, unredacted, whether or not Rift is installed, and Rift
only reads them and never copies a byte anywhere unsealed. **A pack file can
hold a secret you typed**: a rule's sample is *shaped* like a secret by design,
and although the loader never echoes sample text, a real secret pasted into a
sample is at rest by your own choice — packs are the one place a user can write
a secret into the repository through Rift. And **not every durable byte is
typed**: a shard's structural blobs — the report, metadata, digests, content
addresses — are written as plain bytes, because they are the byte-parity
surface and the entropy layer would mask their own hashes; Rift authors them
from counts, ids and content-addresses, and the digest artifacts carry a
containment test on top, so the artifact whose job is to summarise a prompt is
tested never to quote it.

The full inventory — every place a byte the gate should have caught could still
be found, who controls it, and what stands in the way — is the "where
under-redacted bytes could persist" table in [SECURITY.md](../../SECURITY.md),
which deliberately marks the cells where a statement was *measured* but is not
*guarded*.

**And redaction is only the first of two walls.** It decides what may become
durable; a second, separate decision governs what may be *served back* to an
agent, where trust is derived from what the ledger records about where bytes
came from — never from a label the caller supplies and never from the content
itself — and every path to an agent routes through one serializer, so untrusted
material arrives as a neutral digest rather than raw text. Artifacts whose
origin is unknown classify as untrusted and fail closed. That wall is chapter
[07](07-serving-and-injection.md).

---

## 4.11 A converted document is derived, durable text

Reading a document as Markdown ([07](07-serving-and-injection.md)) writes a
conversion of repository bytes into local state and keeps it. That is a new
durable artifact derived from files, so it answers to this chapter's rules
rather than sitting beside them.

**Two refusals happen before a byte is read.** A path this repository declared
sensitive, and a path its serving policy never serves, are never converted: a
sidecar is durable text derived from the file, so a file that must not be
served must not be re-encoded beside the checkout either. The check runs on the
lexically normalised path, so the traversal spellings a glob would miss are
resolved first.

**The converter is a parser surface, and is treated as one.** It reads formats
a repository can contain — containers, markup, binary office documents, PDFs —
which is exactly the shape of input that turns a parser into a vulnerability.
So it carries a security crate's coverage floor, a fuzz target in the required
smoke run, and deterministic output: the same bytes convert to the same
Markdown, byte for byte, which is what makes a content-addressed sidecar
meaningful and a golden test possible. It runs no OCR, makes no network call —
the hosted extraction path its engine offers elsewhere never compiles in — and
a document over the input ceiling is refused rather than streamed.

**Nothing about the conversion enters a pushed channel.** The Markdown is
served in the tool-result position of the read it stands in for, where the
agent asked for it; the hook responses that route an agent to it carry a path,
a hash and counts and never a line of the document. The adversarial corpora
carry document fixtures whose text tries to become instructions, asserting
exactly that.

**A sidecar is a cache, not a record.** It is derived from bytes already in
your checkout, it is git-ignored, it is collected on its own clock, and
deleting the store costs one reconversion. It never becomes a captured
artifact, so it does not pass the gate of §4.2 — the bytes it holds were never
`SecretBytes` to begin with, and a document that is captured is captured
through the ordinary path, redacted like anything else.

---

## 4.12 One thing to know about configuration

`.rift/config.toml`'s `[redaction]` table has two keys, `entropy_threshold` and
`extra_patterns`, and **today they are recorded but not applied**: parsed,
validated and folded into the policy hash, with nothing in the pipeline reading
them back. The entropy floor is compiled in at 20 characters and 4.0 bits, and
the way a project adds patterns is a rule pack (§4.8). So a lowered
`entropy_threshold` changes your policy hash and not what gets masked — see
[02](02-configuration.md), and the gap listed in [18](18-known-gaps.md).

---

## Related chapters

[05-ledger-and-verification.md](05-ledger-and-verification.md) ·
[07-serving-and-injection.md](07-serving-and-injection.md) ·
[13-storage-and-schema.md](13-storage-and-schema.md) ·
[17-invariants.md](17-invariants.md) · [SECURITY.md](../../SECURITY.md) for the threat model and the persistence
inventory.
