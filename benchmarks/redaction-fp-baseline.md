# Redaction false-positive baseline

The redaction gate's false-positive criterion asks for "a false-positive rate measured against this
repository's own text with a **recorded absolute baseline** (a regression, not a
comparison)". This file is that record, and
`the_recorded_false_positive_baseline_holds_over_this_repositorys_own_text`
(`crates/rift-shadow-redact/src/lib.rs`) is the instrument that keeps it true —
the numbers below are read *out of this file* by that test, so a ceiling here is
load-bearing rather than prose.

## What is measured

The **whole pipeline** — the public `redact()`, not a hand-picked subset of
detectors — over this repository's own tracked and unignored text:
`git ls-files --cached --others --exclude-standard`, filtered to the eleven text
extensions the walk knows. Three regions are excluded.
`crates/rift-shadow-redact/**` and `fuzz/**` are excluded because they *are*
the seeded-secret corpus; counting them would be counting the tests' own
needles as repository content. `docs/**` is excluded because the tracked guide
that lives there is prose *about* this pipeline, not material the pipeline
protects: its worked examples spend headroom that belongs to real source
files, and a chapter gaining an example must not push a detector over a
ceiling that measures developer content.

A hit is one masked span, attributed to the detector that claimed it
(`report.by_detector`). The number is therefore what a developer would actually
lose out of a captured transcript of this repository, not an upper bound on
candidates.

## The baseline

Measured 2026-08-17 at base `e2b49f4`, before any of the false-positive fixes:

| | |
|---|---|
| files scanned | **493** |
| bytes scanned | **7 508 629** |
| masked spans | **446** |
| wall clock | 6.4 s (debug, in-crate) |

The first reading — taken before this artifact existed, from a scratch binary
outside the worktree — was **492 files / 7 503 869 bytes / 446 spans**. This file
is itself repository text and is therefore inside its own corpus; the delta is
one file and 4 760 bytes, and it contributes **zero** hits, which is on purpose:
no literal secret shape and no long identifier is written down here.

| detector | hits | a hit here is |
|---|---|---|
| `entropy.high` | **383** | a **false positive** — see the classification below |
| `pii.email` | **38** | synthetic fixture addresses (`@example.com`, `@rift.invalid`, `@rift.local`) |
| `vendor.aws.access_key_id` | **20** | the tree's own seeded fixture key — a **true** positive by design |
| `uri.credentials` | **4** | seeded fixtures in the inclusion tests — **true** by design |
| `vendor.github.pat` | **1** | a seeded fixture under `tests/fixtures/` — **true** by design |
| every numeric PII detector | **0** | — |
| every other vendor shape, `dsn.password`, `key.pem_block` | **0** | — |

### The hand classification

Thirty entropy candidates were read individually — every 13th of the 384 the
scan proposes — and **30 of 30 are false positives; not one is a credential**:

- absolute build paths under the user's home (5 of the 30) — `/` is a token
  character, so a path is one token;
- snake_case test-function names (12+), which clear 20 characters easily;
- CamelCase type names, design-note filename slugs, a git ref, a ULID session id, and a
  price-seed key (the rest).

The cause is the token class: `_ - / + =` are token characters, so an ordinary
long identifier or path is a single token and a 20-character identifier with
enough distinct letters clears Shannon 4.0. This is the pipeline's dominant
false-positive cost, it is concentrated in **one** layer, and no PII decision
touches it.

**Fixed at the layer, 2026-09-11, by owner decision.** The entropy layer now
skips a candidate that is *word-shaped* (`word_shaped` in the crate): split on
`_ - /`, every segment must read as words — runs of one letter case of at most
twenty letters, Capitalised humps, digit runs, at most one single letter and
only beside digits, letters and digits meeting at most twice — and a token
holding `+` or `=` is never word-shaped. Measured over this repository's own
text the layer fell from **468 to 63** masked spans, and what remains is
secret-shaped by construction: ULIDs, hex-bearing paths, `NAME=value` pairs,
tool-use ids, the seeded fixtures. The seeded corpus, the parity table and the
proptest that every proposed byte is masked all still hold; the residual the
rule accepts — a secret spelled as same-case letter groups of twenty or fewer,
joined by separators or a single digit run — is stated at the function, pinned
on both sides of its boundary by `the_word_piece_cap_is_the_boundary`, and
named in the guide.

