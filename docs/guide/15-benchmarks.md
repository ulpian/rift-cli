# 15. Benchmarks

Every performance number in this guide comes from a tracked artifact that a
harness wrote. Nothing is typed by hand except the prose around it.

That sentence is the whole design. A benchmark is easy to get wrong in ways
nobody notices: you time a binary that predates the code you are claiming
about, you compare today's run against last week's control, you publish a rate
whose denominator nothing counted, you quote a number taken on a machine that
was compiling something else. Each produces a real, reproducible, completely
misleading figure. So the apparatus spends most of its effort on making those
four mistakes *mechanically detectable* — and on recording, beside every
number, what it is and what it is not.

> **Tip:** if you only want the numbers, read §15.8; if you are about to
> re-measure, read §15.4, §15.5 and §15.14 first.

---

## 15.1 Where the evidence lives

| Path | What it holds |
|---|---|
| [`benches/native-baseline/`](../../benches/native-baseline) | The harness: a standalone cargo package that produces the numbers. |
| [`benchmarks/`](../../benchmarks) | The results: artifacts, declared bars, and the documents that reduce them. |

Under `benchmarks/`, three families of artifact plus the declaration they are
judged against:

| Family | One artifact is | Where |
|---|---|---|
| capture-scenario runs | one pass over the scenario catalogue — many rows, generated fixtures, host, protocol, load reading, subject provenance | `benchmarks/native-baseline/` |
| git-latency sweeps | one sweep of what a `git commit` costs beyond a bare commit, arm by arm, against that run's own control | `benchmarks/hook-latency/` |
| displacement sweeps | one paired sweep: enable-then-commit and commit-without-a-prior-enable, in the same run | `benchmarks/enable-displacement/` |
| the bars | [`benchmarks/floors.toml`](../../benchmarks/floors.toml) — the declaration published numbers are judged against | read by the guard |

Every artifact is a JSON record with a Markdown twin, written by the same code so
the two cannot drift. Two documents reduce the families:
[`benchmarks/README.md`](../../benchmarks/README.md) carries the across-families
summary, and [`benchmarks/hook-latency.md`](../../benchmarks/hook-latency.md)
the git-latency family plus a long record of what that instrument found and
corrected about itself.

One file there is not a performance artifact: `redaction-fp-baseline.md` is the
redaction false-positive census over this repository's own text — a
*regression* record, not a speed record. See
[04-redaction-and-secrets.md](04-redaction-and-secrets.md).

---

## 15.2 The harness

`benches/native-baseline` is a standalone cargo package with its own workspace
root and lockfile. Two deliberate properties:

- **It links no Rift crate.** It drives the *binaries* as a black box — spawning
  `rift`, reading its stdout, inspecting the files it wrote — because a harness
  linking the product could measure a path the shipped binary never takes.
- **It takes almost no dependencies**: argument parsing, serialization, error
  types. A large dependency graph measures itself.

Nine verbs:

| Verb | What it does |
|---|---|
| `scenarios` | Print the catalogue and what each row waits on. |
| `fixtures list` / `fixtures build <name>` | Inspect or build the generated repositories. |
| `doctor` | Say which binaries resolve here, and why the others do not. |
| `run` | Drive scenario rows and write an artifact. |
| `hook-latency` | Measure what a commit costs beyond a bare commit. |
| `enable-displacement` | Measure where the setup cost sits: at enable, or at the first commit. |
| `facts` | Regenerate a document's generated block from its own artifacts. |
| `audit` | Check that published numbers, bars and provenance agree. Non-zero on any disagreement. |
| `ratchet` | Compare two git-latency artifacts as ratios. Report-only (§15.13). |

All of them run from inside the package:

```bash
cd benches/native-baseline
cargo run --release -- scenarios
```

> **Tip:** `scenarios`, `fixtures list`, `doctor`, `facts --dry-run` and `audit`
> are read-only about your machine; the three measuring verbs write into the
> tracked `benchmarks/` tree and want a quiet machine.

---

## 15.3 The measurement protocol

**The timed region is spawn through exit.** Startup is not overhead to be
subtracted; for a hook it is a large part of the cost a user pays.

