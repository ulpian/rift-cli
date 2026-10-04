# 18 — Known gaps

This chapter lists what is **not finished, not measured, or narrower than it
looks**. It exists because a document that reads uniformly confident cannot be
audited: if every sentence sounds equally strong, nothing tells you which are
load-bearing. Most items are also stated in the chapter explaining the mechanism;
this is where they are collected.

> Tip: if you are evaluating Rift, read this chapter second, right after
> [00](00-start-here.md) — it tells you faster than anything else
> whether the parts you care about are built, half-built, or drawn.

---

## 18.1 How to read an entry

Each entry opens with its **kind**, states what is open, says why it matters and
what closing it takes, and points at the chapter describing the machine.

| Kind | Meaning |
|---|---|
| **Never built** | A design intention with no code behind it, or code with no caller. The scaffolding may be real and tested; nothing shipped reaches it. |
| **Measured defect** | Something demonstrably wrong, with the demonstration recorded. |
| **Honest limit** | Working exactly as intended — but the intent is narrower than a casual reading of the surface suggests. |
| **Not run** | An act nobody has performed. The instrument exists; the measurement does not. |
| **Stale description** | Prose — a comment, a help string — that no longer matches the code. The code is the authority. |

The distinction that matters most is the first against the third: *never built*
is work somebody could do tomorrow, while *honest limit* is usually a decision,
and "fixing" it without understanding that decision makes things worse.

## 18.2 What is *not* on this list

**Reversed decisions are closed, not deferred.** A semantic code-graph plane,
and the repository-wide engine designed alongside it, were reversed permanently
— not unbuilt work waiting for a volunteer but ideas the project declined. Both
scaffold crates stay deleted and their absence is enforced mechanically, so
restoring either reddens a check. The code graph was never revisited, and nothing indexes what your code
*means*. See [17](17-invariants.md).

**Retired machinery is gone, not pending.** Rift once shelled out to a foreign
binary, served recall over a long-lived server transport, spawned your editor
from the dashboard, and keyed everything by individual worktree. All of it is
removed; the read-compatibility paths keeping old captures readable are the only
survivors. A description that reads as live is stale — §18.11 lists the known
ones.

---

## 18.3 Capture

### A history rewrite leaves the ledger holding pre-rewrite shapes

*Never built.* Nothing reacts to a rebase, an amend or a merge, and no hook is
installed for any of them — the absence is visible in the dispatch table rather
than hidden behind a routed name with an empty body. A session whose commits a
rebase moves keeps the shapes it recorded before the move. Closing it needs a
decision before a body: the append-only rule forbids editing sealed rows, so the
answer is a new superseding record.

**Read more.** [03](03-capture.md),
[05](05-ledger-and-verification.md).

### A line whose parent content was never observed attributes as unknown

*Honest limit, with an unbuilt fallback.* Attribution replays a session's
recorded edits onto the file's parent content, and capture records the parent as
it stood at the last boundary, so a file committed part-way through a session
works. What degrades to `unknown` is a parent **no boundary ever observed**:
nothing to replay onto, so the code says so rather than guessing — honest, but
the line cannot be credited to human or agent. The recorded fallback, a per-turn
watermark letting the replay start from a later known point, was designed and
never implemented: no such column, no such code.

**Read more.** [03](03-capture.md).

### Sessions captured before turn boundaries existed have none, forever

*Honest limit.* Adapters gained turn-boundary signals partway through the
project, and the ledger is append-only, so earlier sessions cannot be back-filled
— the doctor's per-session coverage leg lists them on every run. The only
consequence is a permanent tail of zero-coverage rows that reads as a problem and
is not one; the only available change is presentational, separating "captured
before boundaries existed" from "captured badly".

**Read more.** [03](03-capture.md).

### The interval archive strategy still runs on the hook path

*Honest limit.* Most archiving moved off the hook path into a detached worker,
which is how the latency budgets are met; the strategy archiving on an elapsed
interval still decides and runs inline, on agent activity. It is the one path
left where a capture decision can put work in front of the agent — inside budget
today, and the first place to look if a budget regresses. Closing it means
routing that act to the detached worker, which means giving the worker the
schedule state it needs.

**Read more.** [03](03-capture.md).

### A read whose bytes change before the worker runs is not measured

*Honest limit, by design.* Read-tool token counting happens off the hook path,
so if a file's bytes change between the read and the worker's pass and what the
agent saw is not in git, that read's cost cannot be reconstructed and is left out
— a small bounded under-count rather than a wrong number, since the accounting
prefers to omit what it cannot support. Closing it means capturing the bytes at
read time, trading a known omission for a durable copy the redaction pipeline
would then have to gate. The trade was deliberate.

**Read more.** [03](03-capture.md),
[08](08-cost-and-savings.md).

---

## 18.4 Redaction

### The legal cross-class split has no live instance