The numeric PII layer — the detectors whose subject is a bare digit run, and
therefore the ones that could plausibly fire on ordinary numeric text — costs
**exactly zero** over 7.5 MB of this repository's own text. That is the number
any proposal to widen a numeric shape has to be argued against, and it is why
the four `Verdict::Gap` rows that would widen one (`pii.card_pan_comma_separated`,
`pii.phone_uk_national`, `pii.phone_international_zero_prefix`,
`pii.street_address`) are decided on measurement rather than on shape.

## The ceilings the test enforces

`id = ceiling`. The test fails when a detector's count **less the hits the
classified fence below accounts for** exceeds its ceiling, **and** when a
detector fires that this table does not list, **and** when this table lists an
id that is not a built-in detector — so a new detector cannot be added without a
measured false-positive cost recorded here.

**Owner decision, 2026-09-01 (worktree-family close review):** `entropy.high` moves 460 →
470 — the one-time re-derivation the frozen-number clause below reserves for
the owner. The 460 value sat at exactly zero headroom through the whole of that
work; the raise is headroom for ordinary growth, not absorption of a
regression, and the shed-only policy is unchanged.

Ceilings are set at the measured value plus headroom for ordinary repository
growth, except the numeric PII detectors, which are **exact zeros**: they are the
claim, so they get no headroom. A detector whose measured value is zero but whose
hits would be *true* positives (a vendor key shape appearing in a new fixture)
carries a small non-zero ceiling instead, because reddening on a correctly
detected fixture would teach a contributor to weaken the detector.

**Owner decision, 2026-09-11 (the word-shape fix):** `entropy.high` moves
470 → 80. Not a raise and not the frozen-number clause's re-derivation for
growth: the layer's semantics changed by owner order — identifiers, paths and
file names are read as words — and the ceiling is re-derived from the tree
measured under the new rule, **63 raw − 1 classified = 62**, with headroom for
ordinary growth on the same terms as before. The shed-only policy is unchanged;
the number is frozen again at 80.

```fp-baseline
entropy.high = 80
pii.email = 60
pii.ipv4 = 0
pii.us_taxpayer_id = 0
pii.us_taxpayer_id_labelled = 0
pii.phone_e164 = 0
pii.phone_intl_zero_prefix = 0
pii.phone_nanp = 0
pii.phone_uk_national = 0
pii.street_address = 0
pii.card_pan = 0
pii.card_pan_grouped = 0
uri.credentials = 20
uri.credentials_empty_user = 10
dsn.connection_string = 5
dsn.password = 10
dsn.password_quoted = 5
key.pem_block = 10
kv.authorization_header = 5
kv.credential_colon = 5
provider.anthropic.api_key = 10
provider.github.fine_grained_pat = 10
provider.supabase.secret_key = 10
vendor.aws.access_key_id = 40
vendor.github.pat = 10
vendor.google.api_key = 10
vendor.openai.key = 10
vendor.slack.token = 10
vendor.stripe.secret = 10
```

## The classified fence

The subtraction rule — recorded 2026-08-24, **ratified by the owner 2026-09-01**
at the worktree-family close review — changed what the ceiling above binds. It binds
`raw − Σ classified`, and this fence is the only place that difference may live.

A hit is **classifiable** only if it is one of exactly two things. The vocabulary
is **closed**; adding a third class is an owner escalation, exactly as raising
the ceiling is.

**`pinned-evidence`** — a token **whose value is determined outside this
repository**: an upstream object id, a vendor commit or tag, or a published
retrieval URL, cited by a running test. Membership test: *no commit in this
repository could have chosen this value, and deleting or shortening it makes a
committed claim unverifiable.* A token this repository minted — a ULID, a
locally generated digest, a fixture id a regeneration would change — is **not**
pinned evidence, however many tests cite it.

**`frozen-bytes`** — a token inside the pre-Rift dialect readers and import
fixtures the project's standing rules forbid editing to make a test pass. That rule
names the legacy directory prefix outright; this file may not repeat it, because
the owner ordered the legacy brand purged from first-party surfaces and this
file is one. The scope is that rule's scope — the read-compat and import corpus — and not
golden fixtures in general, which is a second parking lot rather than a class.

