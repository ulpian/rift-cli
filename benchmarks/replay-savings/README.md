# benchmarks/replay-savings/ — the transcript replay

One artifact is one run of `cargo run -p xtask -- replay-savings` over recorded Claude Code sessions, written by its `--artifact` flag as a pair: `replay-savings-<unix-seconds>.md`, the table a person reads, and `replay-savings-<unix-seconds>.json`, the same figures as a versioned document.
The seconds are the moment the run started.
Each file carries its provenance: the `git describe --always --dirty` and short `HEAD` of the source tree the command was built from, where the path facts came from (the worktree, or each window's commit and the refs that were searched), the corpus it walked, by its counts alone (transcript files, session trees, context windows, tool calls) and never by its path, the tokenizer, and the run's start and end.
Both files are aggregates only: counts, token totals and shares, never a path, a prompt or a line of a transcript.

## How one is produced

The history is every era of the repository's history the corpus spans, each fetched as a ref into one bare clone: the history as it stood before each rewrite of it, and the branch being measured.
A window no ref reaches back to has no commit and falls back to the worktree, so an era left out shows up as windows with no commit.

```sh
git clone --bare <history bundle> <scratch>/history.git
git -C <scratch>/history.git fetch <older history bundle> main:refs/heads/prepurge
git -C <scratch>/history.git fetch <this checkout> main:refs/heads/current
cargo run -q -p xtask -- replay-savings \
  --transcripts <frozen transcript copy> \
  --repo <the worktree the build was taken from> \
  --history <scratch>/history.git \
  --history-ref main --history-ref current --history-ref prepurge \
  --artifact benchmarks/replay-savings
```

Without `--history` the path facts come from `--repo` as it stands, and a file deleted or split since a session read it has no line count at all.
With it, each window is replayed against the newest commit, across the named refs' first-parent chains, whose committer time is at or before the window's first event; the worktree answers only for a path that commit does not have and the worktree does, and for a window older than every commit, and the table counts both.
What stays under "no name or line count for the path" once every window has a commit is, all but a handful, reads of files no commit ever held — local working notes kept out of version control and since removed — which no history can answer.

## Reading one against another

The corpus is the owner's own recorded sessions, which grow with every session and are pruned by the client's cleanup, so absolute counts move between snapshots.
A run is therefore taken over a frozen copy of the transcript directory rather than the live one, and a before and an after are comparable only when both were taken over one frozen copy and one worktree; across copies, compare shares, and only with care.

The declined share — and every row derived from it — is an estimated upper bound on what the build would have kept out of context, never a claim that the reads would have happened, as the table's own "Reading it" paragraph says.
Searches are in the denominator and the journal never declines one, so the read-only share is the larger of the two rates.
The sections that size later levers — the client's own declines, the sibling re-reads by inheritance route, the machine-format census and the first-sight sizes — measure pools, not savings.