*Honest limit.* The false-positive census lets one file's accepted hits fall
under two justification classes when different detectors fire; the arithmetic
handles it and a control case models it exactly, but no real census row has
exercised the shape. The arithmetic subtracting accepted hits from the ceiling is
the part that must not be wrong, and it is proven against a constructed case.
Nothing active closes this — the first genuinely split file is the event — and it
is worth knowing before assuming a shape is field-proven because a test names
it.

**Read more.** [04](04-redaction-and-secrets.md).

### Two instruments disagree on isolated redaction cost, and nobody knows why

*Honest limit / open question.* Two harnesses run against a provably identical
corpus — same generator, same seed, same span counts — and disagree on how long
isolated redaction takes; one is roughly twice as fast in a release build and
several times slower in a development build, so it is not a build-profile
difference, and no explanation has been produced. The practical rule: **no
absolute redaction-cost figure from either instrument should be quoted across
sessions** — re-measure instead. The growth result is unaffected, since both
agree on how cost scales with input size. The disagreement is recorded rather
than tuned away, since adjusting the corpus until the numbers matched would have
destroyed the evidence.

**Read more.** [15](15-benchmarks.md).

---

## 18.5 The ledger

### The per-checkpoint reference layout was designed and never built

*Never built.* An early design gave each checkpoint its own git reference plus a
policy reference. Checkpoints today write to one moving branch reference under
the Rift namespace, and the hash chain — not the reference layout — is what makes
history tamper-evident. This matters only if you read a description of the
reference layout and expect to find it; closing it would need a reason, and there
is no known problem the current layout causes.

**Read more.** [05](05-ledger-and-verification.md).

---

## 18.6 Recall and serving

This area has the most open items, and the pattern is consistent: **the policy,
the accounting, the audit and the caps are real and exercised; some of the
content behind them is not there yet.** That inverts the usual failure, where a
feature works in the happy path and the guards around it are decorative.

### The content-addressed recall index was never built

*Never built.* A git-native, content-addressed index for recall data was
designed; it does not exist as a crate, a module or a file layout. The recall
store's relational tables carry the graph instead, live and written — thousands of
rows in a working repository. Read descriptions of a content-addressed recall
index as plans. Closing it would need a reason to move a working relational
projection onto a different substrate, and there is not one.

**Read more.** [06](06-recall.md),
[13](13-storage-and-schema.md).

### The semantic retrieval plane was never built

*Never built.* An accepted design called for an embedding-provider seam in its
own crate, content-addressed embedding caches as durable artifacts, and hybrid
retrieval adding a fourth signal to ranking. None exists — no crate, no provider
seam anywhere in the workspace — and the ranker fuses exactly three signals:
changed-file overlap, error overlap, recency. Retrieval is therefore lexical and
structural only, through a transparent linear combination you can inspect.
Closing it takes all three pieces under the standing rules that capture makes no
network calls and the toolchain runs no model of its own — which is why the seam
comes first and the provider behind it is somebody else's process.

**Read more.** [06](06-recall.md).

### Nothing in the shipped path binds a summary

*Never built. This is the single largest gap in the guide.* The validity plane's
whole point is a summary bound to the exact content it describes, so a stale
summary is unconstructible rather than unlikely. The type wall is built, tested
and honest; what is missing is the **writer** — no shipped command computes a
binding and stores one. Downstream, `read-summary` always answers unavailable and
not-bound, served manifests never carry the summary-included state (one of the
four envelope states never occurs), the store's one encrypted column and its
key-custody table have no rows, and the compounding proof has to **seed** its
binding at the seam a future writer would use. Closing it takes an ingest path
that computes summaries at a boundary, calls the binding writer, and hands the
known-state verb a summary to pass through.

**Read more.** [06](06-recall.md) §6.15,
[07](07-serving-and-injection.md),
[09](09-worktrees-and-families.md) §9.10,
[13](13-storage-and-schema.md).

### Exact retrieval returns an empty body

*Never built.* Ask for exact content with a valid grant and the request resolves,
passes the policy gate, is charged against the budget, is written to the audit —
and comes back with no bytes, because the blob reader wired into both live
channels is an in-memory substrate with nothing in it. The gate is real and the
room behind it empty, so a successful audited retrieval is no evidence that
content retrieval works end to end. Closing it means wiring the real repository
blob reader — which exists behind its own seam — into the serving dispatcher.

**Read more.** [07](07-serving-and-injection.md) §7.15.

### A changed-hunks answer needs a basis, and a redacted one says less

*Narrowed, 2026-09-15.* It used to read: the answer needs the caller's
revision to be reachable as a **blob**, the reachability rule is a bounded walk
of recent first-parent history, and a revision that was never committed
resolves never — which made the newest and most interesting revisions exactly
the ones most likely to have no basis, since an agent's working edit is not a
commit. The entry also said closing it would take a durable basis for
uncommitted bytes, which the redaction rules forbid storing raw.