**Neither class admits a change's own new or regenerable artifacts.** Path runs,
duplicated mentions, env-var names, house-style test names, build paths and
product configuration are **not** classifiable and remain shed-only under
paragraph 2 of the policy below.

### The grammar, and what the guard does with each field

```text
--- <class>
<path>
	<detector>	<classified>	<file_total>	<class>: <reason>
```

Grouped by path, one section per class, shrink-only — the same shape as the
brand baseline under `ci/`, adopted for the same reason: a path is one
high-entropy token, so stating each path once is what lets the table grow
without walking the ceiling upwards.

Both numbers are load-bearing and they do different jobs:

- **`classified`** is subtracted from that detector's corpus-wide count;
- **`file_total`** is asserted **exactly** against what the file measures. A hit
  added to a classified file beyond its recorded total is a violation **at the
  row**, in the commit that adds it — not shared headroom spent quietly. This is
  the arm the brand baseline runs, and it is what buys back the exactness that
  the coarser key costs: the key here is `(path, detector)` rather than
  `(path, line)`, because quoting the token would add it to the corpus being
  measured, and `file_total` is what makes this table a per-file **census**
  rather than a per-file *classified* census.

Classification blesses a *count in a file*, never a file. An entry whose path has
left the corpus is a **violation**, not a tidy-up owed later.

**Refused at parse**, so the table cannot rot into a parking lot: an unknown
class; **a repeated `(path, detector)` anywhere in the fence**; a class section
opened twice; a second group for one path inside one section; a row before any
path line or before any class section; a row whose class disagrees with its
section; a row that classifies nothing, or that claims more than the file total;
an entry under `crates/rift-shadow-redact/**` or any `fuzz` component, which the
walk never reaches; and **a reason the entropy layer itself fires on**, so a
quoted high-entropy token is refused rather than billed to the ceiling the row
exists to relieve.

**Why the duplicate refusal is whole-fence and not per section.** Two rows for
one `(path, detector)` each satisfy the `file_total` arm *individually* — each
states truthfully what the file measures — while their `classified` counts are
**summed** into the subtraction. That subtracts one file twice, and it is the
whitelist this arithmetic refuses to be: measured, a file carrying four hits
passed a ceiling of **zero** through two ordinary-looking rows, one under each
class. So the key is unique across the whole fence, a class section may be
opened only once, and the subtraction itself is **checked** rather than
saturating — a table that subtracts more hits than the tree holds is a
violation, not a floor at zero. A file whose hits would split across both
classes files the whole count under the one class it can be defended as, and
says so in the reason. One file may still appear under both classes for
*different* detectors; that is what the two-part key is for.

**Reasons carry paragraph 2's shed discipline.** The token is never quoted —
**guarded**, by the pipeline itself. Files are named by basename or by where
they sit, never by full path — **an ARGUMENT, not guarded**: measured, ordinary
repository paths sit under the entropy layer's threshold and no detector fires
on them, so nothing reddens when a reason spells one out. It still costs bytes
in a file that is inside the corpus it measures, and a reviewer is the only
thing enforcing it.

**This is a shared surface.** In the same commit as the hits it classifies, a
change may add or shrink rows only for paths that change owns in that merge, and
may never touch the ceiling fence above or another change's rows.

**An empty table is the identity** — `raw − 0` is the arithmetic that ran
before that rule, and the fence shipped empty until the two upstream-pin fixtures
below became its first rows. What is refused is an **absent** fence: a reader
that silently returns nothing when its fence has been deleted is a check that
cannot fire.

```fp-classified
# Grouped by path. A change adds rows only for paths it owns, in the commit that
# lands the hits; reasons name files by basename and never quote the token.
--- pinned-evidence
crates/rift-agent-claude-code/fixtures/claude-code-hook-surface.md
	entropy.high	1	1	pinned-evidence: the upstream pin's vendor tree path for the plugin-dev skill at a pinned vendor commit — the one line of the citation block the word-shape rule (2026-09-11) still fires on, because the commit id is not a word. The value was chosen upstream; the pin-resolution test in adapter_sweep.rs resolves it, and shortening it makes the claim claude-code-hook-surface.md commits to unverifiable
--- frozen-bytes
```

