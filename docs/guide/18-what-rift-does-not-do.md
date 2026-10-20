# 18 — What Rift does not do

Each property is a decision or a mechanical fact, kept because the alternative
costs more than it buys.

## 18.1 Capture

- **Capture needs a git repository.** A condense no longer needs a commit, but every checkpoint is still a tree of git objects on a ref of Rift's own, found through the repository's git directory.
  A directory outside git captures nothing, and Rift keeps no object store apart from git. ([03](03-capture.md))
- **A turn-end seal trails the live transcript.** A turn's last line waits for the next turn or the session end, and the lag is reported, never hidden.
- **Unknown parent content attributes as `unknown`.** Attribution replays a
  session's edits onto the file's parent content as the last boundary saw it. A
  parent no boundary ever observed has nothing to replay onto, so the line is
  credited to neither human nor agent. ([03](03-capture.md))
- **Sessions recorded before turn boundaries existed have none.** The ledger is
  append-only, so they cannot be back-filled; the doctor lists them as
  zero-coverage rows.
- **A read whose bytes change before the worker counts them is left out.**
  Token counting runs off the hook path. If the bytes moved and are not in git,
  the cost cannot be reconstructed, so it is omitted: a small under-count
  instead of a wrong number. A turn-boundary snapshot has the same shape: a file
  changed after the boundary's instant is unknown to the window it closes.
  ([08](08-cost-and-savings.md))
- **Two instruments disagree on redaction cost.** Re-measure; never quote an
  absolute figure across sessions.
- **OpenCode and Ollama give Rift no transcript to seal:** OpenCode names no
  transcript file on its hooks and keeps its sessions in its own store, readable
  only through `opencode export`, and Ollama is a model runner that records no
  conversation for the programs that call it, so Rift captures neither and does
  not rebuild one from hook events.
  ([10](10-agent-integrations.md#106-the-six-that-are-refused-and-exactly-why))

## 18.2 Recall and serving

- **A changed-hunks answer masks spans.** A mask is a difference Rift wrote, not
  one the repository made, and serving lines would return the live bytes of the
  span the gate masked, so one hunk touching a masked run reduces the answer to
  `(changed)` with no lines. Storing raw bytes is refused. ([07](07-serving-and-injection.md) §7.4)
- **The per-turn ceiling binds per process.** The budget lives in the caller and
  each `rift recall` invocation starts a fresh one. Accounting and audit stay exact. ([07](07-serving-and-injection.md) §7.10)
- **The three clients are not equivalent.** They differ in size limits on
  injected text, in how long an injected step survives, and in whether a file
  read can be amended, so substitution happens on one; the others get sidecars and a redirect.
- **A name-search answer is bounded by arithmetic.** It names eight ranked paths
  of a pool the client cuts at a hundred, so its largest saving is
  fixed by them. On a short listing the frame costs a large share of what it
  replaces, and the cost guard declines. ([07](07-serving-and-injection.md) §7.7)
- **Two clients publish no ceiling for the call a search replaces.** They are
  priced at the figures of the client that does: an assumption, not a fact.
- **An answered listing ranks on one window.** A hook folds in only its own
  window journal and runs no git status; family-wide warmth belongs to the
  command-line verb.
- **Two clients take the repeat escape every time.** A declined read is allowed
  on the identical repeat, so on those clients a decline costs a round trip and
  returns the bytes; the saving is booked net of the frame. ([07](07-serving-and-injection.md) §7.4)

## 18.3 Cost and savings

- **The savings figure is an estimated upper bound.** It compares what a full
  read of the candidate set would have cost with what was returned, a
  counterfactual with no estimator. It bounds the opportunity; it does not claim
  those tokens were saved. ([08](08-cost-and-savings.md) §8.13)
- **The savings share is real, and its window may be empty.** It is computed one
  way everywhere and published with its counts, never re-denominated.
- **Search is partly outside the denominator.** A composition no argv can run is
  counted, not measured; a re-execution is a reconstruction; a tree that moved
  between call and worker discards the count. ([08](08-cost-and-savings.md) §8.6)
- **A PDF's saving is excluded.** Its native cost is a page count against a
  vision rate nothing pins, so its baseline is zero, not an invented rate. ([08](08-cost-and-savings.md) §8.6)

## 18.4 Storage and sync

- **One registry path can hold a family twice.** A family deduplicates by its
  main work tree; when the registering checkout cannot prove it, its own path is
  registered, since registering nothing would hide the repository.
- **With no remote, the sync queue accrues.** Entries name durable references,
  so cleaning keeps them. There is no fallback to the default remote: pushing to
  one you did not nominate is no default. ([14](14-sync-readiness.md))
- **Bundle import is references-only.** `rift bundle import` attaches
  references; `rift rebuild` is the second step that projects them.

## 18.5 Checks

- **Throughput bars are set, not re-derived;** paired arms are single-run
  medians, not verdicts; the attribution proof rests on a fixture.
- **A screen that cuts at a test-module marker** can be shrunk by a string or
  block comment holding the exact joined sequence at column zero. A parser would
  add a second parse, and two parses drift apart.
- **Some checks are advisory or opt-in** (the interpreter leg, fuzz): a green
  local run means the required ones passed.
- **Non-`PATH` install locations are a fixed table,** so a moved vendor reads
  `absent`. A JetBrains plugin reads `JetBrains plugin`, never `present`: it
  ships no binary; its evidence is a directory holding a jar.