**Warmups are discarded, measured iterations kept individually**, in three
shapes each row declares for itself:

| Shape | Counts | Used by |
|---|---|---|
| latency | 3 warmup + 30 measured | sub-second verbs: hook rows, `status`, `checkpoint list` |
| heavy | 1 warmup + 3 measured | multi-second work: condense, import, a whole scripted session |
| sweep | 3 warmup + 11 measured commits per arm | the git-latency and displacement sweeps |

**Percentiles are nearest-rank.** The median and the 95th are *observed
iterations*, never interpolations, and the sample count is published beside every
statistic, so a 95th over thirty samples reads as "the worst couple".

**A failed iteration is counted, not hidden:** any non-zero exit inside a measure
marks it failed rather than fast.

**Peak memory is taken in a separate, untimed pass**, because the platform
timing utility would otherwise sit inside the measurement. So is the pass
counting child processes per invocation — it perturbs what it observes, so only
rows whose *claim* is a process count ask for it.

**Work is counted, not assumed.** A rate needs a denominator, and the harness
probes for it rather than trusting the script: the condense row counts the
canonical events the projection holds, the import row uses the product's own
count of importable shards, the verify row counts checkpoints. `rift verify` over
a repository with no checkpoints exits quickly and correctly and its wall time is
a beautiful number about nothing; the probed denominator prints `0 checkpoints`
beside it. Where nothing counts the work the field is absent — honest, and not
the same as zero.

**Ingest is proven, not assumed.** A row that was supposed to write something
durable must show it did; publishing the wall time of a hook that quietly did
nothing is the self-flattery this apparatus prevents. The one exempt row is the
replay row, where writing nothing is correct, and it is never presented as the
round trip.

**Isolation is declared per row.** A row runs either against one prepared
repository for every iteration (read-only verbs, or where accumulating state is
realistic) or against a fresh copy per iteration — mandatory for verbs that
write, and the only way a "cold" measure is cold.

### Fixtures are generated, never cloned

A fixture repository is a pure function of its specification: the same
specification produces the same file bytes, timestamps and therefore commit ids
on any machine — what makes two runs on two machines comparable, and what keeps
the harness free of a network dependency.

| Preset | Shape | Scripted turns |
|---|---|---|
| `small` | 3 directories × 4 files, 8 commits | 20 |
| `medium` | 20 × 25 files, 200 commits | 10,000 |
| `large` | 400 × 25 files, 40 commits | 200 |

Checkpoints are deliberately *not* pre-generated: writing them is the thing
being measured. Fixtures are cached under a directory keyed by a digest of the
specification, so re-running does not rebuild them.

### The process plane is hermetic

Every child the harness launches goes through one prepared environment that:

- points `git` at stub configuration files the harness writes — never your
  `~/.gitconfig` — pinning identity, line endings, automatic gc and the default
  branch;