## Where each detector's shape comes from

Recorded in prose, deliberately, because a `spec` column would have to enforce
citation *shape* to be worth more than prose and that is a separate commit's
worth of grammar test. No provenance field is added to the `RedactorPack`
schema: v1 is `deny_unknown_fields`, a new key would refuse every pack written
against it, and **user packs are exempt from citation — never from the caps or
the self-tests**.

Read the third column as *what kind of authority the shape derives from*, at
document granularity. Where a shape has **no** external source, the row says so
rather than borrowing authority from a neighbouring standard; those are the rows
whose evidence is the measurement recorded in this file.

| detector | what it matches | where the shape comes from |
|---|---|---|
| `entropy.high` | any token of ≥20 characters over 4.0 bits that is not word-shaped | Shannon (1948), *A Mathematical Theory of Communication*. The threshold, the length floor, the token class and the word-shape exemption (2026-09-11) are **Rift's own**, measured here — see the classification above |
| `pii.email` | an addr-spec-shaped local part, `@`, a dotted domain | RFC 5322 addr-spec, narrowed: the shape, not the full grammar. Internationalised addresses (RFC 6531) are **not** covered |
| `pii.ipv4` | a dotted quad | RFC 791 |
| `pii.us_taxpayer_id` | a punctuated 3-2-4 digit group | SSA published allocation rules (areas never issued, group and serial exclusions) and IRS ITIN ranges, which share the shape |
| `pii.us_taxpayer_id_labelled` | nine bare digits carrying a key | the same two authorities for the number. The **label vocabulary is Rift's own**, measured against this corpus |
| `pii.phone_e164` | `+`, country code, 8–15 national digits | ITU-T E.164 |
| `pii.phone_intl_zero_prefix` | the ITU exit code in front of a grouped number | ITU-T E.164, international prefix. The group-count and digit-total gate is Rift's own, and the measurement that forced it is recorded above |
| `pii.phone_uk_national` | a trunk-prefixed national number | Ofcom, *National Telephone Numbering Plan* |
| `pii.phone_nanp` | a 3-3-4 group whose leading digits are issuable | NANP administration rules (an area code and an exchange code may not begin with 0 or 1) |
| `pii.street_address` | a number, capitalised words, a street-type word | USPS *Postal Addressing Standards*, street suffix abbreviations. **The weakest row here** and the only PII shape with neither a checksum nor an issuing authority — its cost is a parity `Divergence` row of its own |
| `pii.card_pan` | a 13–19 digit window that passes Luhn | ISO/IEC 7812-1, including its check-digit algorithm |
| `pii.card_pan_grouped` | four uniform comma-separated groups that pass Luhn | ISO/IEC 7812-1. The **grouping** requirement is Rift's own and exists because the ungrouped form measured 281 hits on this corpus |
| `uri.credentials` | a user-and-secret pair in a URL's userinfo position | RFC 3986, the userinfo component |
| `uri.credentials_empty_user` | the same with the user omitted | RFC 3986, same component |
| `dsn.password` | the value of a `password` or `pwd` key | JDBC URL and ODBC-style connection-string key/value syntax |
| `dsn.password_quoted` | the same value written in quotes | as above; the quote handling is Rift's own so the document still parses |
| `dsn.connection_string` | a whole connection string carrying a real password | as above |
| `key.pem_block` | a `BEGIN`/`END` private-key block | RFC 7468, textual encodings; the OpenSSH private-key form shares the delimiters |
| `kv.authorization_header` | an `Authorization` / `Proxy-Authorization` credential | RFC 7235, and RFC 6750 for the bearer form |
| `kv.credential_colon` | a credential key and its value | **no external source.** The vocabulary is Rift's own and was chosen by measurement: `token`, `secret` and `pwd` are excluded because they measured 44, 7 and 1 hits of ordinary Rust here |
| `provider.anthropic.api_key` | a vendor key prefix | the vendor's published key format |
| `provider.github.fine_grained_pat` | a vendor key prefix | the vendor's published token formats |
| `provider.supabase.secret_key` | a vendor key prefix | the vendor's published key format |
| `vendor.aws.access_key_id` | an access-key identifier prefix | the vendor's published unique-identifier prefixes |
| `vendor.github.pat` | a personal-access-token prefix | the vendor's published token formats |
| `vendor.google.api_key` | an API-key prefix | the vendor's published key format |
| `vendor.openai.key` | an API-key prefix | the vendor's published key format |
| `vendor.slack.token` | a token-type prefix | the vendor's published token types |
| `vendor.stripe.secret` | a secret-key prefix | the vendor's published key format |

