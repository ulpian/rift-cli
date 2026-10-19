# 8. Cost and savings

Rift counts tokens and money in two directions. Going **out** it records what
your agent sessions spent: four billable token classes per model call, priced
from a versioned catalog into integer micro-dollars — bookkeeping over facts the
provider reported. Going **in** it estimates what the recall plane spared you:
tokens a serve stood in for instead of a full re-read. That side is an
*estimate*, and the chapter's discipline exists because an estimate that forgets
it is one turns into a marketing number. Serving mechanics are in
[Chapter 7](07-serving-and-injection.md).

---

## 8.1 See your own numbers first

`rift dispatch` (read-only; it creates nothing and says so where recall was never
enabled) prints, among checkpoints, sessions and lines:

```console
  tokens      in 4397438 (cached 7685642568, write 244689408)  out 38783700   ~$5,533.64
  recall      110813640 tokens avoided (estimated upper bound), 52143 injected
```

Each is a different claim: the token classes as the provider reported them;
`~$5,533.64`, those classes times the catalog rate in force, an estimate marked
with a tilde; `tokens avoided`, an **estimated upper bound** on re-reads displaced;
`injected`, real tokens spent pushing manifests into prompts — spend, never credit.

---

## 8.2 Four token classes, four prices

The same thousand tokens cost wildly different amounts by which side of the call
they were on and whether the prompt cache had seen them, so each model call is
priced per class:

```text
call_cost = uncached_input · price_uncached_input
          + cached_input   · price_cached_input     (about 10x cheaper)
          + cache_creation · price_cache_write      (a one-time write premium)
          + output         · price_output           (the most expensive)
          + tool_costs
```

The example above (4.4M uncached against 7.7 **billion** cached-read tokens) shows
why: one blended rate would overstate the bill roughly tenfold. **Absent
classes are zero, not invented.** Each adapter maps what its client reports onto
the four columns verbatim (`input`, `cache_read`, `cache_write`), and a client
with no cache split persists zeros; a back-filled ratio would be indistinguishable
from a measured one in the same row. Every usage row carries a `counter_method`
(`provider_reported`, `tiktoken`, `estimator:heuristic`); the capture path writes
only `provider_reported`.

---

## 8.3 One tokenizer, one provenance tag

Every crate that counted for itself would drift: budgets charge one number,
reports publish another. So counting is one crate (`rift-tokens`), `count(text)`
and `provenance()` — `cl100k_base` when the bundled byte-pair encoder is live,
`chars/4+1` when it could not initialize. The fallback **rounds up**, so a budget
ceiling cannot be breached by an under-count, and the provenance travels beside
every number (each serve response reports its `tokenizer`). A general-purpose
encoder is *not* the provider's billing tokenizer: close, and honest about not
being it. Every estimating path counts through it; the validity plane's envelope
builder floors a count at one so an included artifact is never free.