- scrubs the inherited `GIT_*` variables that silently re-target a repository
  (`GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, the alternates, and the rest);
- forbids network protocols and disables terminal prompting;
- fixes the timezone and locale so timestamps and sorting cannot drift.

Repository hooks are disabled while fixtures are generated — fixture building
must be inert — and enabled while scenarios run, because condensation at commit
time *is* one of the measures and it rides the repository's own hooks.

The scripted agent replaying a session for the condense row is a small separate
binary (`vogon`) built from this checkout. No number is published about it; it is
a driver, held to the same freshness rule as the subject, because a stale driver
scripts the session an older adapter agreed with.

---

## 15.4 Clean subjects: is this binary the code you are claiming about?

A benchmark that times a binary it did not build is not a benchmark of the tree
it is published against, and nothing in the timing says so — this project has the
defect on record twice, once with a release build 168 source files behind the
tree it would have been published against. So the question is answered
mechanically, before any measurement:

1. **The subject is resolved deliberately.** An explicit path in
   `$RIFT_BENCH_RIFT_BIN`, else the release build in this checkout's `target/`,
   else `rift` on `PATH`.
2. **A debug build is refused.** A number from an unoptimized binary is not a
   benchmark, and silently accepting one is how false claims get published.
3. **A stale binary is refused, by name.** The harness scans the newest
   modification time across everything a build would recompile from — crates,
   binaries, manifests, lockfile, toolchain file — and refuses a binary older
   than that, naming the file that outran it.
4. **The subject's provenance is recorded into the artifact**: the commit its
   checkout described, and whether that checkout had uncommitted changes.

The check is conservative in one direction only: touching a source without
rebuilding marks the binary stale, costing you a rebuild, while a build landing
after every source is never marked stale. Both refusals have an escape hatch — and taking it *stamps the artifact*.
`RIFT_BENCH_ALLOW_DEBUG=1` or `RIFT_BENCH_ALLOW_STALE=1` lets the run proceed,
and the leg's recorded source string then says so, so a report taken that way
carries the admission instead of hiding it.

---

## 15.5 Was the machine quiet?

The older sweeps attest idleness *in prose* — "one machine, one session, idle" —
and every such sentence was written by the person who also wanted the numbers to
be good, so none could fail. The load average is now **read per arm, into the
artifact**, against a stated ceiling.

- The ceiling is **one-minute load average 8.0**, derived from this project's own
  evidence rather than picked: runs between 5.7 and 8.8 produced a curve that
  could be reasoned about, while runs between 12.0 and 16.7 produced one that
  could not — every step's spread overlapped every other's, and a larger corpus
  measured *faster* than a smaller one. Eight is also this host's logical CPU
  count, where a run stops having a core to itself.
- **Above the ceiling a run is void as a source of a bar** — not deleted, not
  hidden, still published: a real measurement of a busy machine, which may not set
  a bar other runs are judged against.
- **An unread load is void too.** A reading the harness could not take is not
  evidence that there was nothing to read.
- Exactly *at* the ceiling is admitted: the rule is "above". For a sweep whose
  arms run minutes apart, one busy arm voids the whole run, because a bar is one
  number over a whole run.

---

## 15.6 What a number is compared against

**Absolute bars, never another implementation.** The harness has a second leg
slot and it is empty: the binary it was once written to compare against was
deleted from the project, the harness never downloads or provisions anything,
and the leg defaults to off. Every published number here is a single-leg
measurement of Rift judged against a stated bar.

That is a deliberate narrowing. A ratio against a binary nobody can obtain is
unreproducible; a ratio against a *developer's own* copy of something is worse,
because it looks reproducible and is not. An absolute bar with a published
protocol is checkable by anyone with the same kind of machine. Where a verb has
no analogue the row records that and never a loss, and a scenario that is not
wired says so instead of publishing a zero.

---

## 15.7 Two bars of 100 ms, and they are not one bar

**Git latency** is what a `git commit` costs *beyond a bare commit*, summed over
the three commit-path hooks: a displacement, measured against a bare-commit
control taken in the same run, bar 100 ms, hard, set by the project owner rather
than derived. **The agent-hook budget** is the wall time of one
`rift hooks <agent> <verb>` process including its ingest: median under 100 ms,
95th under 500 ms — a per-process figure, not a displacement.

A number from one must never be quoted against the other; this is the most
quotable mistake in the area. [03-capture.md §3.8](03-capture.md) carries the
per-verb table and the three rows that once breached the agent-hook budget.

---

## 15.8 The bars, and where each one stands

[`benchmarks/floors.toml`](../../benchmarks/floors.toml) is the declaration.
Each row names a scenario and states one bar, with what is needed to read it
honestly:

- `kind` — throughput or latency; they fail in opposite directions, so more work
  per second is not thereby better and a slower commit is not thereby doing more.
- `status` — how much authority the number carries (below).
- `machine_class` — an absolute moves roughly twofold with load, so a bar
  without its machine class is unquotable.
- `protocol` — the iteration shape behind it.
- `caps` — the product limits in force; a number under different limits is about
  a different product.
- `measured_paths` — the repository paths that decide what this scenario
  measures.
- `note` — anything else the row needs said, carried verbatim, never
  interpreted.

Three statuses exist, each costing something different:

- **placeholder** (literal status `underived`) — the structure exists, the
  number does not. The guard reports the row as pending, with the command that
  would fill it, and it binds nothing. It is never a pass.
- **derived** (literal status `derived`) — a number following arithmetically
  from recorded per-run medians. The guard recomputes the arithmetic and reds if
  the two disagree, so a hand-typed number cannot quietly stop being derived.
- **owner-set** (literal status `owner-ratified`) — a bar somebody decided. Not
  derived, not re-derivable, and no measurement moves it.

### The derivation convention, and what the halving is not

Where a throughput bar *is* derived, the convention is:

```
bar = min(per-run medians over at least three fresh runs at the shipped caps)
      × 0.5, rounded down to at most two significant figures
```

**The halving is not a noise budget**, and the file says so where it is read
rather than in a log nobody opens. Run-to-run variance on this hardware has been
measured reaching +98% between two identical runs of the same binary; no factor
of two absorbs that. The halving is a *publication convention*, so a tracked
artifact can be checked for internal consistency without the check becoming a
live performance gate that reds on machine weather.

### Today's rows

All four rows are **owner-set bars**, ratified by the project owner on the day
this repository's history was collapsed to a single fresh root: their
derivations named commits that no longer exist here, and publishing a derivation
nobody can re-run is worse than publishing a decision somebody made. So the
numbers stayed, the derivation lists were emptied, and the rows say plainly that
a person set them.

| Scenario | Bar | Most recent published median |
|---|---|---|
| import throughput | ≥ 71 shards/s | 156.21 shards/s |
| verify throughput | ≥ 27 checkpoints/s | 56.22 checkpoints/s |
| condense throughput | ≥ 11 events/s | 24.92 events/s |
| first commit after enable | ≤ 100 ms of git latency, hard | 30.20 ms |

Each median is a nearest-rank median across runs of each run's own median, over
the three published runs of each family.

The displacement row's claim under test is not speed. Setup work was deliberately
moved to `rift enable` time, and displacement is invisible from one direction: a
cheap first commit alone is equally consistent with "the cost moved" and "the
cost never existed". So both orders run in one sweep — the first commit in a
repository enabled beforehand, and the first commit in one whose hooks are
installed but whose projection state is not — and the difference is the displaced
cost, measured rather than argued. The second arm is a *faithful simulation*
(there is no supported way to install the hooks without initializing, since
`rift init` does both), and it carries that admission as a field of the
artifact.

---

## 15.9 The consistency guard

The guard answers exactly one question: **do the published numbers, the
published bars and the published provenance agree with each other?** It never
answers "is this machine fast today", and its own output says so — with variance
reaching +98% between identical runs, a live performance gate would fail on
weather and teach everybody to ignore it.

Six arms, each calibrated by planting the exact defect it exists to catch:

| Arm | Catches |
|---|---|
| enumeration | A scenario that owes a bar and has no row — a dropped row must not leave a green gate behind it. |
| the declaration itself | A bars file that does not hold together: a placeholder stating a number, a latency row claiming to be derived, a derived row with fewer than three medians, more medians than artifacts, or a stated number its own medians do not imply. |
| violation | A measured value that misses a bar which binds. |
| missing artifact | A binding bar with nothing measuring it — a bar with nothing to judge cannot fail. |
| staleness | An artifact claiming to be current that is not: no provenance, a subject this history does not contain, a subject that is not an ancestor of `HEAD`, a subject predating `HEAD`'s last change to that scenario's measured paths, or a bar derived from a record or from a run over the load ceiling. |
| generated-block disagreement | A generated summary that disagrees with the artifacts it names, in either document. |

```bash
cd benches/native-baseline
cargo run --release -- audit
```

It prints how many artifacts it read, how many findings and how many rows are
pending, then one line per finding naming both sides of the disagreement, and
exits non-zero on any finding. The artifact count is part of the verdict, because
an empty directory reported as clean is the failure that count exists to prevent;
today it reads twenty-five artifacts. The same guard runs as an ordinary test in
the harness package, so that package's CI job carries it — see
[16-development.md](16-development.md) for the job list.

---

## 15.10 Records: kept, quotable, barred from setting a bar

An artifact is a **record** when it has no commit provenance, or was taken over
a tree with uncommitted changes. A record is **kept** (rewriting one to satisfy a
check is the failure this apparatus exists to prevent), **quotable as history**
with its date and conditions, and **barred from deriving a bar** — also exempt
from the staleness arm, because being superseded is what a record means.

Today **every tracked artifact is a record.** The oldest eighteen git-latency
sweeps were always records: taken before the working-tree capture limits were
cut (roughly five hours after the last of them), under an artifact schema
carrying no provenance at all. They are the evidence that *produced* those
limits and are kept exactly as measured. The rest became records when the
history holding their subject commits was collapsed: an artifact naming a commit
this repository no longer contains cannot have its currency checked, and unknown
is not fresh. That is why the four bars are owner-set (§15.8): no artifact
without provenance may set a bar, and the rule is enforced by resolving each
named artifact rather than by trusting a sentence.

---

## 15.11 Caveats travel with their numbers

A caveat living only in prose beside a table is one a reader can quote the number
without, so the important ones are *fields of the serialized record*: anything
reading an artifact reads the qualification, and the generated summaries copy
them verbatim. Three rows carry one today.

**Import throughput measures our reader, not the format.** No pre-Rift artifact
was ever obtained or is obtainable; what exists is a reconstruction — a frozen
golden shard replicated under distinct ids, every spelling cited to a
declaration that already lived in this repository. The rate is true about *our
own reconstruction of the on-disk format* and silent about the format itself: it
measures the walk, the parse and the root re-derivation. Never a compatibility
claim.

**The ancestry row is the whole serve, not the walk.** The cost the
branch-containment rule adds is a *difference* between this verb with the branch
axis and the same verb without it; this row is one of the two, so quoting it as
the cost of containment quotes a total as a delta. See
[09-worktrees-and-families.md §9.9](09-worktrees-and-families.md).

**The compounding row publishes a numerator, not a ratio.** Its unit is the
tokens one serve in a linked checkout avoided out of the family's shared store —
reads that checkout never captured. A ratio needs both numerators; this is one.

### Honest absence

A row no clean run has measured says so — null medians, zero runs, a rendering
reading *not yet measured* — instead of publishing a zero, and a row *declared*
but not yet measured is **absent** from the generated summary altogether. A test
retires that state automatically: the moment a clean run for such a row exists in
the tracked tree, it reds until the row is published. So the field can defer a
number and can never suppress one.

---

## 15.12 The generated summaries

Both reducing documents end in a generated block, and those blocks are the only
numbers in them anything is gated on: **generated, never typed**, with the guard
recomputing each from exactly the artifacts it names. There is **one home per
number** — the git-latency arms live in the block in `hook-latency.md`, and the
across-families block in `benchmarks/README.md` carries only their counts, so the
two documents cannot disagree about the same figure.

```bash
cd benches/native-baseline
cargo run --release -- facts                  # the git-latency block
cargo run --release -- facts --kind report    # the across-families block
```

Add `--dry-run` to print instead of write.

> **Tip:** never edit a generated block by hand — the guard reds on a block that
> disagrees with the artifacts it names. If a number looks wrong, the fix is a
> new run, not an edit.

---

## 15.13 The ratio comparison is report-only

`ratchet` compares two git-latency artifacts arm by arm, as *control-relative
ratios* rather than absolutes: if the machine was uniformly slower, both the arm
and its control move together and the ratio cancels; a real regression does not.
The default tolerance is 10% of growth in that ratio.

**It exits zero on a regression.** That is a decision, not a bug. At a 10%
tolerance on this hardware it flags five arms out of five between two identical
runs of the same binary, so an enforcing gate would be noise with a red light
attached. `--enforce` makes it exit non-zero, keeping the enforcing path
exercised for a better threshold later.

---

## 15.14 Re-measuring, end to end

Nothing here needs the network.

**1. Quiet the machine.** Close the dashboard, stop other builds, let the load
average settle: a reading above 8.0 makes the run unusable as a source of a bar.

**2. Build the subject in release** (§15.4), then ask `doctor` what resolves — a
stale or debug binary is refused there rather than measured.

```bash
cargo build --release -p rift
cargo build --release -p rift-shadow-hooks --bin vogon   # only for the condense row
cd benches/native-baseline
cargo run --release -- doctor
```

**3. Measure what you need.** Each verb writes into the tracked directory for
its family:

```bash
cargo run --release -- run --scenario <row>     # one catalogue row (default: all)
cargo run --release -- hook-latency             # git latency, all arms + the control
cargo run --release -- enable-displacement      # both orders, one run
```

`run` accepts `--scenario` repeatedly, `--iterations`/`--warmup` to override the
shape, and `--out`/`--run-dir`/`--fixture-dir` to keep output and sandboxes
elsewhere. The sweeps default to 11 measured commits after 3 warmups per arm and
accept `--arms` to narrow the set — the bare-commit control always runs
regardless, because an arm quoted against a control from a *different* run
compares machine weather rather than code.

**4. Regenerate the blocks** (§15.12). **5. Audit** (§15.9): expect zero
findings, zero pending, and an artifact count showing it was not looking at an
empty directory.

**6. Read the staleness findings, if any.** The most common after a real change
is *"measures `<row>` at `<subject>`, which predates `<commit>`"*: the code
deciding that measurement moved after the run was taken. The answer is a new run,
or an explicit decision to keep the old one as a record.

> **Tip:** re-measuring updates the published medians and the guard checks them
> against the bar; it never moves the bar (§15.8). Moving one is a decision a
> person makes.

---

## 15.15 What these numbers cannot tell you

Stated plainly, because a benchmark chapter that omits this is advertising.

- **They are one machine class**: an eight-core x86-64 macOS host, where an
  absolute moves roughly twofold with load. Nothing generalizes into a claim
  about your machine.
- **Process launch is an implementation term, not a hardware floor.** A fresh
  Unix install dispatches each unchained hook through a direct executable link; a
  preserved user-hook chain and non-Unix installs keep a costlier shell shim.
- **The byte columns in the historical git-latency record are declarations, not
  measurements.** The probe meant to have the product confirm how many bytes a
  hook read returned nothing in every one of the 135 recorded values across those
  eighteen sweeps. The timings are real; the byte figures are the harness's own
  arithmetic over the corpus it wrote. A probe that fails silently is worse than
  no probe: it occupies the slot where a check would go.
- **Parts of that record describe an instrument that no longer exists.** The
  scripts its protocol section named were never committed and are unrecoverable,
  and one arm measured an execution route since removed. Those sections are kept
  as a record of what was measured and of a real problem found and fixed, not as
  a protocol a second person can repeat; the tracked replacement is the
  `hook-latency` verb.
- **Six paired opt-in arms have been executed exactly once.** Sized for the
  current working-tree capture limits, one published sweep carries all six as
  single-run medians — two of the newline-dense arms landed over the 100 ms
  budget in that run, so they are a measurement, not a verdict. They are
  declared in the instrument so it states what it is prepared to measure, and
  the document's prose about them is pinned against the code by a test, so the
  next re-size reddens the prose instead of outliving it.
- **No cap makes the git-latency budget safe at the 95th percentile.** That was
  the finding of the sizing work behind the current limits, recorded rather than
  smoothed over.
- **Savings and cost figures are not in this chapter.** They come from a
  different instrument with a different honesty ladder:
  [08-cost-and-savings.md §8.13](08-cost-and-savings.md) has the measured share, the
  staged bar the harness raises, and what may be claimed.

---

## 15.16 Related chapters

[03-capture.md](03-capture.md) (the two latency budgets, per verb) ·
[04-redaction-and-secrets.md](04-redaction-and-secrets.md) (the census sharing
this directory) · [08-cost-and-savings.md](08-cost-and-savings.md) (accounting
and the savings bar) ·
[09-worktrees-and-families.md](09-worktrees-and-families.md) (compounding, and
the containment cost the caveats refer to) ·
[16-development.md](16-development.md) (the CI jobs) ·
[17-invariants.md](17-invariants.md) (the standing rules quoted here) ·
[18-known-gaps.md](18-known-gaps.md) (rows still unmeasured) ·
[`benchmarks/README.md`](../../benchmarks/README.md) (the tracked evidence).