Twenty-nine rows, which is the enforced census: the test that reads this file
requires the fence and the built-in tables to be the same set, so a detector
added without a row above is a detector added without a measured cost.

**What this table is not.** It is a record of provenance, not a guarantee of
currency: a vendor may change a prefix without telling this repository, and
nothing here reddens when they do. Six of the vendor rows say *"the vendor's
published format"* rather than naming a document, because the durable citation
for a token prefix is the prefix itself — which is in the detector — and a URL
to a vendor page is the part that rots. The rows that carry a standard's number
carry it because that number does not move.

## What the gap closures cost, measured on the same corpus

The gate's first criterion took the parity table's thirteen `Verdict::Gap`
rows from 13 to 2. Eleven closures added **twelve detectors** to the pipeline
— two credential key/value shapes, three provider prefixes, an empty-user URI, a
quoted DSN value, a whole connection string, two phone spellings, a
comma-grouped card number, and a street address.

**Four counts in this section were corrected on 2026-08-24 and the correction is
its own small lesson.** The paragraph above said *thirteen* detectors and then
listed *twelve*; the table below said 17 → 30 where the enforced census is
17 → 29. The list was right and the count was wrong, both times — which is
a standing rule in miniature ("the LIST is the deliverable, not the count").
Measured rather than reasoned before correcting: at `e2b49f4` the built-in
tables carry **16** shape detectors and at `b22d88b` **28**, the twelve added
being exactly the twelve the sentence enumerates; the entropy layer has no
`ShapeDetector` to carry it, so the census the artifact enforces is 17 → **29**
and was 29 at `c0911c3` too — the number was wrong when it was written, not
stale. `the_false_positive_baseline_prices_every_builtin_detector` has been
holding the fence to the real census all along, which is why no prose number
could redden anything.

Re-measured on the same corpus after all of them:

| | before | after |
|---|---|---|
| detectors | 17 | 29 |
| files / bytes | 493 / 7 508 629 | 493 / 7 509 375 |
| **masked spans** | **446** | **446** |
| per-detector counts | see the table above | unchanged, all new detectors at **0** |

Every one of the twelve was measured against this corpus *before* it was
written, and three candidate rules were narrowed or refused because of what the
measurement said rather than after the fact:

- adding `,` to the payment-card separator class: **0 → 281** hits on tracked
  text (73 → 361 across the whole diff history), because a comma-separated
  coordinate list forms Luhn-valid 16-digit windows. Refused; the comma is
  accepted only in a uniform four-by-four grouping, which measures **0**;
- accepting the ITU exit code `00` in front of any digit run: **1 188** hits,
  because a decision-record id and a Cargo version are exactly that shape. Refused; the
  rule requires three separated groups and 10–15 national digits, and measures
  **0**;
- the bare credential words `token` (**44** hits, all Rust type annotations),
  `secret` (**7**, same) and `pwd` (**1**, `$PWD:/repo`) are excluded from the
  credential key vocabulary for that reason.

The one cost that is not zero is time: the same 7.5 MB scan takes **15.6 s
before and 27.9 s after**, for 29 detectors instead of 17. Hit counts, the
scaling test and the placement of redaction on the detached worker (off git's
critical path, by owner decision) are all unchanged, but the constant factor is
real and is recorded here rather than left for the next performance change to
find.

## The class-tagged PII placeholder cost nothing here

An owner decision gave the PII family a placeholder per detector —
`[REDACTED:pii.email]` and its ten siblings in place of one `[REDACTED:pii]`.
That changes the bytes a redacted transcript carries, so it could in principle
change what a later detection pass sees: the pipeline runs detect→mask to a
fixpoint, and the text a second round reads is text the first round wrote.