That is what was built, and the rules were kept rather than relaxed: the
detached worker now keeps the read-time bytes of every revision a read
recorded, **through the redaction gate**, once per repository family and keyed
by the read's own content address, and the ladder is blob → snapshot →
`basis_unavailable`. A revision that was never committed now resolves.

What is left is the cost of obeying the gate, and it is a real limit rather
than a rounding. A mask is a difference Rift wrote, so a changed place computed
against one is not a difference the repository made — and on the verb that
serves lines it would return the **live** bytes of exactly the span the gate
masked. So one hunk that touches a masked run takes the whole answer down to
`(changed)` with no lines, and `changed-hunks` refuses the lines and returns
the neutral digest. A file with a secret in it therefore gets a weaker delta
frame from a snapshot than it would have got from a blob, and a mask that
*moves* a line number is not snapshotted at all. Neither is closable without
either storing raw bytes or teaching the diff to reason about masked spans,
and the first is ruled out.

The blob window itself is unchanged and still the first rung: measured on this
repository, a committed revision inside it resolves in every case tried and
94.2 % at its edge.

**Read more.** [07](07-serving-and-injection.md) §7.4, §7.14,
[06](06-recall.md) §6.8, [02](02-configuration.md) §2.6.

### A delta's provenance is anchored to a checkpoint, not to a turn

*Honest limit.* Every delta a pull verb returns now carries when it happened,
who made it and what it cost
([07](07-serving-and-injection.md) §7.9). Two things about that object are
narrower than they look, and both are visible in what it omits rather than in
what it claims.

The bracket is looked up **by revision**, so the `to` end — the checkpoint that
recorded the bytes the answer is about — exists only where some session read
those bytes. An agent's own working edit is not a read, so on the freshest
change the object usually carries `from` alone. That is the same shape as the
basis gap above and has the same cause: the projection knows what was read, and
an edit nobody has re-read yet is not in it.

And the four token classes and the cost are the **originating session's**
recorded totals — the figure `rift why` prints — not the single turn's. The
capture projection holds usage per model call, so a per-turn cut is available in
principle; the join that would narrow it does not exist, and reporting a session
total under a turn's name would be the kind of mislabelled denominator
[08](08-cost-and-savings.md) §8.7 exists to prevent. Closing it is a narrower
group-by, not new capture.

**Read more.** [07](07-serving-and-injection.md) §7.9,
[08](08-cost-and-savings.md) §8.6.

### The per-turn call ceiling binds per process

*Honest limit.* The budget capping calls and tokens for a turn is held by the
caller, and each command-line invocation is its own process with a fresh budget.
The ceilings bind *within* one call — list caps compose against the token ceiling
and oversized charges are clamped — but do not accumulate across a turn, so the
number in the policy file reads like a per-turn guarantee and is a per-call one.
The accounting and the audit are exact regardless. Closing it needs a caller that
survives a turn and carries the budget; the only one that ever did was the retired
server transport, so this needs a new home rather than a restoration.

**Read more.** [07](07-serving-and-injection.md) §7.10.

### The three clients are wired, and they are not equivalent

*Honest limit.* All three supported clients now receive the pointer, the
manifest and the recovery map on channels pinned to their own published
schemas, and each can be declined to. What they do not have is parity. One
declares a size limit on injected text and truncates past it; one accepts an
injected step that survives later invocations only in a particular kind; only
one documents amending a file read, so document substitution happens there and
the others get sidecars plus a redirect; and one of them, on this repository,
has produced decisions and **no captured reads at all**, which makes its own
savings share degenerate and is labelled as such wherever it is printed.
Closing the last one is capture work on that client's read path, not serving
work.

**Read more.** [07](07-serving-and-injection.md),
[10](10-agent-integrations.md) §10.7.

### A name-search answer is bounded by arithmetic, and weakest where the listing is short

*Honest limit.* An answered listing names a ranked head — eight paths by
default — of a pool the client itself cuts at a hundred, so the most it can ever
remove is fixed by those two numbers before any repository is measured, and the
only lever on it is the head size. At the other end the arithmetic turns
against it: on a listing of a few dozen paths the frame costs a large fraction
of what it replaces, and below the crossing point the cost guard declines to
answer at all. So the figure is published per search rather than as an average,
and the weak cases are published beside the strong ones for the same reason the
small-file re-read case is. A repeat pays a little too: the staleness probe that
asks whether the index still describes this checkout runs on the second call as
well as the first, and on a repository this size it is most of the decision's
cost.

**Read more.** [07](07-serving-and-injection.md) §7.7,
[08](08-cost-and-savings.md) §8.12.

### Two of the three clients publish no ceiling for the call a search replaces

*Honest limit.* What a name search is worth is priced at the result ceiling the
client's own documentation states for the call being replaced — a hundred files
for one client's glob, about thirty thousand characters for its shell result.
The other two clients pin no such ceiling. They are priced at the same figures
as the client that does, so that one number describes a listing across all
three rather than three numbers describing the same call, which is a stated
assumption about those clients and not a fact read off their pages: it says a
shell they run is cut off at about the same size. Closing it needs a pinned row
from each client's own documentation, the same way every other channel fact
here got one — and if either publishes a different ceiling, that number becomes
its own and the assumption stops applying to it.

