# 15. Benchmarks

Every performance number in this guide comes from a tracked artifact that a
harness wrote; nothing is typed by hand except the prose around it. A benchmark
goes wrong unnoticed when you time a binary that predates the code you claim
about, compare against a control from another run, publish a rate whose
denominator nothing counted, or quote a number from a busy machine. The apparatus
makes those four mistakes *mechanically detectable* and records beside every
number what it is and is not.

---

## 15.1 Where the evidence lives

[`benches/native-baseline/`](../../benches/native-baseline) is the harness, a
standalone cargo package. [`benchmarks/`](../../benchmarks) holds the results:
capture-scenario runs (`native-baseline/`), git-latency sweeps (`hook-latency/`:
what a `git commit` costs beyond a bare commit, against that run's own control),
displacement sweeps (`enable-displacement/`, both orders in one run), and
[`floors.toml`](../../benchmarks/floors.toml), the declaration they are judged against. Each artifact is JSON plus a Markdown twin;
[`benchmarks/README.md`](../../benchmarks/README.md) and
[`benchmarks/hook-latency.md`](../../benchmarks/hook-latency.md) reduce them.
The redaction census (`redaction-fp-baseline.md`) is a regression record, not a
speed record; its rules are [§4.9](04-redaction-and-secrets.md#49-the-false-positive-census).
[`benchmarks/replay-savings/`](../../benchmarks/replay-savings) holds the
transcript replay of
[Chapter 8](08-cost-and-savings.md#813-the-bar-is-what-the-harness-measured),
aggregates only, its declined share published over its denominator.

---

## 15.2 The harness

It **links no Rift crate**: it drives the *binaries* as a black box, because a
harness linking the product could measure a path the shipped binary never takes.
It
takes almost no dependencies, since a large graph measures itself. Verbs (`cargo run
--release -- <verb>` in `benches/native-baseline`): `scenarios`, `fixtures`, `doctor`,
`run`, `hook-latency`, `enable-displacement`, `facts`, `audit`, `ratchet`; the three
measuring ones write into `benchmarks/`.

---

## 15.3 The measurement protocol

**The timed region is spawn through exit** (startup is much of a hook's cost). **Warmups are discarded and measured iterations kept individually**, in three
shapes each row declares: latency (3 + 30), heavy (1 + 3) and sweep (3 + 11
commits per arm). **Percentiles are nearest-rank** (observed iterations, never interpolated), sample
count beside each; a non-zero exit marks an iteration failed, not fast. Memory and child counts use separate untimed passes.

**Work is counted.** A rate's denominator is probed, not trusted (`rift verify` over zero checkpoints is
fast and a number about nothing, so the count prints beside it); where nothing
counts the work the field is absent, not zero. **Ingest is proven** for rows meant to write. **Isolation is declared per row**:
one repository for read-only verbs, a fresh copy per iteration for writers, the only way "cold" is cold.

**Fixtures are generated, never cloned**: a pure function of their specification
(`small`, `medium`, `large`), so commit ids match across machines; checkpoints are
not pre-generated, since writing them is the measure. **The process plane is hermetic**: git reads stub configuration, never
`~/.gitconfig`; re-targeting `GIT_*` variables are scrubbed; the network is
forbidden; timezone and locale fixed. Hooks are off while fixtures build and on
while scenarios run, because condensation at commit time *is* a measure.

---

## 15.4 Clean subjects: is this binary the code you are claiming about?

A binary the tree did not build says nothing about it and the timing never
reveals it, so the harness checks first. **The subject** is `$RIFT_BENCH_RIFT_BIN`, else this checkout's release build, else
`rift` on `PATH`. **A debug build is refused.** **A stale binary is refused, by
name**: the harness compares the newest modification time of everything a build
recompiles from with the binary's and names the file that outran it. **The subject's provenance is recorded**: the
commit and whether the checkout was dirty. `RIFT_BENCH_ALLOW_DEBUG=1` or `RIFT_BENCH_ALLOW_STALE=1` lets a run
proceed and *stamps the artifact's leg source string*, so the report carries the
admission.

---

## 15.5 Was the machine quiet?

Prose idleness claims are written by the person who wants good numbers, so the
one-minute load average is **read per arm into the artifact** against a ceiling
of **8.0**, derived from evidence: runs at 5.7 to 8.8 gave a curve that could be reasoned
about, runs at 12.0 to 16.7 one that could not (a larger corpus measured
*faster*). Eight is the host's logical CPU count. **Above the ceiling a run is void as a source of a bar**, though
still published; so is an unread load. One busy arm voids a sweep, since a bar is one number over a run.

---

## 15.6 What a number is compared against

**Absolute bars, never another implementation.** The second leg slot is empty and
off: the harness never downloads or provisions anything. A ratio against a binary
nobody can obtain, or a developer's own copy, only looks reproducible.

**Two bars of 100 ms are not one bar.** *Git latency* is what a `git commit` costs
beyond a bare commit, summed over the three commit-path hooks: a displacement
against a same-run bare-commit control, hard, **set by the owner** rather than
derived. *The agent-hook budget* is the wall time of one `rift hooks <agent>
<verb>` process including ingest: median under 100 ms, 95th under 500 ms. Never quote one against the other ([Chapter 3](03-capture.md)).

---

## 15.7 The bars, and where each one stands

[`floors.toml`](../../benchmarks/floors.toml) gives each row one absolute bar with
its `kind` (throughput and latency fail in opposite directions), `status`,
`machine_class`, `protocol`, `caps` (other limits, another product),
`measured_paths` and a verbatim `note`. A **placeholder** (`underived`) binds
nothing and is reported pending, never a pass; a **derived** bar follows from
recorded per-run medians that the guard recomputes, so a hand-typed number cannot
quietly stop being derived; an **owner-set** bar (`owner-ratified`) is a decision,
moved by no measurement. A derived throughput bar is `min(per-run medians over at
least three fresh runs) × 0.5`, rounded down to two significant figures. **The
halving is a publication convention, not a noise budget** (variance has reached
+98 % between two identical runs), so a tracked artifact can be checked for
consistency without a live gate that reds on weather.

All four rows are owner-set (a derivation nobody can re-run is worse than a
decision). Medians are nearest-rank across the three published runs:

| Scenario | Bar | Published median |
|---|---|---|
| import throughput | ≥ 71 shards/s | 156.21 shards/s |
| verify throughput | ≥ 27 checkpoints/s | 56.22 checkpoints/s |
| condense throughput | ≥ 11 events/s | 24.92 events/s |
| first commit after enable | ≤ 100 ms of git latency, hard | 30.20 ms |

The displacement row's claim is not speed: a cheap first commit fits both "setup
moved to `rift enable`" and "never existed", so both orders run in one sweep and
the difference is the displaced cost; the second arm is a *faithful simulation*
(`rift init` does both).

---

## 15.8 The consistency guard

It answers one question, **do the published numbers, bars and provenance agree?**,
never "is this machine fast now", which would fail on weather and teach everybody to
ignore it. Six arms, each calibrated by a planted defect: a scenario owing a bar
with no row; a declaration that does not hold together; a value missing a binding
bar; a binding bar nothing measures; *staleness* (no provenance, a subject not an
ancestor of `HEAD` or predating its last change to the `measured_paths`, a bar
derived from a record or load-void run); a generated block disagreeing with its
artifacts.

```bash
cd benches/native-baseline && cargo run --release -- audit
```

It prints artifacts read (twenty-five), findings and pending rows and exits
non-zero on any; the count is part of the verdict, since an empty directory reported
clean must not pass ([Chapter 16](16-development.md) has the CI job).
Both reducing documents end in a **generated block**, **one home per number**
(`facts`, `facts --kind report`); never edit one by hand, the fix for a wrong number
is a new run. `ratchet` compares git-latency artifacts as control-relative ratios
(10 % tolerance) and **exits zero on a regression** (it flags five arms of five between identical
runs); `--enforce` exits non-zero.

---

## 15.9 Records: kept, quotable, barred from setting a bar

An artifact with no commit provenance, or taken over uncommitted changes, is a **record**: kept (rewriting one to satisfy a check is the failure this
prevents), quotable as history with its conditions, **barred from deriving a
bar** and exempt from staleness. Every tracked artifact is a record (unknown is not fresh), which is why the four
bars are owner-set.

---

## 15.10 Caveats travel with their numbers

Caveats are *fields of the record*, copied verbatim into summaries, since prose can
be quoted around. **Import throughput measures our reader, not the format**: only a frozen golden
shard replicated under distinct ids exists, so the rate covers walk, parse and root
re-derivation and is never a compatibility claim. **The ancestry row is the whole serve, not the walk**: containment's
cost is a *difference* with and without the branch axis
([Chapter 9](09-worktrees-and-families.md)), and this row is a total. **The compounding row publishes a numerator, not a
ratio**: tokens one serve in a linked checkout avoided out of the family's shared
store. **Honest absence**: an unmeasured row carries null medians and zero runs, never a
zero.

---

## 15.11 Re-measuring, end to end

**Quiet the machine** (load above 8.0 voids the run as
a source of a bar), **build the subject in release and ask `doctor`**:

```bash
cargo build --release -p rift
cargo build --release -p rift-shadow-hooks --bin vogon   # condense row only
cd benches/native-baseline && cargo run --release -- doctor
```

**Measure** with `run --scenario <row>`, `hook-latency` or `enable-displacement`
(`--arms` narrows; the bare-commit control always runs, since an arm quoted
against another run's control compares machine weather). **Regenerate the blocks** (§15.8), then **audit**: zero findings,
zero pending, a non-empty count. A staleness finding (*"measures `<row>` at
`<subject>`, which predates `<commit>`"*) means the code deciding that measurement
moved: take a new run or keep the old as a record. Re-measuring never moves a bar.

---

## 15.12 What these numbers cannot tell you

They are **one machine class** (eight-core x86-64 macOS). **Process launch is an implementation term, not a hardware floor**: a preserved
user-hook chain and non-Unix installs pay a shell shim. **The byte columns in the git-latency record are declarations, not
measurements**: the probe returned nothing in all 135 values. Savings and cost figures come from another instrument with
another honesty ladder
([Chapter 8](08-cost-and-savings.md#813-the-bar-is-what-the-harness-measured));
permanent limits are in [Chapter 18](18-what-rift-does-not-do.md).

---

## 15.13 Related chapters

[03-capture.md](03-capture.md) ·
[04-redaction-and-secrets.md](04-redaction-and-secrets.md) ·
[08-cost-and-savings.md](08-cost-and-savings.md) ·
[09-worktrees-and-families.md](09-worktrees-and-families.md) ·
[16-development.md](16-development.md) ·
[17-invariants.md](17-invariants.md) ·
[`benchmarks/README.md`](../../benchmarks/README.md)