Measured on the same corpus, the same way, immediately before and immediately
after the change — **every count identical**:

| detector | before | after |
|---|---|---|
| `entropy.high` | 459 | **459** |
| `pii.email` | 50 | **50** |
| `uri.credentials` | 4 | **4** |
| `vendor.aws.access_key_id` | 20 | **20** |
| `vendor.github.pat` | 1 | **1** |
| every other detector | 0 | **0** |

568 files / 9 627 470 bytes on both readings. The reason it is free is worth
one sentence, because it is a property and not a coincidence: the longest label
this adds is `[REDACTED:pii.us_taxpayer_id_labelled]`, whose only token over the
entropy layer's 20-byte floor is `us_taxpayer_id_labelled` — 23 bytes at 3.61
Shannon bits, under the 4.0 threshold. That is a fact about that particular
word, not about the naming scheme, so it is held by a test rather than by this
paragraph: `every_placeholder_is_a_fixpoint_and_is_delimited_the_same_way`
redacts every placeholder the crate can emit and requires it to come back
unchanged, so a twelfth PII detector whose id happens to read as high-entropy
reddens instead of shipping.

## Occupancy, and the policy on the ceiling

Read this section before adding tracked text anywhere in this repository.

**Measured 2026-08-24 on the final tree of the change that closed that review**
(base `b22d88b`; the reading at that base before that change wrote anything was 568
files / 9 627 470 bytes and the same 459):

| | |
|---|---|
| files scanned | **569** |
| bytes scanned | **9 648 963** |
| masked spans | **534** |
| `entropy.high` | **459 of a 460 ceiling — one hit of headroom** |
| `pii.email` | 50 of 60 |
| `uri.credentials` | 4 of 20 · `vendor.aws.access_key_id` 20 of 40 · `vendor.github.pat` 1 of 10 |
| every other detector | 0 |

The corpus grew from 493 files to 569 and from 7.5 MB to 9.6 MB since the
baseline was recorded, and `entropy.high` grew from 383 to 459 with it. The
headroom the ceiling was set with was **spent** at that reading.

**Then it was breached, and then it was corrected — both measured, six hours
apart, and the sequence is the whole argument for paragraph 5.** On base
`1bbd8cf` the count reached **461 of 460**: red, on trunk, with no unmerged work
on top of it. The two hits that did it were attributed per file rather than
guessed — one in the integration checkout's new baseline rows, one in a token in this
repository's own new source — and **neither was classifiable**, because both are
this repository's own text and paragraph 5's classes take neither. The
mechanism's answer was therefore *no*, which is the mechanism working. What
cleared it was that rule's shedding clause, executed on base `5f237f5`: the nine tracked
`docs/` files left the index, and the count fell to **404 of 460** — a 57-hit
correction, the figure clause 8 predicted, confirmed by this instrument
independently. That correction is **the last one available** for text a fresh
clone never contained. The headroom it returned is not a budget either.

**This file still contributes zero, and keeping it that way needed a
measurement rather than care — twice.** The detector-provenance section above
was first written with the key-equals form, its three-letter abbreviation, and
a URL-userinfo template spelled out in its "what it matches" column — ordinary
technical prose, and three false positives in this document's own corpus
(`dsn.password` 0 → **2**, `uri.credentials` 4 → **5**), each of them under
its ceiling and therefore invisible to the test. The first rewording then
re-spelled all three shapes in the paragraph describing their removal — found
by a verifier measuring the committed artifact, not by reading it; this
paragraph now names the shapes only by description. Re-measured after the
shapes were reworded into words:
back to 0 and 4. Everything that change added — that section, this one, and a new
`bins/rift` e2e — costs `entropy.high` exactly **zero**, which is the discipline
paragraph 3 below asks of every change, applied here first.

**The policy, stated because two changes hit it in one session** (one change's own
benchmark artifact took the count to 471, and a merge candidate to 464; both
were fixed by shedding, neither by moving the ceiling):

1. **The ceiling is an absolute the tree must stay under.** It is not a budget
   to be topped up when it binds. Since the subtraction rule the quantity that must stay under
   the ceiling is `raw − Σ classified`; the raw count may exceed it, and the
   classified fence is the only place that difference may live.