Two things are not priced at all and are excluded rather than guessed: a content
search, which books no avoided tokens by construction, and a listing shape Rift
declines to answer.

**Read more.** [08](08-cost-and-savings.md) §8.12,
[10](10-agent-integrations.md) §10.7.

### An answered listing ranks on one window's reading, not the family's

*Honest limit.* The command-line search gathers every member's window journal,
so warmth compounds across a family. The answer given inside a hook cannot: it
folds in this conversation's own window journal, one bounded file, and runs no
git status, because a tree walk and a full status are exactly what a decision on
that path may not do. The consequence is narrow and worth stating — the ranked
head a hook offers is ordered by this window's reading, so the compounding claim
of [09](09-worktrees-and-families.md) belongs to the verb and not to that frame.
Closing it means finding family-wide warmth in a form a hook can read as one
bounded file.

**Read more.** [06](06-recall.md) §6.14,
[07](07-serving-and-injection.md) §7.7.

### Two clients take the repeat escape every time

*Honest limit.* A declined read is allowed on the identical repeat, which is
what makes declining safe. On the client whose reason channel its own
documentation describes, eleven scripted runs took the escape **zero** times
and answered from context. On the other two, ten runs each re-issued the
identical call immediately — every time, on both frame shapes, with the same
sealed text — so on those families a decline currently costs a round trip and
returns the bytes anyway, and the saving is booked net of what the frame cost.
The rate is published as telemetry beside the savings and is deliberately not a
gate: the escape working is not a failure, and no rate may demote a family
whose channel is otherwise sound. Closing it is not a wording change — the same
frame produced both outcomes — so it needs an understanding of the clients'
differing tool-failure handling that nobody has yet.

**Read more.** [07](07-serving-and-injection.md) §7.4,
[10](10-agent-integrations.md) §10.7.

### An edit that moves several regions grants no coverage

*Honest limit.* The agent holds what it wrote only where Rift can state the
edit exactly: a whole-file write whose bytes hash to what the agent supplied,
or a single replacement it can locate uniquely in the new bytes. A multi-region
edit, a notebook edit and an applied patch each record the edit and grant
**no** coverage, so the verification read after one of them runs at full price.
That is the conservative direction on purpose — a site Rift cannot state
exactly is one it will not state at all — and closing it means locating several
regions with the same certainty as one, not relaxing the certainty.

**Read more.** [06](06-recall.md) §6.13.

### The cheapest answer to a post-compaction re-read does not exist

*Never built.* After a compaction the first re-read of each file is legitimate
and is allowed, so one full read per file per compaction is the floor of the
current design. The one mechanism that would make that read cheaper — a
structural digest of the file, standing in for the bytes rather than pointing
at them — is the summary rung that nothing binds today. The recovery map
narrows the problem by saying which held files are unchanged and where the
others moved, and the post-compaction re-read share is published so the pool
stays visible. Closing it is the same missing writer as the summary gap above.

**Read more.** [07](07-serving-and-injection.md) §7.5,
[06](06-recall.md) §6.12.

### A parser-free relevance hint is measured but not built

*Never built.* When a file changes, the files that mention it are often the
next thing an agent reads. A parser-free stand-in — the held files whose text
names the edited file's module stem, listed beside its changed-range entry —
is designed and deliberately left unbuilt until observation says the hint would
be used, because it is a relevance claim without a parser behind it and the
plane's rule is that a semantic claim needs evidence, not plausibility.

**Read more.** [07](07-serving-and-injection.md) §7.3.

---

## 18.7 Cost and savings

### The savings figure is an estimated upper bound

*Honest limit. The most important entry in this chapter.* Every savings number
Rift shows compares what a full read of the candidate set *would* have cost
against what was returned. That is a counterfactual, and no counterfactual
estimator exists — so the figure is an **estimated upper bound**, not measured
realised savings, and both the dashboard row and this guide label it as such. An
upper bound is genuinely useful, bounding the opportunity, and it is not a claim
that this many tokens were saved. Closing it takes a counterfactual estimator
plus an honesty-ladder rung above the current top one; until then the stronger
claim is unavailable by rule, not by modesty.

**Read more.** [08](08-cost-and-savings.md) §8.13,
[17](17-invariants.md).

### A second, cruder estimator lives in the validity plane

*Honest limit.* Serving-side size checks use a characters-over-four
approximation rather than the one counting crate. It is used for bounding, not
billing — a cap decision, not a cost figure — but it is still a second
estimator in a codebase that otherwise insists on one authority per number.
Closing it means counting those trials through the same crate every other
estimate goes through.

**Read more.** [08](08-cost-and-savings.md).

### The savings share is real and its window is nearly empty

