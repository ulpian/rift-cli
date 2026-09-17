# Enable-time displacement

- harness `0.0.1`, artifact schema 1, generated 1788196892
- host: macos / x86_64
- protocol: 3 warmup + 11 measured, **a fresh repository per iteration** (a first commit exists once in a repository's life)
- budget: **100 ms** of git latency on the first-commit arm (a hard budget, set by the owner)

**Record.** No commit is recorded for this run. It is kept as a *record* — a real measurement, quotable as history, barred from setting a bar — because the history that held its subject was collapsed to a single fresh root on 2026-09-02.

The claim under test is **displacement, not speed**: the owner moved projection init to enable time, and one order cannot tell a cost that moved from a cost that never existed. Both orders are measured here, in this run.

| arm | p50 | git latency | verdict |
|---|---|---|---|
| `A_bare` | 34.23 ms | — | control · load1 3.90 |
| `E0_enable` | 162.40 ms | n/a | not a git latency — an absolute wall time · load1 3.90 |
| `E1_commit_after_enable` | 64.43 ms | 30.20 ms | within budget · load1 3.99 |
| `E2_commit_without_prior_enable` | 65.54 ms | 31.31 ms | within budget · load1 3.91 |

**Displaced: 1.12 ms of first-commit cost is not paid when enable paid it.** That is `E2 - E1`, both measured in this run.

- **`E0_enable`** — Never quoted against the bare-commit control: initializing a repository and committing to one are different acts, and a ratio between them would be a number wearing a unit it does not have.

- **`E1_commit_after_enable`** — The arm the hard 100 ms git-latency budget binds: it is what a user's next commit costs.

- **`E2_commit_without_prior_enable`** — FAITHFUL SIMULATION, recorded as such: there is no supported way to install rift's hooks without initializing, because `rift init` does both — which is the claim under test. This arm runs the real `rift init` through the real binary and then removes the projection state the hook path would find, leaving the installed hooks in place. It is not a user flow; it is the counterfactual the displacement claim is measured against.

**Machine class.** Every absolute here is one machine's measurement and moves about 2x with load, which is why each arm is quoted against a bare-commit control measured in the SAME run and the ratio is the portable number. Fresh Unix installs invoke unchained hooks through direct executable links; preserved user-hook chains and non-Unix installs retain shell shims. Process launch is part of the measured implementation, and nothing here may be generalised into a product claim about another machine.