2. **A change sheds, or classifies under the fence above — it does not raise.**
   Classifying is not the cheap door: the two classes are closed, they are
   discriminated by externality rather than by how much an author would like to
   keep their bytes, and neither admits a change's own new or regenerable
   artifacts. Both breaches this session were closed
   inside the change's own new text — a fence that listed eighteen stems twice, a
   doc comment that spelled a full path while explaining that full paths are the
   problem, a 49-character slash-joined literal that became path segments. In
   every case nothing was lost, because `/` `_` `-` `+` `=` are token characters
   to this detector and a slash-joined path is *one* long token: naming a thing
   once, or by where it sits rather than by its full path, costs a reader nothing
   and costs this counter a hit.
3. **The census commit states the current occupancy**, which is what this section
   is, so the next change can see its margin before it writes rather than after its
   gate reds. The guard states it too: run it with `--nocapture` and it prints
   the file count, the byte count, the span total, the per-detector totals and
   this file's own contribution. Three separate instruments were built in one
   session to read a number the instrument can simply say.
4. **Raising it is an owner decision** (redaction semantics and
   invariant relaxations are owner-only). ARGUED, NOT GUARDED, and recorded here
   rather than acted on: every `entropy.high` hit over this repository's own text
   that has ever been classified by hand was a false positive — 30 of 30 in the
   sample above — so this ceiling is a *growth* guard on a layer known to be
   noisy, not a security guard, and the question of whether a growth guard should
   bind a security crate's suite is one for an owner with these numbers in front
   of them. No individual change may answer it — and none may answer it sideways by adding
   a class to the fence above, which is the same owner escalation in a different
   spelling.

   **The number is frozen.** That rule froze `entropy.high = 460` and every other
   value in the ceiling fence: they do not move, not by a change, not by an
   integrator, and not by the rule itself, which changed the arithmetic and left
   the number exactly where it was. The accommodation route for growth that
   cannot be shed is **subtraction under the classified fence**, and it is the
   only one. **Moved once, by the route this clause names**: at the worktree-family close
   review (2026-09-01) the owner granted the one-time re-derivation and the
   `entropy.high` ceiling moved 460 → 470 — after four recorded data points
   showed the count moving on sha- and path-luck alone, and after the
   census-inert recording sheds landed first (the committed tree measured 454
   at the grant). The number is frozen again at 470 on the same terms.
5. **Growth that is irreducible *and* unclassifiable escalates; it does not
   drift.** Those are two different problems and only one of them has a route
   through the fence. Agent-config files are the measured example: the five in
   this worktree carry **43** `entropy.high` hits (11 · 10 · 9 · 8 · 5), a sixth
   staged in the integration checkout carries 10 more, and the owner has ordered
   four further writers of that same shape — this record prices that at 32–44
   hits against roughly 34 of projected headroom. Product configuration is
   correctly *not* classifiable under either class, so no fence entry can
   absorb it. When shedding is exhausted and no class can be defended, the
   integrator writes an owner escalation carrying the per-file census this
   fence now makes readable, and the escalation **names what the owner may
   grant**: a third class, or a one-time re-derivation of the ceiling from a
   stated corpus. It does not move the number on its own authority, and it does
   not reduce the corpus.
6. **The instrument's corpus is worktree-dependent, so isolate before you
   attribute.** The walk takes `--cached --others --exclude-standard`: an
   untracked scratch file in a side worktree counts, and a file staged in one
   checkout is invisible in another. Demonstrated rather than argued, and bound
   to the tree it was taken on because that is the only form of this claim that
   cannot go stale: on base `ca0287f` the guard read **459** in the side
   worktree that wrote this section while the same guard recorded **469** in the
   integration checkout, and the whole
   difference is one agent-config file — staged in the one, absent from the
   other, and measured at exactly 10 hits where it exists. This record also notes an
   untracked request payload measuring 2 hits of a delta the change writing it was
   about to attribute to its own code. CI is immune; working checkouts are not. **For the
   same reason no branch delta is derived in this document**: a change's
   post-rebase re-measure governs, and a difference between two numbers taken in
   two worktrees is not a delta.

