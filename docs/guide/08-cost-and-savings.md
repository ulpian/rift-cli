# 8. Cost and savings

Rift counts tokens and money in two directions. Going **out** it records what
your agent sessions spent: four billable token classes per model call, priced
from a versioned catalog into integer micro-dollars — bookkeeping over facts the
provider reported. Going **in** it estimates what the recall plane spared you:
tokens a serve stood in for instead of a full re-read. That side is an
*estimate*, and this chapter's discipline exists because an estimate that
forgets it is one turns into a marketing number. The serving mechanics behind
the figures are in [Chapter 7](07-serving-and-injection.md).

---

## 8.1 See your own numbers first

If you have captured sessions here, one read-only command prints the picture:

```console
$ rift dispatch
dispatch  (open) → (open)
  checkpoints 182 (133 timed from their sessions)   sessions 86 (68 open)
  worktrees   main, feature-a, unknown
  lines       AI 759  MX 0  HU 7440   confidence 12.5%
  tokens      in 4397438 (cached 7685642568, write 244689408)  out 38783700   ~$5,533.64
  recall      110813640 tokens avoided (estimated upper bound), 52143 injected
```

Each is a different kind of claim. `in` / `cached` / `write` /
`out` are four separately-billed token classes as the provider reported them;
`~$5,533.64` is those classes times the catalog rate in force at the time, an
estimate marked with a tilde; `110813640 tokens avoided` is an **estimated upper
bound** on re-reads the recall plane displaced; and `52143 injected` is real
tokens spent pushing manifests into prompts — spend, never credit.

> Tip: `rift dispatch` and `rift recap` open every database read-only and create
> nothing — run them where recall was never enabled and they say so rather than
> bootstrapping a projection.

---

## 8.2 Four token classes, four prices

A modern model bill is not one number times one rate: the same thousand tokens
cost wildly different amounts by which side of the call they were on and whether
the prompt cache had seen them. Rift records four classes per model
call and prices each separately:

```text
call_cost = uncached_input · price_uncached_input
          + cached_input   · price_cached_input     (about 10x cheaper)
          + cache_creation · price_cache_write      (a one-time write premium)
          + output         · price_output           (the most expensive)
          + tool_costs
```

`uncached_input` is fresh input at the full rate, `cached_input` comes from the
provider's prompt cache, `cache_creation` is written *into* that cache at a
one-time premium, `output` is what the model generated. The `rift
dispatch` line above shows why the split matters: 4.4M uncached input tokens
against 7.7 **billion** cached-read tokens, so pricing them all at the uncached rate
would overstate the bill roughly tenfold. A blended rate is a wrong answer.

**Absent classes are zero, not invented.** Each adapter maps whatever its client
reports onto the four columns verbatim — `input` to `uncached_input`,
`cache_read` to `cached_input`, `cache_write` to `cache_creation` — and a client reporting no
cache split persists zeros there, provenance still provider-reported. Rift never back-fills a plausible cache ratio, because a
fabricated split would be indistinguishable from a measured one in the same
row.

