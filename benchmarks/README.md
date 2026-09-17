# benchmarks/ — the tracked evidence

Every performance claim Rift makes is a number a harness produced, and every
such number lives here, in the repository, as an artifact a reader can re-run the
reduction over. Nothing in this directory is typed by hand except the prose; the
numbers a gate reads are generated from the artifacts and recomputed by the
artifact-consistency guard (`vs-reference audit`).

## Families and where they live

| family | what one artifact is | home | reduced by |
|---|---|---|---|
| `native-baseline` | one `vs-reference run` — a scenario set over generated fixtures, both legs, with fixture manifests, host, load attestation and subject provenance | `benchmarks/native-baseline/<family>-<stamp>.json` (+ `.md`) | the fence below |
| `hook-latency` | one git-latency sweep: what a commit costs beyond a bare one, arm by arm, against its own run's control | `benchmarks/hook-latency/` | `hook-latency.md`'s ```` ```bench-facts ```` fence (the arms' one home; only their counts appear below) |
| `enable-displacement` | one displacement sweep: enable-then-commit and commit-without-prior-enable in the same run, so a cost that moved is told from a cost that vanished | `benchmarks/enable-displacement/` | the fence below |
| `floors.toml` | the declaration published numbers are judged against — one absolute bar per scenario, each stating how much authority it carries | `benchmarks/floors.toml` | read by the guard |

`redaction-fp-baseline.md` is the redaction false-positive census over this
repository's own text and is not a performance artifact.

## Protocol and machine class

Every artifact records the protocol it ran under (warmup and measured iterations,
per-iteration isolation), the host, the rift binary's `git describe` and dirty
flag, and a load attestation. Every published number came from one machine class
— `macos / x86_64 / 8 logical CPUs` — measured on a quiet host with nothing else
running, and an absolute here moves roughly 2× with load. An artifact with
no provenance, or taken over a dirty tree, is a **record**: kept as history,
barred from setting a bar. A run on a machine over the load ceiling is
**load-void** for the same purpose. Both counts are in the fence.

Every artifact tracked today is a record. Eighteen of the git-latency sweeps
never carried commit provenance; the rest carried a commit that this
repository's history no longer holds, because the history was collapsed to a
single fresh root on 2026-09-02. They are kept exactly as measured, and the
bars they once implied are now stated as decisions somebody made — see
`floors.toml` for the reasoning and for what re-deriving a row would take.

## Two budgets, kept apart

Two 100 ms figures exist and they are not one budget. The **git-latency
budget**, set by the project owner and hard, binds the synchronous cost a
commit-path hook adds to a bare commit, measured by the `hook-latency` and
`enable-displacement` families against a bare-commit control. The **agent-hook
budget** —
p50 < 100 ms, p95 < 500 ms, including ingest — binds the hook rows of the scenario
family, which time a whole `rift hooks <agent> <verb>` process. A number from one
must not be quoted against the other. The agent-hook path reports its own phases
under `RIFT_HOOK_TIMING=1` (`hook=agent-hook`), the way the git-hook path does.

## Caveats that travel with their rows

The `import-throughput` row's rate is true about **our own reconstruction** of the
pre-Rift on-disk format and silent about the format itself — no legacy artifact
was ever fetched. That sentence is a field of the row in the
fence, copied from the scenario catalogue, so nothing that reads the artifact can
read the number without it.

A row that no clean run has measured says so in the fence — `null` medians, zero
runs — rather than carrying a number. Honest absence is the point: a row that
quietly renders as zero reads like a fast one. Every row the fence carries today
is measured over three clean runs, so no such row is published; a row added
later shows up as unmeasured until three clean runs exist for it.

## How to regenerate

```bash
cd benches/native-baseline
cargo run --release -- run --scenario <row>          # writes benchmarks/native-baseline/
cargo run --release -- hook-latency                  # writes benchmarks/hook-latency/
cargo run --release -- enable-displacement           # writes benchmarks/enable-displacement/
cargo run --release -- facts                         # hook-latency.md's bench-facts fence
cargo run --release -- facts --kind report           # this file's bench-report fence
cargo run --release -- audit                         # every arm; exit 1 on any disagreement
```

Measure only on a quiet machine: nothing else running, release profile, and no
other cargo build in flight. Never edit a fence by hand — the guard reds on a
fence that disagrees with the artifacts it names, and a hand-typed number is
exactly what it exists to catch.

## Gate-bearing facts (every family)

Generated by `vs-reference facts --kind report` from every artifact under `benchmarks/`, and **the only gate-bearing numbers in this document**. The artifact-consistency guard recomputes the fence from the artifacts it names and reds on disagreement. Rates are milli-units per second and times are nanoseconds; every median is nearest-rank across runs of each run's own median. A row that no clean run has measured says so rather than carrying a number. The git-latency arms keep their one home in `hook-latency.md`'s fence; only their counts appear here.

```bench-report
{
  "schema_version": 1,
  "families": [
    {
      "family": "enable-displacement",
      "artifacts": [
        1788196865,
        1788196878,
        1788196892
      ],
      "records": 3,
      "load_void": 0
    },
    {
      "family": "hook-latency",
      "artifacts": [
        1787120100,
        1787120240,
        1787120379,
        1787120478,
        1787120545,
        1787127649,
        1787127756,
        1787127877,
        1787127988,
        1787128560,
        1787128664,
        1787128771,
        1787128951,
        1787129088,
        1787129241,
        1787129490,
        1787129660,
        1787129832,
        1788101269
      ],
      "records": 19,
      "load_void": 18
    },
    {
      "family": "native-baseline",
      "artifacts": [
        1788201188,
        1788201423,
        1788201657
      ],
      "records": 3,
      "load_void": 0
    }
  ],
  "throughput": [
    {
      "scenario": "import-throughput",
      "work_unit": "shards",
      "runs": 3,
      "rate_milli_per_s_median": 156211,
      "floor_milli_per_s": 71000,
      "status": "owner-ratified",
      "runs_under_floor": 0,
      "caveat": "This rate is true about our own reconstruction of the pre-Rift on-disk format and silent about the format itself. No legacy artifact was ever fetched — the README beside the golden shards under `legacy-fixtures/` says so in its own words, \"Nothing here was fetched\" — and every spelling in these shards is cited there to a declaration that already lived in this repository, and the corpus is that frozen shard replicated under distinct ids. It measures the reader — the walk, the parse, the root re-derivation. It is never a format-compatibility claim."
    },
    {
      "scenario": "verify-throughput",
      "work_unit": "checkpoints",
      "runs": 3,
      "rate_milli_per_s_median": 56222,
      "floor_milli_per_s": 27000,
      "status": "owner-ratified",
      "runs_under_floor": 0,
      "caveat": null
    },
    {
      "scenario": "condense",
      "work_unit": "events",
      "runs": 3,
      "rate_milli_per_s_median": 24918,
      "floor_milli_per_s": 11000,
      "status": "owner-ratified",
      "runs_under_floor": 0,
      "caveat": null
    }
  ],
  "latency": [
    {
      "scenario": "hook-round-trip",
      "runs": 3,
      "p50_ns_median": 18403832,
      "p95_ns_median": 20317393,
      "budget_p50_ms": 100,
      "budget_p95_ms": 500,
      "runs_over_p50": 0,
      "runs_over_p95": 0
    },
    {
      "scenario": "hook-session-start",
      "runs": 3,
      "p50_ns_median": 52686465,
      "p95_ns_median": 55391955,
      "budget_p50_ms": 100,
      "budget_p95_ms": 500,
      "runs_over_p50": 0,
      "runs_over_p95": 0
    },
    {
      "scenario": "hook-prompt-submit",
      "runs": 3,
      "p50_ns_median": 41529926,
      "p95_ns_median": 45504617,
      "budget_p50_ms": 100,
      "budget_p95_ms": 500,
      "runs_over_p50": 0,
      "runs_over_p95": 0
    },
    {
      "scenario": "hook-read-tool",
      "runs": 3,
      "p50_ns_median": 18835959,
      "p95_ns_median": 21335308,
      "budget_p50_ms": 100,
      "budget_p95_ms": 500,
      "runs_over_p50": 0,
      "runs_over_p95": 0
    },
    {
      "scenario": "hook-tool-result",
      "runs": 3,
      "p50_ns_median": 16086301,
      "p95_ns_median": 18354817,
      "budget_p50_ms": 100,
      "budget_p95_ms": 500,
      "runs_over_p50": 0,
      "runs_over_p95": 0
    },
    {
      "scenario": "hook-stop",
      "runs": 3,
      "p50_ns_median": 49781925,
      "p95_ns_median": 55687036,
      "budget_p50_ms": 100,
      "budget_p95_ms": 500,
      "runs_over_p50": 0,
      "runs_over_p95": 0
    },
    {
      "scenario": "hook-session-end",
      "runs": 3,
      "p50_ns_median": 20939329,
      "p95_ns_median": 23919760,
      "budget_p50_ms": 100,
      "budget_p95_ms": 500,
      "runs_over_p50": 0,
      "runs_over_p95": 0
    },
    {
      "scenario": "hook-replay",
      "runs": 3,
      "p50_ns_median": 15183127,
      "p95_ns_median": 16813573,
      "budget_p50_ms": null,
      "budget_p95_ms": null,
      "runs_over_p50": 0,
      "runs_over_p95": 0
    },
    {
      "scenario": "identity-attribution",
      "runs": 3,
      "p50_ns_median": 51521748,
      "p95_ns_median": 55927620,
      "budget_p50_ms": 100,
      "budget_p95_ms": 500,
      "runs_over_p50": 0,
      "runs_over_p95": 0
    },
    {
      "scenario": "cold-status",
      "runs": 3,
      "p50_ns_median": 12242962,
      "p95_ns_median": 13297953,
      "budget_p50_ms": null,
      "budget_p95_ms": null,
      "runs_over_p50": 0,
      "runs_over_p95": 0
    },
    {
      "scenario": "checkpoint-list",
      "runs": 3,
      "p50_ns_median": 11468376,
      "p95_ns_median": 12914740,
      "budget_p50_ms": null,
      "budget_p95_ms": null,
      "runs_over_p50": 0,
      "runs_over_p95": 0
    },
    {
      "scenario": "resume-restore",
      "runs": 3,
      "p50_ns_median": 125404132,
      "p95_ns_median": 138281583,
      "budget_p50_ms": null,
      "budget_p95_ms": null,
      "runs_over_p50": 0,
      "runs_over_p95": 0
    },
    {
      "scenario": "blame-why",
      "runs": 3,
      "p50_ns_median": 60279493,
      "p95_ns_median": 65555078,
      "budget_p50_ms": null,
      "budget_p95_ms": null,
      "runs_over_p50": 0,
      "runs_over_p95": 0
    },
    {
      "scenario": "worktree-add-adopt",
      "runs": 3,
      "p50_ns_median": 69864673,
      "p95_ns_median": 77556001,
      "budget_p50_ms": 100,
      "budget_p95_ms": 100,
      "runs_over_p50": 0,
      "runs_over_p95": 0
    },
    {
      "scenario": "member-switch",
      "runs": 3,
      "p50_ns_median": 35051217,
      "p95_ns_median": 41679636,
      "budget_p50_ms": 100,
      "budget_p95_ms": 100,
      "runs_over_p50": 0,
      "runs_over_p95": 0
    },
    {
      "scenario": "recall-known-state",
      "runs": 3,
      "p50_ns_median": 132158161,
      "p95_ns_median": 142937314,
      "budget_p50_ms": null,
      "budget_p95_ms": null,
      "runs_over_p50": 0,
      "runs_over_p95": 0
    }
  ],
  "displacement": {
    "runs": 3,
    "first_commit_after_enable_ns_median": 30197522,
    "budget_ms": 100,
    "runs_over_budget": 0
  },
  "git_latency": {
    "artifacts": 19,
    "records": 19,
    "load_void": 18
  }
}
```