**Measured 2026-09-11 under the word-shape rule**, on the tree of the change
that added it:

| | |
|---|---|
| files scanned | **683** |
| bytes scanned | **14 482 330** |
| masked spans | **149** |
| `entropy.high` | **63 raw − 1 classified = 62 of an 80 ceiling** |
| `pii.email` | **60 of 60 — no headroom** |
| `uri.credentials` | 4 of 20 · `vendor.aws.access_key_id` 21 of 40 · `vendor.github.pat` 1 of 10 |
| every other detector | 0 |

The same tree measured 468 for `entropy.high` the morning before, so the rule
shed 405 hits without touching a byte of the corpus; the classified fence
shrank with it (one row shrunk 5 → 1, one deleted, as the guard's row arm
demanded). Note `pii.email` sitting exactly on its ceiling: that is the next
detector a change will meet, and it is the synthetic fixture addresses, not
this rule's doing.

## History

| date | tree | change |
|---|---|---|
| 2026-08-17 | `e2b49f4` | first recording: 492 files, 7 503 869 bytes, 446 spans |
| 2026-08-17 | `c0911c3` | after eleven gap closures: 29 detectors, 446 spans, every new detector at zero |
| 2026-08-24 | `b22d88b` | re-measured under the class-tagged PII placeholder: 569 files, 9 648 963 bytes, 534 spans; the class-tagged PII placeholder moves no count; `entropy.high` at 459 of 460 |
| 2026-08-24 | the subtraction rule on base `ca0287f` | the classified fence and the subtraction. 569 files, 9 660 000 bytes, 534 spans, `entropy.high` 459 of 460 — **unchanged**, because the fence starts empty and `raw − 0` is the arithmetic that ran before it |
| 2026-08-24 | the subtraction rule on base `5f237f5` | re-measured after its shedding clause untracked the nine `docs/` files: **561 files, 9 636 365 bytes, 479 spans, `entropy.high` 404 of 460**. Between the two rows trunk itself reached 461 and breached; the correction returned 57. The subtraction is still the identity — the fence carries no rows — so this number is the corpus's, not the mechanism's. **Self-cost of everything that rule added to this file: `entropy.high` = 0**, measured on 2026-08-24 by the guard's own per-file census over the committed bytes, and it survived every rebase and every rewording since. That figure is **UNGUARDED**: nothing reddens when it goes stale; an earlier grouped census recorded 26 and measured 29 that day. A row is bound to a BASE and not to a branch name, because a branch name denotes a different tree after every rebase and a reading that names one is false the moment it lands |
| 2026-08-25 | on base `e57872b` | **the first live rows.** The two upstream-pin fixtures are classified under `pinned-evidence` — claude-code-hook-surface.md 5 of 5, codex-hook-surface.rs 1 of 1, six subtracted. What fires in them was measured token-by-token before the rows were written: the retrieval URLs, the upstream repository URL and the vendor tree path — all four upstream object ids score under the entropy threshold bare, so the citation apparatus is the cost, not the ids. Census on that change's tree with the rows live: **583 files, 9 845 795 bytes, 506 spans; `entropy.high` 429 raw − 6 classified = 423 of 460; `pii.email` 52 of 60**. This file's own price for the rows is `entropy.high` **+1** — the codex group's path line fires; the longer claude-code path line measures 0 — UNGUARDED like every self-cost figure here |
| 2026-09-02 | the commit that tracked the guide | `docs/guide/` became tracked — the written guide to this toolchain now ships with the code — and the census gained its third excluded region, `docs/`, in that same commit. No revision ever measured the guide's worked examples as this repository's own text, so every ceiling, classified row and count above still describes the same set of files it described before |
| 2026-09-11 | the word-shape fix | the entropy layer skips word-shaped tokens by owner decision. Re-measured on the same 683 files / 14 482 330 bytes: **149 spans, `entropy.high` 63 raw − 1 classified = 62**, down from 468 on the same tree that morning. Ceiling re-derived 470 → **80**; the claude-code pin row shrank 5 → 1 and the codex pin row was deleted, both because the guard's row arm said so. Every other detector's count is unchanged, which is the measurement that the rule touched one layer |