*Honest limit.* The share of read tokens the levers avoided is computed one way
everywhere and is honestly small: about 1 % across this repository since the
last reset, about 3 % over the window in which the levers were acting. The
second figure rests on **three captured reads**, because the window that
followed the flip did very little reading, while the window before it spent
60.9 % of its read tokens on repeats — which is the pool the levers exist for.
Every decline, substitution and fallback recorded here falls on the acting side
of a cut derived from the data and none on the other, so the mechanism is
demonstrably live; what has not been measured is a steady state. Both figures
are published with their counts attached, unrounded, and are not re-denominated
to look better. Closing it takes ordinary use over an ordinary period, not
code.

Both figures are now taken over a **wider** denominator, so both are smaller
than the ones quoted above: the pool is repository content on any channel, and
a measured search is in it. The figures here are the ones the old denominator
produced and are left as they were recorded rather than re-derived, with the
change noted, because a number quietly restated under a different denominator
is the one thing this chapter exists to prevent.

**Read more.** [08](08-cost-and-savings.md) §8.6, §8.7.

### A search on any channel is now in the denominator, and some of it still is not

*Narrowed, 2026-09-15.* It used to read: `read_tokens` joins
`tool_reads_file`, so only read-shaped calls can be in the pool the share is
taken over, and a search — 324 calls and 237,978 tokens on the owner's replay,
18.7 % of that pool — is never measured at all.

A search is measured now, by re-running it read-only in the detached worker
against a tree whose key has not moved, or by matching it with Rift's own
index and content matcher where it can be matched in process. Three pieces of
it are still outside the figure, and each is published rather than assumed.

A **composition no argv can run** — a pipe, a redirect, a substitution — is
counted and not measured, because reproducing a shell to measure it is a
larger surface than the measurement is worth; `reads_unmeasured` is the count.
A **re-execution is a reconstruction**, not the tool's own bytes: `rg` run here
prints what `rg` prints, and an in-process glob is Rift's rendering of the
paths rather than the tool's, which is why every row carries the basis of its
count instead of averaging the two. And a **tree that moved** between the call
and the worker discards the count rather than attributing a measurement of
something else to it, so a busy repository measures fewer of its searches than
a quiet one. Closing the first takes a shell; closing the second takes each
client publishing its own result bytes, which none does; the third is a
property of deferring the work at all.

**Read more.** [08](08-cost-and-savings.md) §8.6,
[07](07-serving-and-injection.md) §7.7.

### The transcript replay declines nothing on the recorded corpus

*Honest limit, measured 2026-09-15.* `cargo run -p xtask -- replay-savings`
over the seven session trees recorded on the owner's machine before the
work it measures (21 windows, 2,563 calls, 1.07M repository-content tokens)
has this build declining **0 calls**; the build before it declined one, a
slice the floor of 28 lines now leaves alone. The levers did move what they
were aimed at — the shell grammar recognises 132 of 1,252 shell calls where it
recognised 29, the denominator is 9 % wider, every served delta carries its
provenance — and none of that is a decline, because the pool is not made of
the reads a decline can answer.