**A read's count is taken over the bytes of the revision it recorded**: the
working file while it still hashes to that revision, else the git blob, else the
family's basis snapshot rehashed against it. A read none of the three still holds
is **unmeasured once** — no retry, nothing said by a hook — and weighs zero in
the denominator ([§8.12](#812-the-measured-baseline-repeated-and-avoidable-context)).

**The capture path counts nothing**: it records what the provider reported and,
where an adapter reports nothing, no count. The cost crate keeps a heuristic for
sizing and no counter, because a second way of producing a number that reaches a
bill is a second authority for it.

---

## 8.4 Prices are data, appended and never updated

Prices live in a table under the ledger's append-only law
([Chapter 13](13-storage-and-schema.md)), seeded by migrations. A row is keyed by
`(model, token_class)` and carries **two** time axes: `valid_from` / `valid_to`
(when was this the provider's price?) and `recorded_at` (when did we come to
believe it?). Resolution takes the latest row valid at the moment priced, among
rows recorded at or before the belief date, with a deterministic tie-break — so
"what did this call cost, according to what we knew then" stays answerable, and a
correction appends a superseding row (`supersedes_id`). A slice, dollars per
million tokens:

| Model | Uncached in | Cached in | Cache write (5 min) | Output |
|---|---|---|---|---|
| `claude-opus-5` | $5.00 | $0.50 | $6.25 | $25.00 |
| `claude-fable-5-1` | $10.00 | $0.25 | $12.50 | $50.00 |
| `claude-sonnet-5` | $2.00 | $0.20 | $2.50 | $10.00 |
| `claude-opus-5-5` | $4.00 | $0.20 | $5.00 | $20.00 |

Every class is transcribed from the provider's page, never derived from a
multiplier: `claude-fable-5-1` reads its cache at a fortieth of its input rate
and `claude-opus-5-5` at a twentieth, where the others use a tenth. There is no
one-hour cache-write class because no usage path records one.

Sonnet shows both axes: standard rows first transcribed at $3.00 / $15.00 are
corrected to $2.00 / $10.00 by rows naming the ones they supersede; the later
`recorded_at` wins, while a belief held earlier still answers $3.00 / $15.00.
Seeded rows are valid from the start of valid time, so earlier usage quotes at a
newly appended price with no rebuild.

Two rules keep it honest. **All four classes or nothing**: a partially priced
model produces no cost fact rather than one silently omitting output tokens.
**Prices are transcribed, never remembered.** A model the page does not list
(`Gemini 3.7 Flash (Low)`) has no row, and `rift doctor` names it
([§8.8](#88-putting-a-dollar-figure-on-avoided-tokens)).

---

## 8.5 From usage to a cost fact

A provider usage report writes a **model call** row, its 1:1 **token usage** row,
and a **cost fact** only if the catalog fully covers the model. The fact is
bitemporal and strict-append: an identical recomputation writes nothing, a changed
belief appends a superseding fact, so "what did we think this cost last month" and
"what do we think now" both stay on file. Reports quote captured usage against
the latest catalog belief at read time, using each call's start as valid time,
counting each call once — a newly appended price prices old unpriced usage
without a cost fact or a rebuild. A session with no completely priced call has no
dollar figure; a priced call with zero usage has a zero one.

Money is **integer micro-dollars** end to end: a 64-bit integer through an
overflow-safe 128-bit intermediate, fractions below one micro-dollar truncated per
class. Floating point never touches a currency value; formatting happens at the
edge.

---

## 8.6 What "avoided" actually measures

Every serve reports two figures from one serialization of the payload at the
single accounting seam of
[§7.12](07-serving-and-injection.md#712-one-measure-one-charge-one-sealed-event),
so no handler can hand-write either: `estimated_tokens_returned`, a
**measurement**, and `estimated_tokens_avoided`, an **estimate**, `baseline −
returned`, saturating at zero. The baseline is the ingest-time read cost of a file
only when the caller stated its revision, the checkout still holds it and this
family recorded reading it; zero when moved or stale, and zero for
`retrieve-exact`, refusals, invalidations and ranked `changed-hunks`. Documents
credit a declared native cost, a declined read the real count of the slice it would
have delivered, a **prevented** read the real count of the lines Rift announced.

A pull that states no hash books **zero**: a manifest over two hundred candidates is
a list, not two hundred spared reads. The caller's word alone is refused (a
published figure rests on what Rift recorded, not text an agent wrote) and so is
the projection's alone (it trails every edit since the last checkpoint). Each
response names its rule as `avoided_basis` (`stated_recorded`, `none_stated`,
`stated_unrecorded`, `unverified`, `declared`, `not_credited`); a pull that stated
hashes writes a `memd_audit_pull_claim` row, printed as `pull claims:`. A pull is
still a **claim**, since nothing it displaced is a recorded read and a share over it
would divide by its own numerator: pulls never enter the share
([§8.7](#87-the-honesty-ladder)) and are published **beside** it as `claimed by
pulls`. **Avoided is net**, because a gross baseline double-counts against any
denominator, and **injection is spend with no credit**: a manifest pushed unasked
carries only a returned figure.

### A read that never happened

When Rift tells a window it holds a file and the window acts on it without
re-reading, the announcement replaced a read. It is booked **only on evidence a
hook can see**: a manifest or recovery-map entry the frame carried, in the window's
own ledger ([§6.12](06-recall.md#612-what-a-context-window-already-holds)), then a
tool call on the file; prose is invisible to a hook. It is booked **once per
announcement**; a **read answers it** (including one the gate decided against and
let through, since `observe` names what Rift did, never what the tool delivered); a
**decline answers it**; and **the revision must still match**. A slice is booked
once, under whichever verb reached it first: booked first, a later read is still
declined (decline mechanics: [Chapter 7](07-serving-and-injection.md)) but books
zero under `prevented_first` once the runs *share a line*, else one revision is
charged twice. The row `memd.read_prevented` carries no returned figure (the frame
is charged where injected) and sits *inside* the avoided total.

A slice whose bytes moved books **zero** with a tag, never a figure imputed under new
bytes. A document's baseline is **declared** (office and container formats at the
tokens their bytes would cost the reading tool; a PDF at **zero**, its native cost
being a page count against a vision rate nobody pinned), so what it saves is
excluded from the share rather than estimated into it. An answered search is
declared at **the ceiling its own client documents**: a hundred files for a glob,
about thirty thousand characters for a shell listing, the latter stated by one of
three clients and applied to the others, an assumption
([Chapter 18](18-what-rift-does-not-do.md)). Paths the repository refuses to name
leave the listing *before* the cut; a content search declares nothing.

### The denominator: repository content on any channel

A share divides avoided by *avoided + read*, and the second term decides whether the
first means anything. A search returns repository content on every call, so the
denominator is file reads **plus** measured search results
([§7.7](07-serving-and-injection.md#77-searches-are-bounded-and-sometimes-answered));
searches were 18.7 % of the pool in one recorded replay. `rift savings --json`
carries `channels` (`read_tool`, `shell_read`, `search`), which **sums to
`read_tokens` exactly**, and `reads_unmeasured`, a count of calls, not in the sum. A
figure nobody could compute is `null`, not zero. Each measured row records its
`basis`, `reexecuted` or `in_process`, so a reconstruction is never averaged into a
measurement unlabeled.

---

## 8.7 The honesty ladder

Each rung claims strictly less than the one below, and no surface promotes a figure
up a rung.

1. **Measured**: tokens returned, provider-reported classes, catalog cost facts; a
   read counted over the bytes delivered, a search counted by running it again.
2. **Estimated upper bound**: tokens avoided, **counterfactual-free** (what
   re-reading last cost, not whether the agent would have). The rule is **never
   impute without evidence**: a prevented read is imputed, but on two recorded facts
   (announcement and act); a frame nobody was measured receiving, prose, or a moved
   revision is not. The report prints `tokens avoided (estimated upper bound)` and
   JSON puts the caveat in the key, `tokens_avoided_estimated_upper_bound`.
3. **An estimate over an estimate**: the dollar figure, a rung-2 count times a rate
   for a model the trail never recorded, always with a tilde
   ([§8.8](#88-putting-a-dollar-figure-on-avoided-tokens)).
4. **Absent**: no honest basis shows nothing (`n/a`, `null`), never a zero.

### Every rate names its denominator

The **savings share**, `avoided ÷ (avoided + read)` since the last window boundary,
is the rate to quote: the numerator is the audit relation's avoided tokens less the
pulls' claims, the denominator adds the read tokens the capture projection recorded
in the same window. `rift recap --json` publishes
`rate_percent_of_avoided_plus_spent`, avoided against everything spent. Avoided over
avoided plus returned is published nowhere: it asks a different question in the
share's on-screen shape.

**The share counts only decisions the family's reads can denominate**, the hook
channel's. Counting pulls inside it let one verb book millions of avoided tokens on
a few thousand returned and made the headline read two thirds where the measured
share was about one percent. One function serves every surface and publishes a
variant dropping families with decisions and no captured reads, naming them, since
a family never observed reading divides by its own numerator and prints 100 %. A
recognised shell read counts as a read
([§3.5](03-capture.md#a-read-is-a-tool-call-or-a-command-line)). Two properties are
**published, not papered over**: decisions are cut by `memd_audit_event.recorded_at`
and reads by `tool_call.started_at`, so a decision and its read can straddle a
boundary (every surface prints its window); and an unfrozen read weighs zero, so
the share is over fewer measured reads than `n` suggests (§8.12).

**The product declares no target**: the only threshold is the evaluation bar
([§8.13](#813-the-bar-is-what-the-harness-measured)). **The window boundary is a
file**: `rift savings reset` writes `<common_dir>/rift-family/savings-window.json`,
the share counts what was recorded after it, `--at` places it (a boundary after now
is refused, since it would exclude the sessions it opened to measure), and linked
worktrees share it. Nothing is deleted; removing the file restores the whole
record. The reread report is the whole-record exception and says so.

The **prevented figure** is the part of the avoided total that is reads that never
happened. The **honesty figure**, `avoided − returned`, is the per-class test: a
lever whose frames and sidecars cost more than the reads they displaced is not a
saving, and a failing class is turned off by data. Bounding a content search books
no avoided tokens, so that lever publishes the **re-query rate** (bounded searches
asked again in the same turn), since a bound worked around immediately saved nothing
([§7.7](07-serving-and-injection.md#77-searches-are-bounded-and-sometimes-answered)).
`dogfood --compare` ([§16.14](16-development.md#1614-a-savings-reading-through-the-real-clients))
checks a booked figure against a removal over scripted sessions, lever off and on;
the arms are separate non-deterministic runs, so it is a size check, not a
replacement.

---

## 8.8 Putting a dollar figure on avoided tokens

The audit trail has **no model and no token-class column**, the catalog is keyed
by the model on a captured call, and the projections share no key: any price
applied is a chosen assumption, to choose defensibly and disclose.

**The class basis: uncached input.** A re-read is fresh input: as output the
tokens would overstate about five times, as cached input understate about ten.

**The model split: a disclosed partition.** The avoided total is partitioned
across the models the project ran, weighted by each model's observed share of
input tokens, each slice at that model's uncached rate; largest-remainder
rounding makes the slices add to the total exactly. Pricing the same tokens at several models would count one saving twice.

**Rendering rules**: a leading `~` on **every** derived amount, none on a
per-million *rate* (a recorded price); a positive saving under a cent as
`~<$0.01`, never `$0.00`; no amount for an unpriced model; no animation, because
a tweening cost figure reads as changing.

**Naming what the figure leaves out.** A model with no price adds usage and no
dollars, so `rift doctor` under `prices` names each such model with its call
count. It writes nothing; the remedy is a seeded price.

---

## 8.9 The degradation ladder

The dollar estimate degrades down four rungs. Every model priced: the full
breakdown, the headline the sum of rows. Some unpriced: those appear with their
token slice and a *no seeded price* marker, and the modal states how many tokens
are unpriced. No usable input-token mix: the most frequently captured model
carries the total as one disclosed row (a session's primary-model field is not
used; it measured empty in every captured session). Nothing resolvable: no
dollar segment, tokens and share alone. The cross-projection read is read-only and
*any* failure lands on that rung, so a pricing problem cannot break the dashboard.

**The mix is cut where the tokens it prices are**: the card's own window, since
all history would attribute this week's tokens to a model the project stopped
running. A period with no model call
lands on the bottom rung. So does a seeded rate of exactly `0`: a zero price is a
missing price, and `~$0.00` beside a real saving reads as a measurement.

---

## 8.10 The savings headline in Canopy

The Recall headline reads `saved 110.8M tokens · 1.4% · ~$803.11`: tokens avoided,
**the share of [§8.7](#87-the-honesty-ladder)**, then the dollar estimate.
Everything on the card comes from one window resolution — the period button's
trailing cut composed with the family's boundary marker, handed to the
`savings::compose` the command line calls — so the headline, the two bars (read
and avoided tokens, one scale), the serve count and the modal cannot cover
different periods. Dollars reach the card through the headline only; what pulls claimed is in none
of them, and the modal draws it on its own line.

Any cell of the headline opens a read-only breakdown with the basis in full: one
row per model summing to the total, a *by worktree* line saying the figure is the
family's, the share block (window, `n`, unmeasured reads, per-family rows) and the
note that `~` marks an estimate, not a bill. **No honest figure means no segment**: no percentage for an unanswerable
share, no dollars for an unpriced window — not greyed, not `0 %`, not `~$0.00`.
**A serve carries no session key**, so the total is not split by worktree on a
guess ([Chapter 9](09-worktrees-and-families.md)). `S` with the graphs column
focused opens the same breakdown ([Chapter 11](11-canopy-tour.md) walks the panels).

---

## 8.11 The same numbers from the command line

- `rift dispatch [--json]`: §8.1's report, caveat in the key names.
- `rift savings [--json]`: the share overall and per family, honesty figure,
  unweighed reads, window and the excluding variant; it says so when a family has
  decisions and no reads instead of printing a 100 %. Beside it `claimed by pulls:`,
  `first contact:` and `family held:`, tags read back out of the audit relation so a
  lever can be taken out of the totals (a family-held net includes what was read of
  those bytes afterwards, as tokens returned, never a negative saving).
- `tool_output` in `rift savings --json` carries two classes, `tool_output_held` and
  `tool_output_folded`.
  Each splits into `would_save`, the results the class only observed, and
  `realised`, the results it acted on.
  `would_save` is beside the share and never added to it; its bytes read `null`,
  because the audit relation keeps the row but not the byte figure.
  `realised` tokens are already inside the share, so the key names which part of it
  the class is and adds nothing.
  The Canopy savings modal draws the same two classes under the share, and
  `xtask replay-savings` reports the held class as the same-window repeats it would
  hold and the folded class as not replayed.
- `--since <rfc3339|epoch-ms>` and `--last 24h|7d|30d|all`: one window per call,
  mutually exclusive, composed with the boundary marker (the later cut) so `[7d]` on
  the card and `--last 7d` are one window; an unknown spelling is refused. Neither
  writes. `rift savings reset [--at …]` writes only the marker.
- `rift recap --since <when> [--until <when>] [--model] --json`: tokens, cost,
  tool mix, serve calls, the split and the `savings` block. Bounds are a UTC day or
  epoch milliseconds, both inclusive, so `--since D --until D` is all of day `D`;
  no ledger reads `no ledger in this repository`, never a zero.
- `rift-memd metrics`: §8.12's baseline; it also **ingests**.
- `rift-memd rebuild` sets the recall projection aside under a dated name, since a
  serve made at a terminal is in no checkpoint; `rift backfill --recall-from <file>
  [--dry-run]` reads those serves back
  ([§13.12](13-storage-and-schema.md#1312-rebuilding-and-what-a-rebuild-cannot-restore)).

---

## 8.12 The measured baseline: repeated and avoidable context

Before optimizing, Rift measures how much spend was avoidable. `repeated_context_rate`
is input tokens spent re-reading a `path@hash` already seen; `avoidable_context_rate`
adds reads of content identical to the current worktree, since a delta could have
stood in. Both divide by total input tokens over in-scope reads
([Chapter 6](06-recall.md)). On this repository: 0.1899 and 0.3370, i.e. 262,000
repeated and 465,060 avoidable of 1,380,000 input tokens over eight sessions,
**33.7 % avoidable context**, which the bar of
[§8.13](#813-the-bar-is-what-the-harness-measured) was calibrated against.

A live rate is hostage to how much reading a window did, so each lever is also
measured on a **deterministic fixture through the shipped binary**: pool (cost with
the lever off) against what the agent received. **Re-reads**: 106,221 tokens, 6,438
received, **93.94 %** removed; at small-file size **42.64 %**, held to no bar,
because it prices the floor (a reason frame is a large fraction of a small read).
**Documents**: **90.94 %** of the priced pool (107,784 against 9,761 tokens).
**Searches**: seven name searches over a generated five-thousand-path repository at
each client's documented ceiling, published per lever, family and search, since a
glob answer is capped by arithmetic (a head of eight against a listing cut at a
hundred) and a recursive listing is not. Each is the pessimistic reading where a
choice existed.

**Reads whose count was never frozen** would weigh zero, so the ledger resolves what
it can, verifying each candidate against the recorded hash *before* counting (a
sibling's figure for the same hash, or the worktree or a recorded blob whose bytes
verify; the latter two need the real encoder, since characters-over-four figures
are worse than absence). Two rebuilds may differ in **coverage**, never **value**.
The remainder is a weightless row counted as `reads_unmeasured`; imputing it is the
imputation without evidence the ladder forbids, and dropping it would change what a
read *is*.

---

## 8.13 The bar is what the harness measured

A measured share and a gate are different numbers; conflating them fails builds for
work nobody has done. **The share** is the repository-content tokens an agent would
otherwise read, any channel, **first reads included**, that the plane returned
instead; it has no target.

**The bar** is `SAVINGS_BAR_PCT` (`rift-recall-ledger`, **15**): the median savings
the paired replays measured, raised only after the harness measures a higher median
and **never above what it last measured**, because a bar set to an aspiration is no
evidence. The go/no-go gate reads the constant; 15 was calibrated against §8.12's
33.7 %.

`cargo run -p xtask -- replay-savings` is neither: it replays recorded transcripts
through this build's read gate over the share's denominator, the field measurement,
never read against the bar since its denominator carries first reads and searches
no gate can decline. The tracked run in `benchmarks/replay-savings/` (421 context
windows, 58,942 tool calls, 4,736,226 repository-content tokens) declines **0.58 %**
of the content (**0.51 %** net of reason frames); 28.54 % is a first sight of a
revision inside its window, 11.50 % a first sight a sibling window read before.

The harness replays each scenario twice, full-read against a delta-served run. **Go**
needs median token and cost savings to clear the bar, success, tests and the
human-modification outcome **not degraded**, and latency within 16 deterministic
steps; **NoGo** names every failing dimension (`token_savings`, `cost_savings`,
`success_degraded`, `tests_degraded`, `human_mod_degraded`, `latency_envelope`).
Non-degradation is the non-negotiable half: a cheaper-but-worse agent is not a win,
and the percentage lives in a *versioned* thresholds value so moving it is a
reviewable edit. The delta leg is a real serve (the dispatcher in-process, the claim
from real audit events, not stamped by the harness); latency is steps, so no clock
enters ([Chapter 15](15-benchmarks.md) measures wall time). Each claim is compared
with `baseline − delta` within ±5 %, and a **seeded regression** whose delta leg
re-reads the world must return NoGo at exactly 0 %, because a gate never seen to
fail is no evidence.

Committed snapshots make a verdict change a reviewed diff. The served condition's
four tasks spend 543 to 595 tokens against baselines of 11,064 to 44,214: P10
95.17 % | P50 96.99 % | P90 98.34 %, GO. The intercepted condition (declines and delta
frames) saves P50 **67.68 %**, GO, lower because a decline returns a frame where a
pack returns nothing. Both are **over a synthetic corpus of small fixture files**:
evidence the machinery works and the gate is calibrated, not a promise about your
repository, whose number is §8.12's. What these numbers do not cover is in
[Chapter 18](18-what-rift-does-not-do.md).

---

## Related chapters

[Chapter 7 — Serving and injection](07-serving-and-injection.md) ·
[Chapter 6 — Recall](06-recall.md) ·
[Chapter 3 — Capture](03-capture.md) ·
[Chapter 13 — Storage and schema](13-storage-and-schema.md) ·
[Chapter 11 — Canopy tour](11-canopy-tour.md) ·
[Chapter 15 — Benchmarks](15-benchmarks.md) ·
[Chapter 18 — What Rift does not do](18-what-rift-does-not-do.md)