Every usage row carries a `counter_method`: `provider_reported` (the provider's
own count), `tiktoken` (a real byte-pair count), or `estimator:heuristic` (the
offline floor). In practice the capture path writes only
provider-reported counts — see [§8.3](#83-one-tokenizer-one-provenance-tag).

---

## 8.3 One tokenizer, one provenance tag

Any figure Rift *estimates* rather than receives has to be counted somehow, and
every crate would otherwise grow its own counter, which drifts: budgets charge
one number, reports publish another. So counting is one small crate exposing
`count(text) -> u32` and `provenance() -> &str` for the basis those counts have
— `cl100k_base` when the bundled byte-pair encoder is live, `chars/4+1` when it
could not initialize.

The fallback deliberately **rounds up**, so a budget ceiling can never be
breached by an under-count, and provenance travels beside the number — every
serve response reports its `tokenizer`, every report states its basis — so a
figure is never presented as more exact than it is. It is also why the money
side hedges: a byte-pair count with the general-purpose encoder is *not* the
provider's billing tokenizer. Close, and honest about not being it.

**Who counts through it today.** The serving dispatcher (measuring every
response payload at the accounting seam), the pack compiler (budgeting and
re-measuring rendered packs), the delta ledger's changed-hunk budgeting, and the
ingest-time backfill of
[§8.12](#812-the-measured-baseline-repeated-and-avoidable-context).

**One place that does not.** The validity crate sizes its internal trials with
a local characters-over-four estimate
(`crates/rift-recall-validity/src/envelope.rs:247-251`). It affects no published
figure — those trials never charge, and what they shape is measured again at
the seam — but "one counter everywhere" is the design, not yet the code.

**The capture path counts nothing**, which is the other half of the same
discipline. It records what the provider reported and, where an adapter reports
nothing, records no count rather than an estimated one
(`crates/rift-shadow/src/project.rs:444-463`). The cost crate keeps a heuristic
for sizing decisions and no token counter of its own, because a second way of
producing a number that reaches a bill is a second authority for that number.

---

## 8.4 Prices are data, appended and never updated

Prices live in a table, not constants, and it obeys the ledger's append-only law
([Chapter 13](13-storage-and-schema.md)). A row is keyed by
`(model, token_class)` and carries **two** time axes: `valid_from` / `valid_to`
(valid time — when was this the provider's price?) and `recorded_at`
(transaction time — when did we come to believe it?). Resolving a price means
the latest row whose `valid_from` is at or before the moment being priced and
whose `valid_to` has not passed, among rows recorded at or before the belief
date, with a deterministic tie-break — so "what did this call cost, according to
what we knew then" stays answerable forever, and a correction appends a
superseding row rather than rewriting what it corrects.

The catalog seeds the models the adapters report and the models observed in captured calls.
A slice of the seed, in dollars per million tokens:

| Model | Uncached in | Cached in | Cache write (5 min) | Output |
|---|---|---|---|---|
| `claude-opus-5` | $5.00 | $0.50 | $6.25 | $25.00 |
| `claude-fable-5-1` | $10.00 | $0.25 | $12.50 | $50.00 |
| `claude-sonnet-5` | $2.00 | $0.20 | $2.50 | $10.00 |

Every class is transcribed from the provider's page, rather than derived from a multiplier: `claude-fable-5-1` reads its cache at a fortieth of its input rate, where the other two models use a tenth.
The one-hour cache-write class is outside this seed because the usage path records no one-hour writes.

The Sonnet rows show both time axes working.
Its introductory and standard epochs have adjoining valid-time ranges.
The original standard rows recorded $3.00 / $15.00; four later rows correct them to $2.00 / $10.00 for the same range, each naming the row it supersedes.
Resolution prefers the later `recorded_at`, so the current belief prices both epochs at $2.00 / $10.00, while a belief held before the correction still prices the standard epoch at $3.00 / $15.00.
Nothing is updated.

A model the page does not list gets no row; `rift doctor` names it instead (below).
Neither `claude-opus-5-5` nor `Gemini 3.7 Flash (Low)` has a seeded price.

Two rules keep the catalog honest. **All four classes or nothing**: resolution
returns a result only when every class has a row, so a partially-priced model
produces no cost fact rather than one silently omitting output tokens. And
**prices are transcribed, never remembered**, from the provider's published
pricing at the time of writing, with the effective date recorded.

> Tip: an unfamiliar model showing tokens but no dollars is not a bug — nobody
> seeded a price for it, and the breakdown modal
> ([§8.10](#810-the-savings-headline-in-canopy)) says how many tokens are affected.
> `rift doctor`'s `prices` line names every such model and its call count.

---

## 8.5 From usage to a cost fact

When a captured event carries a provider usage report, the capture projection
writes a **model call** row anchoring the call to its turn and model, its 1:1
**token usage** row (four classes plus counter provenance), and a **cost fact**
*only* if the catalog fully covers that model then. The cost fact is
bitemporal and strict-append: recomputing an identical amount writes nothing,
while a changed belief — a corrected price, a re-derivation — appends a
superseding fact. Nothing is updated in place, so "what did we think this cost
last month, and what do we think now" has two answers, both on file.

Current reports quote the captured usage against the latest catalog belief at read time, using each call's start as the price's valid time.
They count each call once, so an old booking and a correction never add together.
A newly appended catalog can price previously unpriced usage immediately without writing a cost fact or rebuilding the projection.
A session with no completely priced calls has no dollar figure; a completely priced call with zero usage has a zero figure.
Booked facts remain historical beliefs: a correction supersedes the applicable visible predecessor, even when both were recorded at the same instant.
Recomputing that correction writes nothing, and a same-instant reversion gets a distinct identity linked to the correction.

Money is **integer micro-dollars** end to end — a millionth of a dollar in a
64-bit integer, calculated through an overflow-safe 128-bit intermediate.
Each token class truncates fractions smaller than one micro-dollar, and amounts outside the final range are clamped.
Floating point never touches a currency value;
formatting to a string happens only at the edge, where a human reads it.

> Tip: if a session shows tokens but its cost stays flat, check the model name
> against the price catalog — honest absence looks like a bug until you know the
> rule.

---

## 8.6 What "avoided" actually measures

Every recall serve reports two figures, both produced at the single accounting
seam of
[§7.12](07-serving-and-injection.md#712-one-measure-one-charge-one-sealed-event),
which measures, charges and seals them from one serialization of the payload. No
handler can hand-write either.

`estimated_tokens_returned` is a **measurement**, the token count of the payload
that shipped; `estimated_tokens_avoided` is an **estimate**, `baseline −
returned`, saturating at zero. Everything rests on `baseline`, which differs by
verb:

| Verb | Baseline it stands in for |
|---|---|
| `known-state` | ingest-time read cost summed over the ranked candidates the caller stated, that the checkout still holds, and that this family recorded reading |
| `diff-since` | the same sum over the loaded candidate set |
| `changed-hunks` against a held revision | the cost of the file on disk, only when this family recorded reading the revision the caller holds; the lines returned leave through `returned` |
| `changed-hunks`, ranked | zero — it answers about no file the caller said it held |
| `file-if-changed` | that file's read cost when it is stated, held by the checkout, and recorded as read; zero when it moved |
| `read-summary` | the bound file's read cost when the binding matches the file on disk, stated and recorded as read; zero when stale |
| `context-pack` | the `known-state` sum over the ranked candidates, plus the current cost of each changed file whose older revision the caller stated and this family recorded reading |
| `retrieve-exact` | zero — you asked for the bytes; nothing was displaced |
| `doc-read` | the declared native cost of reading that document unconverted |
| A declined read | the real token count of the slice the read would have delivered |
| A **prevented** read | the real token count of the lines Rift announced, at the revision it announced them for |
| A substituted document read | the declared native cost of the document, against the sidecar as returned |
| Refusals and invalidations | zero |

A pull that states no hash books **zero**.
A manifest over two hundred candidates is a list, not two hundred spared reads: the only file a pull demonstrably spared is one the caller said it held, that the checkout still holds, and that this family is recorded as having read.
The caller's word alone is not enough, because a figure Rift publishes rests on what Rift recorded, not on text an agent wrote, and the projection's word is not enough either, because it trails every edit made since the last checkpoint.
The record is one query per call against the recall projection, and the checkout is one read of each file a claim is made about, both in the process that serves the pull, never on a hook decision's path.
A changed file earns nothing on a list answer, which returns no lines and leaves the read to do; on `changed-hunks` and `context-pack`, which carry its changed lines, it earns its current cost less those lines, and only against an older revision the caller stated and this family recorded reading.
A stated file the checkout holds but no read recorded is still answered as unchanged, and its entry carries `"avoided_basis": "stated_unrecorded"` so the withheld credit is visible; a stated file the checkout no longer holds carries `worktree_sha256`, and one it cannot read carries `unverified` with the reason.
Each response names the rule it was booked under as `avoided_basis` beside the figure: `stated_recorded`, `none_stated`, `unverified` (hashes stated, but no checkout to check them against, so the answer is the projection's view and books zero), `declared` (a document or a path listing), or `not_credited` (exact retrieval, the cost view, the ranked hunk shape).
Audit rows written before this rule keep the figures they were booked with; the relation is append-only and nothing is re-booked.

Two properties follow. **Avoided is net, not gross**: the figure subtracts what
the response cost, since publishing the gross baseline would double-count
against the denominator of any savings percentage from the same rows. That is
also why a delta's `provenance` object
([§7.9](07-serving-and-injection.md#79-the-pull-surface-ten-verbs)) needs no
special accounting: it is serialized into the payload the seam measures, so it
raises `returned` and lowers `avoided` by exactly what it cost, and a turn too
tight to carry it drops it before it drops a delta.
And **injection is spend with no credit**: a manifest pushed into a prompt
unasked is audited under its own verb and carries only a returned figure, the
event type having no field for an avoided one.

### A read that never happened

The declined read is the read Rift *answered*. There is a second kind, and
until it was booked the whole of what an injected frame is worth sat outside
every figure here: Rift tells a window it holds a file, and the window then
**acts on that file without reading it again** — edits it, or searches inside
it. No call was declined, so nothing was returned; what was avoided is the read
the announcement replaced.

It is booked because the owner's rule is that a saving Rift caused is a saving
whoever caused it — and it is booked **only on evidence a hook can see**. The
announcement is a manifest or recovery-map entry the frame actually carried,
recorded in the window's own ledger
([§6.12](06-recall.md#612-what-a-context-window-already-holds)); the act is a
tool call. A citation of the file in the model's prose is none of Rift's
business: no family reports prose to a hook, so nothing claims one happened.
Four rules bound it, and each closes a way it could be counted twice or not at
all. **Once per announcement**: a second act on the same one books nothing.
**A read answers it**: the bytes came back, so nothing was avoided — and a
re-read the gate decided against and let through answers it too, because
`observe` names what Rift did and never what the tool delivered: that call
ran, and the lines arrived. **A decline answers it as well**: the intercept
has already booked that slice. And **the revision must still match**: a file
that has moved on is not the file the window was told about.

The third rule is really a rule about **order**, and it has to hold in both
directions. A slice is worth one read, and it is booked once under whichever
verb reached it first. Declined first: the decline books the slice and
answers the announcement, so no booking follows it. Booked first: the read
that follows is still declined — the model is still told what it holds, and
the reason it was sent is still charged — but its avoided figure is a zero
under the tag `prevented_first`, which names the row that has the slice. It
is a zero as soon as the two runs *share a line*, not only when the booking
covers the whole of what was asked for: a decline books the run it was asked
for or nothing at all, so a wider read over an already-booked slice would
charge one revision twice for the lines they have in common. A file both
intercepted and prevented in one window is one saving, never two. A booking
stands once made: if the window later reads the file on a call nothing
declined, that read is paid for and recorded as a read, and the earlier
booking is not withdrawn — it measured the read the announcement spared at
the time, not a promise about the rest of the window.

The row is its own verb, `memd.read_prevented`, carries no returned figure at
all — the frame that made the read unnecessary is charged where it was
injected, and charging it twice would net one spend against two savings — and
is reported beside the measured intercepts in every breakdown while remaining
*inside* the avoided total, never added to it.

The last four rows are the ones the levers of
[chapter 7](07-serving-and-injection.md) produce, and they are counted after
the fact rather than on the hook path: the gate journals what it decided, and
the detached worker counts the slice and the frame with the real tokenizer.
Two rules keep that detour honest. A decision whose slice cannot be counted —
the file's bytes have moved on since — books **zero** avoided with a tag saying
so, never a figure imputed under the new bytes. And a document's baseline is
**declared**, not measured: office and container formats are priced as the
tokens their bytes would have cost the reading tool, while a PDF's native cost
is a page count against a vision rate nobody has pinned, so its declared
baseline is **zero** and every token it saves is excluded from the share rather
than estimated into it. Declared figures are marked as such wherever they are
published.

An answered search's baseline is declared for the same reason and priced the
same way, **at the ceiling its own shape's client documents** rather than at
one figure for both shapes. The two shapes are cut off by different things: a
glob tool stops itself after the hundred files its page states, while a shell
listing prints every match and is cut only where the client stops reading a
command's result inline — about thirty thousand characters, which is many
times a glob's worth of paths. So the paths are laid out one per line and cut
at whichever bound applies, and the figure is the tokenizer's count of what is
left. Only one of the three clients states the character ceiling; the other two
are priced at the same figure, so that one number describes a listing across
all three, which is a stated assumption about them rather than something read
off their own pages
([Chapter 18](18-known-gaps.md)). Every path the repository refuses to name — its
denied list, its sensitive list, a name that is not structurally safe — leaves
the listing *before* the cut, so a name Rift will not say is never a token Rift
claims to have saved. A content search declares nothing at all: its native
output is every matching line of every matching file, nothing in the tree
bounds it, and zero is the only honest answer.

The word "estimated" is load-bearing. `baseline` is the sum of **ingest-time
read costs** — what the agent's earlier reads actually cost, as recorded when
the session was captured. It is not a prediction of what a re-read would cost
now, nor evidence that a re-read was going to happen. Hence the standing rule in
the next section.

### The denominator: repository content on any channel

Every share on this page divides avoided tokens by *avoided + read*, and the
second term is the one that decides whether the first means anything. It used
to be **read-shaped calls**: the query joined `tool_reads_file`, so a call had
to resolve to a file to be in it at all.

A search does not resolve to a file, and it returns repository content on
every call. On the owner's own replay that was 324 calls and 237,978 tokens —
18.7 % of the pool the levers act on — outside every published figure. So the
denominator is now **repository content on any channel**: file reads as
before, plus the measured results of searches
([§7.7](07-serving-and-injection.md#77-searches-are-bounded-and-sometimes-answered)).
It is wider, so every share is smaller, and the figures published before this
landed were the share of a smaller pool than the one being worked on.

`rift savings --json` says which channel a token came from and how much of the
pool nobody could measure:

```json
"overall": {
  "share_pct": 3.1,
  "tokens_avoided": 41230,
  "tokens_prevented": 8800,
  "tokens_returned": 5120,
  "read_tokens": 1289430,
  "channels": { "read_tool": 812300, "shell_read": 239150, "search": 237980 },
  "reads_unmeasured": 26,
  "honesty": 36110,
  "sessions_with_reads": 14,
  "relation": "memd_audit_event"
}
```

Three rules keep that block honest. **`channels` sums to `read_tokens`
exactly**, so a reader who distrusts one channel can subtract it rather than
having to distrust the total. **`reads_unmeasured` is a count of calls, not of
tokens**, and is therefore *not* in the sum: it says how much of the pool is
missing from the denominator — a search whose composition no argv can run, a
tree that moved before the worker got to it — so its absence is visible rather
than assumed. And **a figure nobody could compute is absent, not zero**: the
split between the read tool and the shell is made from the families' own
pinned tool tables, and a surface that does not hold them publishes `null` for
both halves rather than a zero that reads like a measurement.

Each measured row also records the **basis** of its count — `reexecuted` where
the worker ran the search again and counted the real output, `in_process`
where it matched the query with Rift's own index or content matcher and
counted that rendering — so a reconstruction is never averaged in with a
measurement without saying so.

---

## 8.7 The honesty ladder

Rift's numbers sit on a ladder. Each rung claims strictly less than the one
below it, and no surface may promote a figure up a rung.

**Rung 1 — measured.** Tokens returned by a serve, provider-reported usage
classes, cost facts from a seeded catalog — counts of things that happened.
The denominator's read side sits here too, with one distinction inside it: a
file read is counted over the bytes the tool delivered, while a search is
counted by running the same search again and counting what comes back. The
second is a measurement of a *re-execution*, which is the same call over the
same tree or it is discarded, and every row says which of the two it is in its
own `basis` column. A reconstruction that could not be told from a measurement
would be a rung-1 claim about a rung-2 figure.

**Rung 2 — estimated upper bound.** Tokens avoided. The standing rule is carried
in the code where the figure lives: *the avoided total is an estimated upper
bound and never a claim.* It is **counterfactual-free** — it does not know
whether the agent would have re-read those files, only what re-reading them last
cost — so no statement of the form "Rift saved you X" rests on it.

The ladder's own rule was amended once, on 2026-09-09, and it is worth stating
plainly because it reads like a relaxation and is not. It used to say *never
impute*. It now says **never impute without evidence**. The difference is the
prevented read of [§8.6](#86-what-avoided-actually-measures): a read that did
not happen leaves no payload to measure, so booking it at all is an imputation —
but it is an imputation with two observed facts under it, Rift's own
announcement and the window's own act on the file, both recorded before the
figure is computed. What stays forbidden is the imputation with *nothing* under
it: a saving inferred from a frame nobody was measured receiving, from prose no
hook can see, or from a revision that has since moved. The rung does not change
either — a prevented read is an estimated upper bound like every other avoided
token, and it is published under its own name so a reader can take it out. The surfaces
say so out loud: the report prints `tokens avoided (estimated upper bound)`, and
the machine-readable document puts the caveat in the *key name*, so a script
cannot read a claim out of it:

```json
"recall": {
  "available": true,
  "tokens_avoided_estimated_upper_bound": 110813640,
  "tokens_injected_spend": 52143,
  "tokens_returned": 66532,
  "net_estimated_upper_bound": 110761497
}
```

**Rung 3 — an estimate over an estimate.** The dollar figure: a rung-2 token
count (counted with a general-purpose encoder, not the provider's billing
tokenizer) times a rate chosen for a model the savings trail never recorded.
Every rendered amount carries a mandatory leading tilde — see
[§8.8](#88-putting-a-dollar-figure-on-avoided-tokens).

**Rung 4 — absent.** With no honest basis the surface shows nothing rather than
a zero: a missing recall ledger reads "no recall ledger here", never `0`, an
unresolvable price shows no dollar figure, and a channel split no surface could
make prints `n/a` and serializes as `null`. Absence and a measured zero are
different facts, drawn differently.

### Every rate names its denominator

There used to be three rates here and there are two, because the third had no
business being a headline. The dashboard's recall headline divided avoided
tokens by avoided + returned over every audited row, which asks "of the context
this plane was responsible for, how much did it not send" — a real question,
with the same shape on screen as the share below and a different denominator
under it. The card publishes the **share** now, beside the tokens it avoided,
and that rate is gone from every surface.

What remains beside the share is `rift recap --json`, which divides avoided by
avoided + total spent: "against everything this repository spent, how big is
the avoided figure" — on the repository of
[§8.1](#81-see-your-own-numbers-first), about 1.4%. An unlabelled percentage is
the shape a claim hides in, so the machine-readable key is
`rate_percent_of_avoided_plus_spent`, not `savings_rate`. Both rates now go
through the one guarded ratio function shared with the delta-ledger metrics,
`0 / 0` defined as `0`, so they differ in their denominators and in nothing
else.

The rate to quote when someone asks what the levers are worth is **the savings
share**, `avoided ÷ (avoided + read)` since the
last window boundary, where the numerator comes from the audit relation alone
and the denominator adds the read tokens the capture projection recorded over
the same window. One computation produces it for every surface — the command
line, the status block and the dashboard headline read the same function — and it
publishes a second variant that drops families with decisions and no captured
reads, naming them, because a family that decides and is never observed reading
divides by its own numerator and prints 100 %. The denominator counts a
recognised shell read the same as a read tool's — a Codex `cat` or
`sed -n 'A,Bp'` is priced at the file's own tokens over the range it asked for
([§3.5](03-capture.md#a-read-is-a-tool-call-or-a-command-line)) — so the
exclusion is now a statement about a family that genuinely read nothing this
repository could see, rather than about a client whose reads nobody looked
for.

Two properties of that fraction are **published rather than papered over**,
because both move it and neither is fixable where it is computed. The two
sides are cut on two clocks — decisions by `memd_audit_event.recorded_at`,
reads by `tool_call.started_at`, each the instant its own relation records and
the second the one the reread report already places a read by — so near a
boundary a decision and the read it answers can fall on opposite sides of the
cut, which is why every surface prints the window it used. And a read whose
token count was never frozen ([§8.12](#reads-whose-token-count-was-never-frozen))
is a denominator row of **weight zero**: it counts as a read and adds nothing,
so the share is over fewer measured reads than its `n` suggests. Those are
counted and published — `reads_unmeasured` on the report, on `--json` and in
the dashboard's breakdown — never imputed, and never quietly dropped from the
denominator, which would be a different lever with a different name.

**The product declares no target.** The share is the figure the levers move,
and every surface publishes it as a measurement with its denominator — never
beside an aspiration it could be mistaken for. The only threshold anywhere is
the evaluation bar's Go/NoGo
([§8.13](#813-the-bar-is-what-the-harness-measured)), a separate, versioned
economic choice that gates the harness and not this figure.

**The window boundary is a file, not a fact about the data.** By default the
window is the whole record — every row either projection holds — which is what
"since the reset" has always meant. `rift savings reset` writes
`<common_dir>/rift-family/savings-window.json`, and from that instant on the
share counts only what was recorded after it; `--at` places the boundary
explicitly, and a boundary in the future is refused, because a window starting
after now excludes the very sessions it was opened to measure. The marker is
family state, in the directory every member shares, so a linked worktree
measures the period you set rather than one of its own. Nothing is deleted and
no relation is touched: `rift savings --since 0` still answers over the whole
record, `--since <instant>` answers over any other window for one call without
moving the stored boundary, and deleting the file restores the whole record.
Every surface prints the boundary it used, which is why the share on the
command line and the share on the dashboard headline cannot be two different
periods. The reread report is the deliberate exception — it is the whole
record's document, and when a marker makes that a different period it says so
on the line where a reader would otherwise compare the two.

One lever publishes a second figure of its own, for a reason worth stating:
bounding a content search books **no** avoided tokens — the bound is not a
saving anybody counted — so what it publishes instead is the **re-query rate**,
the share of bounded searches the model asked again in the same turn. A bound
the model immediately works around has saved nothing, so the rate is how this
lever shows it did not simply move the same cost one call later
([§7.7](07-serving-and-injection.md#77-searches-are-bounded-and-sometimes-answered)).

Beside the share goes the **prevented figure**: how much of the avoided total
is reads that never happened rather than reads that were declined. It is
published on every surface the share is — overall and per family — as a part of
the avoided figure and never as a second one beside it, and both are taken in
one pass over the same rows, because a breakdown that does not add up to the
number it breaks down is worse than no breakdown at all.

Beside it goes the **honesty figure**, `avoided − returned` over the same rows.
It is the standing per-class test: a lever whose frames, maps and sidecars cost
more than the reads they displaced is not a saving, and a class that fails it
is turned off by data rather than defended. On this repository the figure is
positive by roughly ten thousand tokens since the reset — and the share itself
is small (about 1 % overall, 3 % over the window in which the levers acted), on
a window holding three captured reads. Both are published with their `n`
attached, unrounded and never re-denominated, because the honest reading is
that the mechanism is demonstrably live and the window is nearly empty, not
that the steady state has been measured.

Audit rows recorded before the accounting seam landed carry
`tokens_returned = 0` and under-count. They stay as they are — the relations are
append-only and a correction would be a rewrite — so the figure is cumulative
and honest about the trail it sums.

### Checking a booked figure against a removal

Everything above is booked from what Rift decided, which is why rung 2 stops
where it does: the ledger knows what a re-read last cost, never whether the
re-read was going to happen. There is one way to put a number beside that, and
it belongs to developers rather than to any surface. `dogfood --compare` plays
the same scripted sessions twice through the real clients — once in a checkout
with every read lever switched off, once as they ship — and prints what the
levers booked over what the two arms actually removed, as a percentage. Read it
as a sanity check on the booked figure's size, not as its replacement: the two
arms are separate, non-deterministic model runs, so the difference between them
carries the levers and the day's tool choices together, and a run where the
lever arm read *more* is reported as the negative it is rather than clamped.
Adding `--bypass-shell` plays a third arm beside the other two: the levers as they ship, with the client's own read and search tools withheld so every read goes through its shell.
Most real traffic reads that way, and no scripted session otherwise does, so this arm gives shell reads a controlled corpus; it prints beside the on arm and leaves the calibration itself unchanged.
Only Claude Code plays it: Codex has no read tool and already reads through its shell, and Antigravity has no pinned flag that withholds its read tool.
It is a command a person types on their own machine
([§16.15](16-development.md#1615-a-savings-reading-through-the-real-clients)),
never a claim the product makes and never a figure that promotes a rung.

---

## 8.8 Putting a dollar figure on avoided tokens

Tokens are a true unit but not a felt one: nobody can tell from "14.6M tokens
avoided" whether a toolchain earns its place. Money answers that, but the
two halves of the arithmetic live in different projections and share no key. The
**savings** half is the recall projection's audit trail (verb, handle, reason,
tokens returned, tokens avoided, timestamps) with **no model column and no
token-class column**; the **pricing** half is the capture projection's
bitemporal catalog, keyed by the model on a captured call. Nothing on the
savings trail says which model a serve was for, so any price applied is a chosen
assumption — the job is to choose defensibly and disclose it.

**The class basis: uncached input.** Avoided tokens stand in for full re-reads,
and a re-read is fresh, uncached input: as output they would overstate by about
five times, as cached input understate by about ten.

**The model split: a disclosed partition, not a fabrication.** The avoided total
is *partitioned* across the models the project ran, weighted by each model's
observed share of input tokens, each slice priced at that model's uncached-input
rate, the headline being the sum of the subtotals. Largest-remainder rounding
makes the slices add to exactly the avoided total, so no tokens are lost or
invented and the breakdown cannot contradict the headline. Pricing the same
tokens at several models and adding the results would count one saving twice,
and is not offered.

**The rendering rules**, each a requirement rather than decoration: a leading
`~` on **every** derived amount, headline and row alike, never conditional; a
per-million-token *rate* without a tilde, because a seeded catalog rate is a
recorded price; a positive saving below a cent as `~<$0.01`, never `$0.00`; no
amount at all for tokens whose model has no seeded price; thousands separators;
and no animation between frames — the figure is copied untweened from the target
frame, because a cost figure that tweens reads as if it were changing.

**Naming what the figure leaves out.** A model with no seeded price adds usage and no dollars, so a dollar figure over a window that used one silently reads low.
`rift doctor` says so under its `prices` heading: how many models the captured calls named, then one line per model the catalog cannot price now, with its call count, in a stable order:

```text
  prices:         3 model(s) observed, 2 without a price
    Gemini 3.7 Flash (Low): 12 call(s) — their usage is recorded, and no cost is derived until the catalog prices this model
    claude-opus-5-5: 8 call(s) — their usage is recorded, and no cost is derived until the catalog prices this model
```

When every observed model prices, the heading says so and nothing follows it.
The line reads the projection and writes nothing; the remedy is a seeded price, appended like any other, never a guessed one.

---

## 8.9 The degradation ladder

The dollar estimate degrades down four rungs. With a mix observed and every
model priced, the breakdown shows in full and the headline is the sum of rows.
With some models unpriced, those still appear with their token slice and an
explicit *no seeded price* marker contributing no dollars, the modal stating how many
tokens are unpriced so a partial figure is not mistaken for full coverage. With no usable input-token mix, one reference model — the most
frequently captured — carries the total as a single disclosed row. With nothing
resolvable (no model, no price, or any read failure) the headline draws no
dollar segment and the tokens and share stand alone. The cross-projection read that resolves the mix is
opened read-only and *any* failure lands on that last rung: a pricing problem
cannot break the dashboard.

Rung 3 originally intended to use a session's primary-model field, but it
measured empty across every captured session: when a fallback's own basis is
empty, the fix is a different fallback, not a default value.

**The mix is cut where the tokens it prices are.** Both the observed mix and
that reference-model fallback are read over the card's own window, because the
avoided tokens they partition are that window's: a mix taken over all history
against one period's savings attributes this week's tokens to a model the
project stopped running in the spring, and the modal would then disclose a
basis that was never this window's. A period holding no model call therefore
lands on the bottom rung and shows no dollars — which is the honest answer, not a
lost figure. A seeded rate of exactly `0` lands there too: a price of zero is a
missing price, and `~$0.00` beside a real saving reads as a measurement.

---

## 8.10 The savings headline in Canopy

The dashboard's Recall panel headline reads
`saved 110.8M tokens · 1.4% · ~$803.11` — the tokens the plane avoided, then
**the share of [§8.7](#87-the-honesty-ladder)**, to one decimal because that is
how `rift savings` prints it, then the dollar estimate. Every figure on the
card comes from one window resolution: the period button's trailing cut
composed with the family's boundary marker, handed to the same
`savings::compose` the command line calls, so the headline, the two bars, the
serve count and the modal cannot cover different periods. The bars under it
are the window's read and avoided tokens on one shared scale; dollars reach
the card through the headline and nowhere else.

The headline is the card's one door: any cell of the line opens a read-only
breakdown overlay carrying the disclosed basis in full — net tokens avoided
priced as uncached input at the seeded rates, one row per model summing to the
total, a *by worktree* line saying the figure is the family's, the share block
with its window, its `n`, its unmeasured reads and its per-family rows, and
the note that `~` marks an estimate, not a bill. This repository sits on the
ladder's second rung — five models observed, two without a seeded price — so
the total covers the priced share only.

Three rules there carry the honesty of the figure. **No honest figure means no
segment**: an unanswerable share draws no percentage and an unpriced window
draws no dollars — not a greyed figure, not `0 %`, not `~$0.00` — and a seeded
rate of exactly zero is a missing price rather than a free saving. **A serve
carries no session key**, so the avoided total cannot be split by worktree;
the modal says the total is the family's rather than splitting on a guess (see
[Chapter 9](09-worktrees-and-families.md) for why savings still compound). And
**the breakdown is reachable both ways**: `S` while the graphs column has focus
opens exactly what the headline opens, through one opener, so a keyboard-only
operator is not reading the disclosed basis out of `--json` instead.
[Chapter 11](11-canopy-tour.md) walks the panels, including the four token
classes' treemap.

---

## 8.11 The same numbers from the command line

`rift dispatch` prints the report of [§8.1](#81-see-your-own-numbers-first), and
`--json` emits the same facts with the caveat in the key names.

`rift savings` prints the share of [§8.7](#87-the-honesty-ladder): overall and
per family, with the honesty figure, the reads it could not weigh, the window
it covers and the excluding variant beside it. Reading is read-only, and it
says so when a family has decisions and no reads rather than printing a 100 %
it cannot defend.

Two flags narrow it for one call, and neither writes anything.
`--since <rfc3339|epoch-ms>` answers from a stated instant.
`--last 24h|7d|30d|all` answers over a **trailing** period — the dashboard's
own four, from the dashboard's own vocabulary, so `[7d]` on the card and
`--last 7d` in the terminal are the same window rather than two tables that
happen to agree. A trailing window **composes** with the family's boundary
marker instead of replacing it: the cut is the later of the two, because both
are exclusions and two exclusions compose to the tighter one, so no period can
report rows that fall outside the evaluation somebody set. The two flags are
mutually exclusive — one window per call, named one way — and an unknown
spelling is refused rather than rounded to something near it.

`rift savings reset [--at <rfc3339|epoch-ms>]` is the one verb
in this chapter that writes, and all it writes is the family's boundary marker:
it moves where the share starts counting and destroys nothing.

`rift recap --since <when> --json` gives offline usage analytics for a window:
`tokens` (all four classes), `cost.amount_micros`, `tool_mix`, `serve_calls`, a
per-agent or per-model session split, and the `savings` block of
[§8.7](#87-the-honesty-ladder). Windows accept a UTC day (`YYYY-MM-DD`) or epoch
milliseconds, and both bounds are inclusive: a **day is a period**, so
`--since D` opens at that day's midnight and `--until D` closes at its last
millisecond — `--since D --until D` is the whole of day `D`. (It used to
resolve both edges to midnight, which admitted one millisecond of the day a
reader asked for.) `--model` splits by model instead of agent. One wrinkle: the
*human* rendering prints sessions, checkpoints, tokens, tool mix and split but
no savings line, even though the command's help advertises savings
(`render_recap` in `bins/rift/src/workflows/recap.rs`) — use `--json`, or `rift dispatch`.
`rift-memd metrics` reports the delta-ledger's *baseline* rather than the
serving trail: total input tokens, `repeated_context_rate`,
`avoidable_context_rate` and the counts they imply. All of these commands
are read-only with respect to your repository's files; `rift-memd metrics` and
`rift-memd eval` write to the recall projection, which is rebuildable from git.

A rebuild of the recall projection keeps the serves it cannot derive.
`rift-memd rebuild` sets the old projection aside under a dated name rather than delete it, because a serve a person made at a terminal is in no checkpoint, and the figures above would lose it.
An active store or retained connection makes replacement refuse promptly; close the reader or writer and retry.
A live legacy recall projection without its concurrency marker is unavailable to read-only diagnostics until `rift-memd metrics` opens it through the writable ingest path.
`rift backfill --recall-from <file> [--dry-run]` reads those serves back into the family's recall projection, insert-or-ignore, and prints what it read and inserted per relation; run it again and it inserts nothing.
`rift doctor` lists every set-aside projection by name, size and age, and `rift clean` never removes one ([§13.12](13-storage-and-schema.md#1312-rebuilding-and-what-a-rebuild-cannot-restore)).

> Tip: `rift-memd metrics` **ingests** as well as reports, walking the
> repository's checkpoints into the recall projection before computing. `rift-memd status` opens or migrates the recall store to report node counts.
> Use `rift doctor` or `rift status` for guarded read-only diagnostics.

---

## 8.12 The measured baseline: repeated and avoidable context

Before optimizing anything, Rift measures how much of your context spend was
avoidable. Two rates come out of the read stream captured in your own sessions:
`repeated_context_rate`, input tokens spent re-reading a `path@hash` already
seen earlier in the stream, and `avoidable_context_rate`, that plus reads of
content identical to the current worktree. Both divide by total input tokens
over in-scope reads. The second is wider: a file read once is still avoidable if
its content is unchanged from what is on disk now, because a delta or handle
could have stood in. Scope containment applies here as everywhere — out-of-scope
reads never contribute ([Chapter 6](06-recall.md)).

This is the "measure before you optimize" number, computed from *your* sessions
rather than a benchmark. On this repository's captured history it measured
`repeated_context_rate` 0.1899 and `avoidable_context_rate` 0.3370 — 262000
repeated and 465060 avoidable tokens out of 1380000 input, so **33.7% avoidable
context** across eight sessions, which the evaluation bar in
[§8.13](#813-the-bar-is-what-the-harness-measured) was calibrated against.

### What each lever removes from its pool

A rate over live traffic tells you what happened; it cannot tell you what a
lever is capable of, because it is hostage to how much reading a window
happened to do. So each lever is also measured on a **deterministic fixture
driven through the shipped binary**: a scripted session reads, edits, verifies
and re-reads, and the fixture publishes three numbers — the *pool* (what those
calls would have cost with the lever off), what the agent *received* instead,
and the difference.

- **Re-reads.** On a class-sized corpus the pool is 106,221 tokens and 6,438
  are received, so **93.94 %** is removed, over fourteen declines, with the
  escapees itemised: the tokens that leave through the one-per-key repeat and
  the tokens under the line floor.
- **The same fixture at small-file size** publishes **42.64 %** and is held to
  no bar at all. It exists to price the floor: at a few hundred tokens a read,
  a reason frame is a large fraction of what it replaces, and publishing the
  weak case beside the strong one is what keeps the strong one meaningful.
- **Documents.** Converting one document per format removes **90.94 %** of the
  priced pool — 107,784 tokens against 9,761 received — and the pull verb,
  which pays for its own answer envelope, removes 89.72 % of the same pool.
  Per format the range is wide and stated: presentations and word-processor
  documents shed over 95 %, spreadsheets around 70 %, and legacy binary formats
  almost nothing, one of them refused outright and asserted by name so a
  conversion that quietly stopped happening reddens the run instead of raising
  the average.

- **Searches.** A generated five-thousand-path repository, seven name searches
  named before anything is measured, driven through the shipped binary on all
  three families, in two arms — the lever on and the lever off, which removes
  **93.33 %** of that pool. What the fixture prices is deliberately narrow: only searches for file *names*, only
  at the ceiling each client's own documentation pins for the call being
  replaced (a hundred files for a glob, about thirty thousand characters for a
  shell result, the latter figure taken for the two clients that pin neither so
  that one number describes a listing across all three), never
  a content search, and never a listing shape Rift declines to answer. The
  removal is published per lever, per family and per search rather than as one
  number, because the spread is the honest part: answering a glob is capped by
  arithmetic — a ranked head of eight against a listing the client cuts at a
  hundred — while a recursive listing has no such ceiling, and the short
  listings where an answer would cost more than the listing are the cases the
  cost guard declines to make at all.

Every figure above is the *pessimistic* reading where a choice existed: a
container whose parts are stored rather than compressed reaches a native read
as legible markup and therefore has the smaller pool, and that is the pool the
fixture prices.

### Reads whose token count was never frozen

Sessions captured before per-read token counts existed record only path, hash
and tool, and ingesting them at zero would make the report hollow.
So the ledger resolves what it can, verifying every candidate against the
recorded hash *before* counting it: a sibling record's inline figure for the
same content hash, the worktree when the file still hashes to the read-time
value, or a recorded blob address whose object-store bytes verify. The last two
run only when the real byte-pair encoder is available, since freezing crude
characters-over-four figures would be worse than leaving them absent — and a
hash only ever maps to the one count of its verified bytes, so two rebuilds may
differ in **coverage** but never in **value**.

What resolution cannot reach is **counted where the share is read**. A read the
projection holds no token count for is still a read: it sits in the savings
share's denominator weighing nothing, which lifts the published share by however
much it would have weighed. `reads_unmeasured` is that count — on the report, in
`rift savings --json`, and in the dashboard's breakdown beside the `n` — so a
reader can see how much of the denominator is rows nobody measured. It is never
imputed into the figure and never quietly dropped out of it: the first would be
the imputation-without-evidence the ladder forbids, and the second would change
what a read *is* without saying so.

---

## 8.13 The bar is what the harness measured

A measured share and a gate are two different numbers, and conflating them is
how a gate comes to fail builds for work nobody has done yet.

**The share is a measurement.** It is the fraction of the
repository-content tokens an agent would otherwise read — over *any* channel:
the Read tool, shell reads, search lines — **first reads included** — that
the plane returned instead, over the windows `rift savings --last` and Canopy
show. The product declares no target for it: every surface publishes the
measured share with its denominator, and nothing fails because a window sits
anywhere in particular. One denominator for every surface is what lets "how
are we doing" have one answer.

**The bar is the evaluation harness's**, and it is *staged*: `SAVINGS_BAR_PCT`
is the median savings the harness's own paired replays have
measured — a full re-read leg against a delta-served leg over scripted
scenarios — and the rule written on the constant is that it is raised only
after the harness measures a higher median and **never above what it last
measured**. It stands at **15%** today, the original calibration seed. A bar
set to an aspiration stops being evidence of anything the moment it is set; a
bar that moves only behind a measurement is a record of what the levers have
done.

The bar is declared once, in `rift-recall-ledger`, and the go/no-go gate
*reads* it rather than retyping it.

**A third figure is neither.** `cargo run -p xtask -- replay-savings` replays
recorded transcripts through this build's read gate and reports what it would
have declined, bucket by bucket, over the same denominator the share uses.
It is the field measurement the levers are judged by, and it is never read
against the bar: its denominator carries first reads and searches, which
no gate can decline, so it is always far below the harness's median. On the
seven session trees recorded on the machine this work was done on (21 windows,
2,563 calls, 1.07M repository-content tokens), the build before this work
declined **0.1%** and this build **0.0%** — the one decline it had was a slice
under the floor once the floor moved from 24 to 28 lines — while the shell
grammar went from recognising 29 of 1,252 shell calls to 132, and 52.5% of the
pool is a first sight of a revision inside its window (21.2% of it read again
by a sibling window), 19.7% is a later page of a revision the window already
holds part of, and 638 shell calls naming a file this checkout has are still
shapes the grammar refuses. That table, not the harness's median, is where the
levers' progress on your own traffic is measured, and [§18](18-known-gaps.md)
carries what each of those buckets would take.

The replay also sizes the pools later levers would work on, which it prints beside the buckets rather than inside them.
It sets the client's own declines beside this build's, splits the sibling re-reads by whether the second window was forked from the first, was handed the lines in its spawning prompt, was handed only the path, or was handed nothing — one count per later window's first read of a revision a sibling read first, so the routes sum to those reads rather than to the pairs — counts the reads of machine-written files by path shape, and sorts the first sights by the size of the file against a first-contact floor of 400 lines or 16,384 bytes.
Given `--history <git-dir>`, each window's paths are resolved at the newest commit at or before the window began, so a file deleted or split since is measured as the session saw it, and the worktree answers only for a path that commit does not have.
`--artifact <dir>` writes the table and its JSON twin with the build, the facts mode, the corpus and the run's times attached, and the tracked runs live in `benchmarks/replay-savings/`.

Serving deltas instead of full re-reads is only a win if it saves tokens
*without making the agent worse*, so the harness replays each scenario twice: a full-read baseline, and a delta-served run where the agent
receives a compiled pack and retrieves by handle until it finds the decisive
fact. The verdict is **Go** only when median token savings and median cost
savings both clear the bar, task success, test pass and the
human-modification outcome are **not degraded** versus the baseline, and latency
is inside the envelope of 16 deterministic steps. Otherwise it is **NoGo**,
naming every failing dimension — `token_savings`, `cost_savings`,
`success_degraded`, `tests_degraded`, `human_mod_degraded`, `latency_envelope` —
so the result is actionable, not a black box.

Four design choices. **Non-degradation is the non-negotiable half**: the savings
percentage is an economic choice living in a *versioned* thresholds value, so
changing the bar is a reviewable edit that bumps a version, while the outcome
conditions hold regardless of savings — a cheaper-but-worse agent is not a win.
**The threshold has provenance**: 15% was calibrated against the measured 33.7%
avoidable-context rate of
[§8.12](#812-the-measured-baseline-repeated-and-avoidable-context). It is not
comparable with the share of [§8.7](#every-rate-names-its-denominator): this
one is a *gate* on median savings over a synthetic replay corpus, that one is a
*measurement* over your own traffic, and only the versioned thresholds value
moves what CI refuses. **The delta
leg is a real serve**, driving the dispatcher in-process over a fixture store —
anchor, pack, handle retrievals — on the path the recall verbs use, with the
claim captured from real audit events rather than stamped by the harness.
**Latency is counted in steps**: one dispatch call is one step, no clock enters
the harness, and wall time is measured elsewhere
([Chapter 15](15-benchmarks.md)).

For every pair the harness compares what the serving side *claimed* it avoided
against `baseline − delta` as *measured* by the replay, within a ±5% tokenizer
tolerance, flagging anything outside the band and naming an over-estimate: it carries
its own measurement rather than restating the serving figure.
Alongside it runs a **seeded regression** where the delta leg deliberately
re-reads the world: savings are exactly 0% and the harness must
return NoGo naming the savings dimensions, because a gate never observed to fail
is not evidence of anything.

A second condition runs the same way for the decisions of
[§7.4](07-serving-and-injection.md#74-tier-three-the-decision-at-the-read): the
delta leg is served declines and delta frames rather than packs, over corpora
varying the changed set and where the decisive fact sits. It reports P10
62.35 % | P50 **67.68 %** | P90 70.96 % token savings with correctness, tests
and human-modification outcomes 4/4 against a baseline of 4/4 — non-degraded —
every claim reconciled within the tolerance with no over-estimate, and its
honesty figure 13,316 avoided against 1,250 returned. Verdict: Go. The lower
percentages than the pack condition's are the point: a decline returns a frame
where a pack returns nothing, so it can never reach the pack leg's numbers and
is measured on its own terms.

Both reports are committed snapshots, so a verdict change is a reviewed diff
rather than silent drift. The served report's four tasks spend 543 to 595
tokens against baselines of 11064 to 44214 — 94.62%, 97.54%, 98.67% and 96.44%
saved in 2 steps each — all reconciled within ±5% with 0 over-estimates, for
percentiles P10 95.17% | P50 96.99% | P90 98.34% against thresholds v1 (P50
token ≥ 15.0%, P50 cost ≥ 15.0%, latency ≤ 16 steps) — verdict GO. Those percentages are **over a synthetic corpus of small fixture
files** exercising corpus size, changed-set size and fact position: evidence
that the machinery works and the gate is calibrated, not a promise about your
repository, whose number is in
[§8.12](#812-the-measured-baseline-repeated-and-avoidable-context).

`rift-memd eval` appends the verdict to an append-only, bitemporal experiment
relation and prints it. The run identity is content-addressed over the
task set, condition and code version, so an identical re-run appends nothing:
the committed snapshot is the reviewable contract, the rows the history that
makes regression visible. P10 and P90 are reporting only — the verdict gates on
the median.

---

## 8.14 What these numbers do not cover

- **Per-serve model attribution does not exist**, and threading the requesting
  model onto the audit trail is a capture-and-serve-plane change; when it lands,
  the attributed split becomes a fallback.
- **Historical pricing of each serve is out of scope**: the current rate applies
  to the cumulative avoided total, and re-pricing every historical serve at the
  rate in force then is over-engineering for a hedged estimate.
- **USD only.** No other currency, no locale-aware formatting.
- **The savings figure appears in one dashboard place**, the Recall panel.
  Money reaches it in one form only: the headline's dollar segment, the
  avoided tokens priced on the ladder above, tilde and all — not the cost
  facts the command-line reports print. The read-vs-saved bars under it are
  always token counts.
- **Prices are seeded data, not user configuration.**
- **The evaluation corpus is synthetic and small.** Its representativeness is a
  tracked open question.

---

## Related chapters

[Chapter 7 — Serving and injection](07-serving-and-injection.md) ·
[Chapter 6 — Recall](06-recall.md) ·
[Chapter 3 — Capture](03-capture.md) ·
[Chapter 13 — Storage and schema](13-storage-and-schema.md) ·
[Chapter 11 — Canopy tour](11-canopy-tour.md) ·
[Chapter 15 — Benchmarks](15-benchmarks.md) ·
[Chapter 18 — Known gaps](18-known-gaps.md)