The table says where it is instead, and each bucket names its own lever.
**52.5 % is the first sight of a revision inside its window**, which the owner
chose to keep allowed; 21.2 % of it is the same revision read again by a
sibling window, which a family-wide coverage — a hook reading every member's
ledger as one bounded file, the same shape [§18.6](#186-recall-and-serving)
asks for under listings — would let the second window decline. **19.7 % is a
later page of a revision the window already holds part of**, and 2.9 % is a
range overlapping held lines: the gate decides a request whole, so a read
that is half held is served whole, and answering it with only the unheld
lines is a serve the frame does not yet spell. **638 shell calls name a file
this checkout has and are still refused** by the grammar, on shapes the two
censuses did not reach. And the credit for a read past 47,500 bytes never
fired, because the client wrote `truncatedByTokenCap` in none of the 176
result objects it recorded here, so a paginated read cannot be told from a
truncated one on this client's surface.

Closing the first two takes new serving shapes; closing the third takes a
third census; the fourth is the client's to publish. The figure is published
as the replay prints it, as a measurement with its denominator
([08 §8.13](08-cost-and-savings.md#813-the-bar-is-what-the-harness-measured)),
and is not re-denominated to the reads the gate can see.

**Read more.** [08](08-cost-and-savings.md) §8.13,
[07](07-serving-and-injection.md) §7.4.

### A PDF's saving is excluded rather than estimated

*Honest limit.* Every document format's native cost is a **declared** baseline
— what those bytes would have cost the reading tool — except PDF, whose native
cost is a page count against a per-page vision rate that no measurement here
pins. Rather than invent a rate, the declared baseline for PDF is **zero**: the
tokens converting a PDF saves are excluded from every published share, and the
document figures are quoted on the priced formats with the exclusion named. The
practical effect is that the document lever's real value is understated by
however much PDF reading a repository does. Closing it takes a pinned rate from
a client that publishes one.

**Read more.** [08](08-cost-and-savings.md) §8.6.

---

## 18.8 Worktrees and families

### The compounding proof's binding is seeded

*Honest limit; downstream of the summary-writer gap in §18.6.* The test proving
savings compound across family members has two assertions: the serving half is
proven end to end, and the second has to plant its binding at the seam a
production writer would use, because that writer does not exist. So
served-once-computed-once across members is proven, and *computing* the summary
automatically is not. Closing it takes the summary writer and nothing else.

**Read more.** [09](09-worktrees-and-families.md) §9.10.

### The ancestry-walk budget has never been measured on its own

*Not run.* The design budgets the branch-ancestry walk at a small number of
milliseconds *added* to a serve. What is published is the whole serve, with a
caveat stating exactly this; isolating the addition needs the same verb run
against a build without the branch axis, and no such build configuration exists.
The published serve figure sits comfortably inside its own bar, so nothing is
hidden — but the specific budget the design states has not been checked against a
number. Closing it takes a build fork the project declined to take, or a finer
instrument inside the existing one.

**Read more.** [15](15-benchmarks.md),
[09](09-worktrees-and-families.md).

### Two declared relations have no writer

*Never built (dead schema).* The capture store declares relations for raw shadow
branches and their events; nothing inserts into either. The dashboard's read
query over them is gone; the one remaining reader is `rift gc`, which reports
nothing to collect because there is nothing there. A cache for ancestry
verdicts is named in the design and does not exist at all.
Declared-but-unwritten relations sit on the
append-only registry, carry their generated immutability guards and appear in the
schema snapshot — so they look exactly as live as everything else, and only the
absence of a writer distinguishes them. Closing it means either a writer or a
removal, and removal is not free: the tables ride a frozen migration, so they can
stop being *used* but do not disappear from history.

**Read more.** [13](13-storage-and-schema.md),
[09](09-worktrees-and-families.md) §9.12.

---

## 18.9 Agent integrations

### The Gemini adapter drops the model identifier and under-declares events

*Measured defect.* Its adapter maps the one event carrying the model identifier
to zero canonical events, so a session is captured with no record of which model
ran; and it declares nine of the eleven events the agent publishes, so two land
as unrecognised verbs. These are ordinary bugs and **not** the reason the agent
is unwired — that is a scope decision, held apart on purpose: fixing the bugs
would not change the support decision, and the support decision does not make the
bugs disappear. Closing them takes two small adapter edits, whenever the agent
enters scope.

**Read more.** [10](10-agent-integrations.md) §10.6.

---

## 18.10 The dashboard

### The header's help affordance has no key

*Measured defect.* The header cluster draws a help affordance that is
hit-testable and that no key binding opens — the same parity problem the
savings breakdown once had, a hint teaching an act a keyboard cannot perform. Closing it takes
a binding, or a hint that does not advertise one.

**Read more.** [11](11-canopy-tour.md).

### A reorder hint teaches a spelling no key matches

*Measured defect.* The panel-editor hint shows shifted letter keys for
reordering; the live bindings are the shifted arrow keys. The hint is display
shorthand and reads as literal, so a user following it exactly gets nothing.
Closing it means spelling the hint the way the binding is spelled.

**Read more.** [11](11-canopy-tour.md).

---

## 18.11 Storage, and descriptions that no longer match

### Column encryption has no production writer

*Never built; downstream of the summary-writer gap in §18.6.* The
encrypted-column mechanism is built and proven end to end against the shipping
engine, with its own key-custody table. On a real repository the encrypted column
has no rows and the custody table is empty, because the only sensitive column
would hold summaries and nothing writes those. Closing it takes the summary
writer.

**Read more.** [13](13-storage-and-schema.md) §13.9.

### A rebuild helper has no shipped caller

*Never built.* The store has a helper that deletes a projection file and stamps
the next rebuild epoch, and the dashboard's poller reads that epoch and reacts
correctly — but no shipped command calls the helper: the capture rebuild verb
re-ingests into the existing file instead. The mechanism is real, tested, and not
the mechanism your rebuild uses, which is the answer if you are debugging why a
projection was not replaced. Closing it needs a decision about whether
replace-and-restamp is ever the right rebuild, and a caller if it is.

**Read more.** [13](13-storage-and-schema.md).

### Some comments still call worktree content hashing blake3

*Stale description.* Worktree file content hashing is plain SHA-256, so it
matches the hash recorded when the file was read; blake3 covers salted
identities, configuration and policy fingerprints, integration-artifact ownership
fingerprints, and frozen envelope identifiers. At least two source comments still
describe the first as blake3, and blurring a deliberate split — substrate
byte-compatibility on one side, internal handles on the other — will eventually
mislead somebody into the wrong identifier type. A second instance sits on the
recall plane, where doc comments state a stricter unchanged-state rule than the
code checks. Closing both means editing comments; the code is correct.

**Read more.** [13](13-storage-and-schema.md),
[06](06-recall.md), [17](17-invariants.md) rule 5.

### A task-runner illustration names a retired sub-command

*Stale description.* The task-runner files show an example invoking a recall
sub-command that no longer exists — running it exits with a usage error — because
the retired server transport it belonged to is gone. Closing it means changing
the illustration to a verb that exists.

**Read more.** [16](16-development.md).

### One registry path can hold the same family twice

*Honest limit.* The machine-global registry deduplicates a family by its main
work tree. When that tree cannot be proven from the checkout doing the
registering, the checkout's own path is registered instead, because registering
nothing would make the repository invisible in the dashboard — a deliberate trade
with a visible symptom, two rows where you expected one. Closing it needs a
better answer to "what do you register when you cannot see the family's main
tree", a design question rather than a bug.

**Read more.** [02](02-configuration.md),
[09](09-worktrees-and-families.md).

---

## 18.12 Sync readiness

Sync is the plane most explicitly *not finished*, and its chapter says so at
length. These are the specific items.

### With no remote configured, the queue accrues and nothing is pushed

*Honest limit, by design.* The push queue records what would be shipped. With no
checkpoint remote configured the sync verb refuses honestly and the entries
accumulate; cleaning removes none, because they reference live durable references
and are not stale by the cleaner's rule. A queue that only grows looks like a leak
and is not one: the references it names are durable in git, and the entries record
an intent nobody has satisfied. Closing it means configuring a remote, or
accepting the accrual as correct for a shipper with nowhere to ship. There is no
fallback to the repository's default remote, deliberately — pushing capture
references to a remote you did not nominate is not a default anyone should get by
accident. Queue depth is a status-verb and doctor fact only: the dashboard does
not surface it, though its data plane holds the number.

**Read more.** [14](14-sync-readiness.md) §14.4.

### One consumer does not persist its stream position

*Honest limit.* The stream cursor pairs a chain position with a checkpoint
identifier. One consumer does not persist the position, so the guard arm that
would catch a rewritten stream by comparing positions cannot fire for it; what
catches a rewrite there instead is presence — a recorded tip the walk no longer
holds resolves to no cursor, the ingest falls back to a full pass, and says so. A
full pass is *correct*, the apply being a set union over content-addressed rows,
and merely slower, which is why the missing position is not an error. Closing it
takes an additive column on a canonical relation: a deliberate schema decision on
an append-only store, not a drive-by edit.

**Read more.** [14](14-sync-readiness.md) §14.3.

### Bundle import is references-only

*Never built.* Importing a bundle attaches its references. The store-side entry
point that would import a stream segment is never called by the binary, and a
rebuild after an import is a required step rather than an optional tidy-up — so
"imported" means the objects and references are present, not that the projection
reflects them. Closing it means calling the segment import from the bundle verb,
or documenting the rebuild as the second half of import forever, which is what
the chapter does today.

**Read more.** [14](14-sync-readiness.md) §14.5.

---

## 18.13 Benchmarks

### The throughput bars are set, not re-derived

*Honest limit.* Three throughput bars were derived from measured runs and are
now carried as ratified set values, their artifacts kept as **records** rather
than current measurements; a bar backed by a record is not judged for staleness,
because a record does not claim to be current. The numbers are real and were
measured, and are no longer traceable to a live subject you can re-run — a weaker
provenance, labelled as one in the bars file and its status column. Closing it
takes a fresh sweep on clean subjects under the published protocol.

**Read more.** [15](15-benchmarks.md) §15.8.

### Six paired arms have been executed exactly once

*Not run (properly).* Six opt-in arms exist for the newline-dense capture cases;
one published sweep carries all six as single-run medians, and two of them landed
over the latency budget in that run. A single run is a measurement, not a verdict
— the protocol requires three clean runs and a nearest-rank median — so those two
over-budget figures should not be cited as a breach and the arms should not be
cited as passing. Closing it means running them under the protocol, three clean
runs each, load ceiling respected.

**Read more.** [15](15-benchmarks.md) §15.15.

### An unexplained throughput drop was recorded and never explained

*Honest limit / open question.* Between two sweeps, import throughput fell
noticeably on a path no change touched. The bar was lowered to match the
measurement, the drop was recorded as report-only, and the cause was never
identified — the same discipline as the redaction-cost disagreement: a number
that moved without a cause is written down rather than smoothed. Closing it takes
a bisect across the sweeps, or an accepted attribution to machine conditions.

**Read more.** [15](15-benchmarks.md).

### The attribution proof rests on a fixture, not on this repository's spans

*Honest limit.* Attribution on this repository's own history still reads
`unknown` for the files most edited, because those edits came from shell workers
in linked members: no agent hooks fired, so no boundaries were snapshotted. The
acceptance statement is therefore proven by an end-to-end fixture driven through
the real binary, not by real spans on real history — the mechanism is proven, the
*field* evidence constructed. Closing it takes a stretch of ordinary agent-driven
work on this repository and a re-run of the census.

**Read more.** [03](03-capture.md),
[15](15-benchmarks.md).

---

## 18.14 Guards and checks

The guards here are unusually strong, which is exactly why their residual
weaknesses are worth naming: a guard everyone trusts is the most expensive place
for a silent hole.

### A screen that cuts at a marker can be shrunk by a comment

*Honest limit.* Several checks screen only the production half of a source file
by cutting at the first test-module marker. Each anchors on a joined, column-zero
sequence rather than a bare token, so an indented marker — or one inside a string
or a comment — no longer truncates the screen: everything the anchor does not
recognise is *screened* rather than exempted. The residual, stated rather than
papered over: a string literal or block comment containing the **exact** joined
sequence at column zero still shrinks the screened half. No file contains one, and
a check exists purely to assert the residual is exactly as wide as documented, so
if it ever fails the guard got *stronger*. Closing it takes a parser rather than a
text cut, adding a second normalising parse to a codebase where the recurring
defect has been two parses drifting apart. The trade was deliberate.

**Read more.** [16](16-development.md).

### One module's screens are source-blind and lean on the formatting check

*Honest limit, deliberately taken.* The screens in that module read source text
rather than a parse, and rely on the formatting check to have normalised
whitespace first, so an item exempted from formatting with unusual spacing under
it slips through. That was accepted rather than overlooked: they are one module's
own tests, run by the same check as everything else, guarding a module whose
author is the person editing it — and widening the matcher would add a second
normalising parse to the place where every defect so far has been two parses
drifting apart.

**Read more.** [16](16-development.md).

### One required-job list is narrower than the declared list

*Honest limit.* The workflow declares fifteen jobs; fourteen are on the required
list. The release build is declared and not required, so a failure there is
advisory — a choice, and one that should be visible. Closing it means adding it
to the required list, or stating the reason it is advisory where the list is
defined.

**Read more.** [16](16-development.md) §16.4.

### There is no workspace-wide clock abstraction

*Honest limit.* The determinism convention says to inject the clock. There is
exactly one clock trait in the workspace, local to the dashboard's splash
sequence; elsewhere determinism comes from injecting identifiers and randomness
and from not asserting on time. Relatedly, "no test reads the wall clock" holds
for **assertions** — several tests read it for poll deadlines and a handful of
fixtures seed from it, but no assertion depends on either. The convention as
written is stronger than the code; the property that matters does hold. Closing
it takes either a workspace clock seam or a convention statement scoped to
assertions, and the second is closer to the truth.

**Read more.** [16](16-development.md),
[17](17-invariants.md).

### Some checks are advisory or opt-in and are not run locally

*Not run, by design.* The undefined-behaviour interpreter leg is advisory, and
the fuzz smoke and its nightly counterpart have their own budgets and run in
continuous integration, not in the local suite. A green local run does not mean
every check in the project passed — it means the required ones did.

**Read more.** [16](16-development.md).

---

## 18.14b Agent detection

### The non-`PATH` install locations are a fixed list

Detection ([10](10-agent-integrations.md) §10.5) finds an agent that is not on
`PATH` by looking where its vendor installs it: `~/.local/bin` for Claude
Code's native installer, the platform's application roots for the Antigravity
and Codex desktop bundles, four editors' extension roots, three platforms'
JetBrains plugin roots. Each is a path Rift knows today, verified by a manifest
where one exists, and a vendor that moves is a vendor Rift stops seeing until
the table is edited. The failure is the honest one — `absent`, and a row you
can still tick by hand — but it is a table, and tables age.

**What would close it:** nothing structural; the entry is here so that a
"Claude Code is installed but Rift says absent" report is read against this
table first.

### A JetBrains plugin is "installed", never "runnable"

The Claude Code JetBrains plugin ships no binary, so its evidence can wire an
agent but never yields a version Rift could probe, and its verification is the
weakest of the four: a plugin directory of the right name holding a jar. A
leftover directory after an uninstall that kept its jar would still read as
installed. The row says `JetBrains plugin` rather than `present`, so the claim
is inspectable where it is made.

---

## 18.15 How this list is meant to be used

**An item is listed with what was demonstrated, not what was feared.** A worry
with no demonstration behind it does not earn a row.

**Closing an item is a repository-wide edit, not a local one.** The same shape
usually appears in more than one place: an item is closed when the shape is gone,
not when one site is patched.

**A site "fixed" with a vacuous assertion is worse than one still listed.**
Calibrate every fix against a restored defect: break the thing on purpose and
watch the check go red.

> Tip: the list is the deliverable, not the count. Do not summarise this chapter
> into a number — "about fifty open items" tells you nothing, where the shape of
> any one of them tells you a great deal. The map is [00](00-start-here.md), the
> standing rules [17](17-invariants.md), the terms [19](19-glossary.md), and the
> guarantees that are *not* gaps [../../SECURITY.md](../../SECURITY.md).
