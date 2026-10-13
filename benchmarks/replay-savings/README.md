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
So do the four that follow the first-contact and family-held rows, each also a key of the document:

- *first sight, split by the two floors* cuts the first-sight bucket into six classes that add up to it: under both floors, large and not code (Markdown beneath it), large code read whole, large code from line 1, large code from mid-file, and calls of several reads or one that could not be read.
- *by name, cut by what the spawner handed down* cuts the by-name sibling re-reads into three that add up to the by-name row: the spawner held no line of the path, the read went beyond the lines handed down, and the handed-down lines covered it.
- *after a compaction* counts compactions (a boundary and its summary record are one), the reads in the three turns after one, and the reads of a path held before it by channel and by distance in turns, in each window's file order.
- *pull answers* counts the lookup verbs actually run from a shell, by verb, with the runs in a scratch repository and the requests for help apart, and the reads of a path an answer named.
- *no name or line count, split by what the transcript proves* cuts that bucket into four classes that add up to it, each by file size (≤ 16 KiB, ≤ 64 KiB, ≤ 256 KiB, ≤ 1 MiB, > 1 MiB): known and unchanged at the re-read (declinable), known and changed since (a delta), a re-read with no complete copy of the bytes in the transcript, and a first sight of the path. The bytes are what the transcript proves about the working tree — a whole-file write, an edit applied to known bytes, a whole-file read, or a numbered range joined to known bytes — per session tree and working directory.
  Under it, *of the unchanged re-reads, what the gate declines* puts each unchanged re-read through the shipped decision, over a journal per context window fed by the same proven bytes (hashed as the gate hashes them), and credits the declined row only with a re-read that decision declines: one call of a single read, a file within the 1 MiB inline-hash cap, lines this window holds, a key not already declined, a slice within the credit bound. Every other unchanged re-read sits on a row of its own saying why it is not credited: one of several reads in a call, over the hashing cap, a first read of those bytes in this window (the bytes were proven from another window of the session tree), held before a compaction emptied the window, other lines held and not these, already declined once, past the credit bound, or held and let through (under the line floor). The credited calls are added to the declined and declinable rows and pay their reason frames; the bucket rows do not move.
