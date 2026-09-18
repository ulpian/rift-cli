# 2. Configuration

Rift is configured by two files you own and commit, plus a small set of
environment variables. Everything else it keeps — identities, queues,
projections, caches — is written by Rift, lives outside your working tree, and
is either rebuildable or explicitly minted once. This chapter is the reference:
every key, its default, why the default is what it is, and where each piece of
state lives.

---

## 2.1 The shape of configuration

**The repository's policy is a committed file.** `.rift/config.toml` says what
may be captured, what is redacted, what counts as a sensitive path and how long
things are kept — tracked, reviewable in a pull request, authoritative, read by
Canopy and the CLI rather than copied. **Serving has a second, narrower file.**
`.rift/policies/serve.toml` says what Recall may hand back and how much of a
turn's context it may spend, separate because it is read on a different path,
by a different plane, under a different failure rule. **Everything else is
Rift's own state, not configuration**: identity, queues and projections live
under the git directory or your cache, never in the working tree, because they
are facts about *this machine* and would be wrong in someone else's clone.

The format is TOML, not YAML, for supply-chain reasons: the TOML crate is
maintained and clean under dependency auditing, the obvious YAML crate is not.
Both files load with **unknown keys as hard errors**, because otherwise a typo
silently disables a redactor — `capture.raw_transcipts = false` under a
permissive loader means raw transcripts are on while the file says otherwise.
Validation **aggregates**: four problems report four, not the first, because
policy files are edited rarely and under pressure and a whack-a-mole loader is
one people work around.

---

## 2.2 `.rift/config.toml` — the full reference

### A complete example

```toml
schema_version = 1
[project]
name = "my-service"
[features]
total_recall = true
[capture]
tool_calls = true
token_usage = true
raw_transcripts = false
acknowledge_risk = false
checkpoint_strategy = "manual-commit"
schedule_interval_minutes = 30
[redaction]
entropy_threshold = 4.5
extra_patterns = ["acme-[a-z0-9]{20}"]
[paths]
sensitive_paths = ["infra/terraform/**", "**/*.tfstate"]
[models]
allow = []
deny = ["*-preview-*"]
[retention]
default_class = "standard"
classes = ["standard", "extended"]
[checkpoint]
remote = "origin"
```

Every table is optional. Only `schema_version` and `[project] name` are
required, and Rift scaffolds both when it writes the file.

### `schema_version`

| | |
|---|---|
| Type | integer |
| Required | yes |
| Accepted | `1` |

The document's contract version; a value this build does not know is refused
rather than guessed at. One accepted value today — the field exists so a second
can arrive without older binaries misreading the file.

### `[project]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | required | Human-readable project name. Shown in the dashboard and in reports. |
| `repo_id` | string | derived | An explicit repository identifier. Almost always absent — Rift derives one (see §2.7). |

### `[features]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `total_recall` | bool | `true` | Whether the memory-delta plane is installed for this repository. |
| `search` | bool | `false` | Whether native file search is installed for this repository. Recorded, not read. |

`total_recall` gates dashboard auto-ingest and the provisioning of recall hooks
and skills into your agents. It is **on unless declared off**: a repository
that never mentions the key has Recall, because a recall plane nobody turned
on saves nobody anything, and off is a written `total_recall = false`, never
an omission. The key does not wire anything by itself — skills reach an agent
client only through `rift init`, Project Settings or `rift recall init`, which
read it rather than act on the file's absence; `rift init --no-recall` skips
that step and writes nothing.

`search` is accepted and ignored: file search reads how to behave from its own
policy table, never from a bool here, so the key records a fact about your
install and has no getter, no runtime effect, and no place in the policy hash.
`fff` is its retired spelling — still accepted for one more release, so a
repository configured long ago still loads, and `rift doctor` asks you to rename
it. A third key, `codegraph`, is no longer accepted at all: the packages behind
it were deleted, so a config that still carries the line fails to load, and
deleting the line is the whole fix.

### `[capture]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `tool_calls` | bool | `true` | Capture tool-call events. |
| `token_usage` | bool | `true` | Capture provider-reported token usage. |
| `raw_transcripts` | bool | `false` | Allow raw-transcript retrieval. |
| `acknowledge_risk` | bool | `false` | Required `true` companion to `raw_transcripts`. |
| `checkpoint_strategy` | string | `"manual-commit"` | Base archiving strategy. |
| `schedule_interval_minutes` | integer ≥ 1 | `30` | Interval for the `scheduled` strategy. |

**The two-key raw-transcript rule.** `raw_transcripts = true` without
`acknowledge_risk = true` is a validation error, not a warning. Raw transcripts
are the one capture setting that can hand an agent unredacted developer text,
so enabling it takes two keys and a sentence of intent in the diff — a reviewer
sees an acknowledgement rather than a boolean. The keys move together when Rift
writes them: switching raw retrieval off clears the acknowledgement in the same
edit, so a stale `true` cannot sit waiting to re-arm.

**Checkpoint strategies.** Exactly four values are accepted: `manual-commit`
(on your commits — the default), `on-pr` (at pull-request boundaries),
`scheduled` (every `schedule_interval_minutes`) and `policy-driven` (driven by
policy signals rather than a clock); [03-capture.md](03-capture.md) has what
each archives on. Policy-driven archiving always composes *on top* of whichever
base you choose — a touched sensitive path archives under any strategy — which
is why there is no "off": the base chooses the rhythm of ordinary archiving,
never whether the security-relevant case is recorded. A fifth ledger token, the
marker stamped on read-only checkpoints landed by an import, is deliberately
**not** configurable: a repository cannot declare its own commits to be
imports.

### `[redaction]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `entropy_threshold` | float | `4.5` | Parsed, defaulted and folded into the policy hash. Not read by the redaction pipeline today. |
| `extra_patterns` | array of strings | `[]` | Parsed, deduplicated and folded into the policy hash. Not read by the redaction pipeline today. |

Be honest about these two keys: the schema accepts them and the policy hash
changes when you set them, but nothing downstream consults them. The pipeline's
entropy floor is compiled in (4.0 bits per character on tokens of at least 20
characters), and the only way to add a detector today is a rule pack under
`.rift/policies/redactors/**/*.toml` (or `.json`) — see
[04-redaction-and-secrets.md](04-redaction-and-secrets.md). The keys are kept
so a future pipeline can honour them without a schema change; until then they
change what your policy *says*, not what it *does*, which chapter 18 lists as a
known gap.

### `[paths]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `sensitive_paths` | array of globs | `[]` | Paths whose matches force conservative serving and exact-source-required handling. |

**The built-in floor.** Nine globs are unioned into every effective policy and
cannot be removed by any file:

```
**/.env*        **/*secret*      **/*credentials*
**/id_rsa       **/id_dsa        **/id_ecdsa
**/id_ed25519   **/*.pem         **/*.key
```

Your `sensitive_paths` are added, sorted and de-duplicated; there is no syntax
for dropping one, because a repository that could ship "please stop treating
private keys as sensitive" can turn its own guard rails off.

### `[models]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `allow` | array of globs | `[]` | Allowed models. Empty means allow-all. |
| `deny` | array of globs | `[]` | Denied models. Deny always wins. |

Listing a model in both is a validation error rather than a silent precedence
puzzle.

### `[retention]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `default_class` | string | `"standard"` | The class applied when nothing else says otherwise. |
| `classes` | array of strings | `[]` (`["standard"]` only when the whole `[retention]` table is absent) | Every class this repository defines. |

If `classes` is non-empty it must contain `default_class`, or the default names
a class that does not exist. The table is vocabulary; the storage layer
enforces what the classes mean.

### `[checkpoint]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `remote` | string | *(none)* | The git remote that carries this repository's capture references. |

Three things about this key each prevent an accident. **There is no default,
and absence is a refusal**: an unconfigured repository resolves to "nowhere",
and every consumer — the resume fetch ladder, the push shipper, `rift sync` —
reads that as *do not ship*, never "fall back to `origin`", because `origin` is
whichever project you happened to clone and falling back to it would publish
this repository's capture ledger into someone else's. **The value is screened
before use**, since it becomes a word in a `git` command line: a leading `-`,
whitespace, control bytes or a NUL are rejected, and a present-but-unusable
value is an error rather than a silent "not configured". **It is not part of
the policy hash**: a remote is a destination, so two clones with identical
redaction rules hash identically even when only one ships, and re-pointing must
not make later captures look as though a different policy applied.

Set it with `rift configure --checkpoint-remote <name-or-url>`, through the
safe editor of §2.5.

### Glob syntax and its limits

The matcher for `sensitive_paths` and the model lists is a deliberately small
subset:

| Token | Matches |
|---|---|
| `**` | Any sequence, `/` included. A `/` immediately after is consumed with it, so `**/x` also matches a bare `x`. |
| `*` | Any sequence *except* `/`. |
| `?` | Exactly one character except `/`. |
| anything else | A literal. |

Two caps are enforced at load: **512 bytes** and **64 wildcards** per pattern.
The matcher has no backtracking, so a hostile pattern cannot wedge the policy
gates; the caps are defence in depth.

### The policy hash

Every effective configuration carries a hash of its *content*: schema version,
project name, capture flags, strategy and interval, entropy threshold,
retention default, recall flag, and the sorted model, sensitive-path,
extra-pattern and retention-class lists. Captured records reference it, so you
can always answer "which policy was in force when this was recorded". Two
exclusions are deliberate — the checkpoint remote (a destination) and *where
the policy came from* (§2.4) — because both would make identical policies hash
differently, making the hash useless for its one question.

---

## 2.4 When a policy file is present but not applied

The loader reaches the built-in defaults three ways, and telling them apart
matters. **Declared**: the file was read and applied, so your policy is in
force. **Default**: no file exists, so the built-in secure defaults apply and
nothing was lost. **Refused**: a file exists and was *not read* — the built-in
defaults apply, and everything you declared is not running.

A refusal happens when the policy is not something the working tree plainly
owns: `.rift` is not a directory (a symlink elsewhere on the machine, or a
plain file in its place), `.rift/config.toml` is a symlink, or it is not a
regular file at all.

**Why refuse rather than follow the link.** Both `.rift` and its config file
are repository-shippable names, and git stores symlinks faithfully, so a clone
could point its policy file at your credentials file. A parse error quotes the
offending source line — measured before the guard existed, and the line printed
was a credential — and worse, a linked `.rift` *directory* whose target parses
cleanly echoes nothing while silently loading policy from outside the working
tree. So the loader inspects the link rather than its target: policy that
cannot be trusted does not apply.

**On the capture path, a broken config never blocks git.** The hooks load the
effective config and fall back to the built-in secure defaults if anything goes
wrong, because a policy file mid-edit must not fail your commit. The cost is
that a malformed file is quiet on the hook path and loud everywhere else, which
is why `rift doctor` reports it.

---

## 2.5 How the files get written

You can edit both files by hand. When Rift writes them — `rift init`,
`rift configure`, `rift enable` / `rift disable`, Project Settings in Canopy —
one shared editor does it, and it is **comment-preserving** (only the value
changes), **conflict-checked** (the caller passes the revision it loaded, and a
file changed on disk since then is refused with "refresh and retry" rather than
silently overwritten), **symlink-refusing** (the loader's ownership test,
before writing), and **same-directory atomic** (validated in full, written to a
sibling temporary file, renamed over the original, so a crash never leaves half
a policy).

Canopy's Project Settings may change exactly four values:
`features.total_recall`, `capture.tool_calls`, `capture.token_usage`, and
`capture.raw_transcripts` (which carries `acknowledge_risk` with it). Anything
else is a hand edit or a CLI verb — a dashboard that could rewrite arbitrary
policy is one that can rewrite policy by accident.

---

## 2.6 `.rift/policies/serve.toml` — the serving overlay

This file governs what the recall plane may hand back, what it may spend, and
how far each of its levers may act. Every key is optional, and so is the file:
absent means the defaults below, and a key you delete is a key you never wrote.

```toml
# Never served, not even as a handle.
denied_paths = ["ops/runbooks/**"]

# Where raw transcript retrieval is permitted, if it is enabled at all.
raw_transcript_globs = []

# Tokens one turn's injected manifest may cost. 0 turns injection off.
injection_turn_budget = 400
```

| Key | Type | Default | Meaning |
|---|---|---|---|
| `denied_paths` | array of globs | `[]` | Paths never served — not even by handle. |
| `raw_transcript_globs` | array of globs | `[]` | Paths where raw-transcript retrieval is explicitly permitted. |
| `injection_turn_budget` | integer | `400` | Token ceiling for one turn's injected manifest. `0` disables per-turn injection. |

### The injection budget

The default of 400 is small on purpose. An injected manifest is a *pointer at a
delta*, not the delta: the agent reads it and pulls detail with the verbs the
manifest names, so a budget large enough to inline content would spend your
context on material the agent may not need — the exact cost recall exists to
avoid. Absent is not zero: the key is optional and absence means "the default",
while writing `0` means "off". The budget governs the **per-turn manifest
only**; the once-per-session pointer is gated on whether recall is enabled for
the repository, not on this number
([07-serving-and-injection.md](07-serving-and-injection.md) has both tiers and
the per-turn call and token ceilings).

### The read gate: `[interception]`

The lever that declines a re-read of lines a context window already holds
([07-serving-and-injection.md](07-serving-and-injection.md)) reads its bounds
from one table:

| Key | Type | Default | Meaning |
|---|---|---|---|
| `mode` | `off` \| `observe` \| `enforce` | `enforce` | `off` never consults the lever; `observe` reaches the decision and records it while the call runs exactly as written; `enforce` declines once, with a reason. |
| `max_slice_bytes` | integer | `98304` | The largest slice one decision may cover. |
| `min_slice_lines` | integer | `28` | The shortest slice one decision may act on. |
| `max_inline_hash_bytes` | integer | `1048576` | The largest file hashed on the gate path; past it the read proceeds. |
| `recovery_budget` | integer | `800` | Tokens a post-compaction recovery map may cost. |
| `basis_ttl_days` | integer | `30` | How long a read-time basis snapshot is kept before `rift clean` collects it. |
| `[interception.agents]` | per-family modes | absent | An override per supported family: `claude-code`, `codex`, `antigravity`. |

Three of those numbers are measurements rather than preferences. **96 KiB** is
where a read result truncates in place on the client that was measured — 25,000
tokens, with a paging hint, never written to a file — so a decision never covers
more than the context was going to keep. **28 lines** is the point a decline
starts paying: a plain reason frame costs 111 tokens against roughly 4 tokens a
line, so a shorter slice is decided and recorded but never acted on. That figure
is measured by a test against the sealed renderer rather than quoted, and it
moved from 24 when the frame grew its provenance handle
([07-serving-and-injection.md §7.4](07-serving-and-injection.md#74-tier-three-the-decision-at-the-read)). **1 MiB** is the ceiling on hashing while a person waits, and the
same figure bounds a basis snapshot: past it nothing is diffed anyway, so
keeping the bytes would be disk spent on an answer no caller can be given.

`basis_ttl_days` governs the one durable artifact this lever leaves behind.
The delta frame needs the revision a window holds, and a revision an agent
edited after reading was never committed and has no blob, so the bytes are
kept — redacted, once per family, under the read's own content address
([07 §7.4](07-serving-and-injection.md#74-tier-three-the-decision-at-the-read)).
Thirty days is the same figure a converted document gets and for the same
reason: losing one costs a delta frame and never an answer.

Two rules govern the per-family table. An override is a **cap, not a grant**: a
family whose reason channel no recorded run has confirmed resolves to `observe`
however the table is written, and nothing can lift a family above what its
pinned rows allow. And an unknown family name is a policy that fails to load
rather than a line that silently does nothing.

`enforce` is the default for the three supported families because each one's
channel is pinned and confirmed; an explicit `off` or `observe` you wrote stays
authoritative, and the measured retry rate never changes a mode by itself
([10-agent-integrations.md](10-agent-integrations.md)).

### Search: `[search]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `mode` | `off` \| `observe` \| `enforce` | `enforce` | As above. The Project Settings "native search" row toggles this key between `enforce` and `off` and leaves an explicit `observe` alone until it is toggled. |
| `grep_head_limit` | integer | `40` | The result bound added to a content search that named none. |
| `glob_top_n` | integer | `8` | How many ranked paths an answered name search carries. |
| `answer_budget_tokens` | integer | `160` | What one answered listing may cost. |

One table drives two levers of different kinds, which is why what caps them
differs. **Bounding a content search is an amendment**: the search still runs,
inside a bound its own tool documents, and the tool states its true total even
where that bound truncates the listing — so the lever needs only a family whose
published schema documents amending that call, and it is held to `mode` like
everything else, recording in `observe` the bound it did not add. **Answering a
name search is a decline**: the call does not run and the model is handed the
ranked head and the total instead — so it needs a family whose reason channel a
recorded run has confirmed, and resolves to `observe` on any other however this
table is written.

The three numbers are bounds on how wrong each lever can be. **40** is a result
count the client's own documentation carries as an argument, and the follow-up
that asks again for the rest runs unbounded, so the cap costs the agent nothing
it cannot take back. **8** ranked paths stand in for a listing the client itself
cuts at a hundred, which is where the saving comes from and also its ceiling.
**160 tokens** is a ceiling and not a target: an answer whose frame would cost
more than the listing it replaces is not made at all, so the weakest case for
this lever is *no lever*, never a loss.

A search never turns into a read. An answered listing carries paths and a
count, never file contents, and a search *inside* file contents is never
answered from an index of names — the index does not know the answer, and a
model sent to a listing for it would be sent somewhere the answer is not
([07-serving-and-injection.md](07-serving-and-injection.md)).

### Documents: `[doc]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `mode` | `off` \| `observe` \| `enforce` | `enforce` | As above. The Project Settings "documents as Markdown" row toggles this key between `enforce` and `off` and leaves an explicit `observe` alone until it is toggled. |
| `formats` | `"all"` | `"all"` | Which formats may be converted. A closed set, so a typo fails to load rather than silently narrowing it. |
| `channel` | `rewrite` \| `redirect` | `rewrite` | Whether a document read has its path amended to the sidecar, or is declined once naming it. |
| `table_rows` | integer | `200` | Rows kept per sheet, every sheet. |
| `max_input_bytes` | integer | `16777216` | The largest document converted at all. |
| `pdf_min_chars_per_page` | integer | `200` | Thinner than this, a PDF has no usable text layer and the read is left alone. |
| `convert_budget_ms` | integer | `250` | The gate-path conversion deadline; past it the read runs and the worker converts. |
| `ttl_days` | integer | `30` | How long an unused sidecar is kept. |

`channel` exists because an amendment can be lost: several hooks may answer one
tool call, and the last to finish wins. Rift verifies its own amendment against
the executed call and falls back to `redirect` for that window when one is
dropped — this key lets a repository choose the fallback as its primary instead.

### Fail-closed, and what that means

A valid, owned file is your policy. No file means the defaults: no extra
denials, transcripts still off, injection at 400, and the levers above at their
own defaults. A file that is present but unparseable — or a symlink or special
file — is **fail-closed**: deny all exact retrieval, inject nothing, and turn
**every lever off**.

That is the part people find surprising. Injection spends the agent's context
without being asked and a lever acts on the agent's own calls, while an
unreadable serving policy is precisely the state in which Rift does not know
what you permitted, so acting under an unknown policy has no defensible
reading.

---

## 2.7 Where state lives

Nothing below is configuration; it is what Rift writes for itself. Knowing
which directory owns which fact is what makes "can I delete this?" and "why is
my worktree empty?" answerable.

### The three ownership rules

**Family state lives in the git common directory** — the hooks, the minted
identity, the queue of references waiting to be pushed: facts shared by every
checkout. **Per-checkout progress lives in the git directory**, because a
session lives in one checkout at a time and its caches and queues are that
checkout's private working state. **Working-tree state lives in `.rift/`**:
policy (yours) and local run markers (Rift's), inside the checkout that
produced them. A *linked worktree* is a second path into the same repository,
so it shares the common directory and has its own git directory — the single
fact the table below encodes, whose consequences are in
[09-worktrees-and-families.md](09-worktrees-and-families.md).

**"Initialized" is a fact about this machine, not about `.rift/`.** A
repository counts as set up where its family's capture projection exists under
the machine's cache root — the file `rift init` creates — and nowhere else.
`.rift/config.toml` is committed configuration: it travels with every clone,
and a directory that merely contains it proves nothing about the machine
reading it. Reading `.rift/` as "initialized" was how a fresh clone of a
repository that shares its configuration looked set up on every machine, ran
its commands against a projection that did not exist, and never offered the
setup that would have wired that machine's agents. The CLI, the hook-health
writer and the dashboard now all ask the same question of the same file; the
CLI refuses a config-only clone with `configured by the repository, not set up
on this machine (run rift init)` under the usual not-initialized exit code, and
the dashboard shows it as `◌ not set up here`
([11-canopy-tour.md](11-canopy-tour.md)).

The dashboard asks for one thing more, because the projection alone is weak
evidence: an agent hook opens the store on first contact and creates it, so a
repository whose git hooks were never installed can carry one. A family is set
up here when its projection sits under the cache root **and** its **binary of
record** — `<common_dir>/rift-family/binary.json` in the table below, the note
the hook install leaves naming the binary the hooks point at — names a binary
that is still there. Without it the card and the setup page both read the
repository as setup left half done, and the wizard installs what is missing; it
re-runs safely over what already exists. The hook gates ask only for the
projection: a hook that is running is its own proof that hooks are installed.

### The table

| Location | Holds | Safe to delete? |
|---|---|---|
| `.rift/config.toml` | Repository policy. | No — authoritative. |
| `.rift/policies/serve.toml` | Serving overlay. | Deleting means "defaults". |
| `.rift/policies/redactors/` | Shared redaction rule packs. | Yes — you lose the extra rules. |
| `.rift/agents/` | One capture descriptor per wired agent (`<agent>.json`), plus recall install and probe records. | Yes — `rift enable` regenerates. |
| `.rift/state/native-integrations.json` | Which integrations this checkout has, and their ownership fingerprints. | Regenerated; deleting makes fragments read as unowned. |
| `.rift/state/project-setup.json` | This checkout's setup progress. | Yes. |
| `.rift/state/adoption.json` | Why a member's adoption stopped, so nothing pays to fail the same way twice. | Yes — means "try again". |
| `.rift/state/injection/` | One marker per context window already given its pointer, keyed like `reads/` below. | Yes — costs one duplicate pointer. |
| `.rift/state/manifest/` | Per-context-window manifest baselines and silence stamps, keyed like `reads/` below. | Yes — costs one extra bootstrap turn. |
| `.rift/state/reads/` | One append-only file per context window: the ranges that window was handed, the declines Rift issued, the hashes its writes were asked to produce, and its eviction boundaries. Paths, hashes and counts, never content. | Yes — costs re-reads that would have been declined; never a wrong decision. |
| `<git_dir>/rift-sessions/` | Session ownership markers and usage sidecars for *this* checkout. | Yes; live sessions lose attribution context. |
| `<git_dir>/rift-capture-queue/` | Commits captured but not yet condensed. | No — pending capture work. |
| `<git_dir>/rift-agent-queue/` | Agent-hook events journaled for the detached worker. | No — pending capture work. |
| `<common_dir>/hooks/` | The five git hooks, as links, plus sidecar markers. | No — capture stops. |
| `<common_dir>/rift-family/family.json` | The family's minted identity. | **Never hand-edit.** |
| `<common_dir>/rift-family/family.history.jsonl` | Append-only record of every mint and remint. | No — the audit trail. |
| `<common_dir>/rift-family/binary.json` | Which `rift` binary the family's hooks point at, with a capped history. | Yes; the next install rewrites it. |
| `<common_dir>/rift-family/savings-window.json` | Where the savings share starts counting: the boundary `rift savings reset` wrote, with who set it and when. Absent means the whole record. | Yes — deleting it widens the window back to the whole record; nothing measured is lost. |
| `<common_dir>/rift-push-queue/` | References waiting to be shipped. | No — queued shipping work. |
| `<common_dir>/rift-family/doc/<key>.md` | The family's converted-document sidecars, one per (document bytes, converter version, output-affecting caps); shared by every member. | Yes — `rift clean` collects them at `[doc] ttl_days` or the flag, whichever keeps longer; the next pass reconverts. |
| `<common_dir>/rift-family/basis/<content-sha256>.bin` + `.json` | The family's read-time basis snapshots: the **redacted** bytes of one revision a read recorded, and beside them the runs of lines the mask touched, the redaction policy's fingerprint and the rule packs that ran. Keyed by the read's own content address, shared by every member. | Yes — `rift clean` collects them at `[interception] basis_ttl_days` or the flag, whichever keeps longer; losing one costs a delta frame and never an answer, since the blob route and the file in the checkout are both untouched. |
| `.rift/state/doc/<key>.md` | This member's hardlink view of the family's sidecars (a copy where the filesystem refuses a link); vanishes with a temporary worktree. | Yes — disposable; a view whose family answer is gone is removed by this member's own `rift clean` as an orphan. |
| `.rift/state/doc/w/<window>/` | What one context window has already been told about documents: whether its substitution was verified dropped, which sidecars it was redirected to once, which read awaits its result. Keyed by the same window digest as the window journal. | Yes — costs at most one repeated redirect; this member's `rift clean` collects it at `[doc] ttl_days`. |
| `.rift/state/search/index.bin` | This member's path index: every path the checkout held, with the modification time each had, keyed by HEAD's oid and the newest directory modification time. | Yes — costs one rebuild, which the next session start or commit asks for. |
| `.rift/state/search/w/<window>/` | What one context window has already done about searches: which content searches it bounded, which of those it asked again in the same turn, and which name searches it has already had answered. Keyed by the same window digest as the window journal. | Yes — costs at most one repeated bound or answer; this member's own `rift clean` collects it by age. |
| `~/.cache/rift/<family-id>/projection.db` | The rebuildable capture projection. | Yes — `rift rebuild` restores it. |
| `~/.cache/rift/<recall-id>/recall.db` | The recall projection. | Yes — rebuildable. |
| `~/.cache/rift/canopy/ui-state.db` | Machine-global dashboard state across repositories. | Yes — you lose layout and profile state. |
| `~/.cache/install-salt` | Per-install salt, `0600`, when the keychain is not used. At the cache **root**, beside `rift/`, being one fact per install rather than per repository. | Deleting changes derived machine identity. |
| `~/.cache/machine-id.json` | The derived machine identifier and a one-way tag of the salt it came from. Also at the cache root. | Yes — costs one re-derivation. |
| `~/.config/rift/projects.toml` | The machine-global list of repositories the dashboard watches. | Yes — you re-register. |
| `~/.config/rift/canopy.toml` | Per-user dashboard preferences. | Yes — back to defaults. |

### Two decisions inside that table worth the space

**The push queue is family state; the capture queues are not.** A queue entry
names a *reference*, and references live in the object store the whole family
shares. Per checkout, every consequence was a loss: a push from a linked
worktree read its own directory and shipped nothing, the main checkout's queue
accrued unreachably, and removing a worktree deleted its queue outright. So the
queue lives in the common directory, an older version's queue is drained into
the shared one on first access, and adoption is idempotent per reference and
sequence, so a failed cleanup means a re-read rather than a double-ship. The
capture and agent queues did **not** move, for the mirror-image reason: they
describe one checkout's private progress, and a session worked in checkout A
must never be credited to a commit in checkout B.

**The family identity is minted once and then only read.** The manifest is
written by the first process that can *prove* which checkout is the main one,
using a temporary file plus a hard link so a concurrent minter loses cleanly.
Afterwards identity is read from the manifest, never re-derived from a path,
which is what makes moving a repository survivable. Repair is a single explicit
command that appends its reason to the history; nothing repairs itself
silently, and an unparseable manifest reads as *corrupt* — a `rift doctor`
finding, with hooks degrading to doing nothing durable. That is the posture
everywhere: Rift's answer to "I do not know which repository this is" is to
record nothing, not to guess.

---

## 2.8 Environment variables

These are the variables the shipped binaries actually read; anything not listed
is not consulted.

### Where state roots resolve

The cache root is the first of these that is set: `RIFT_CACHE_DIR` (explicit
override, and what the test suite sets so a test run can never touch your real
cache), then `XDG_CACHE_HOME`, then `HOME` (`LOCALAPPDATA` on Windows) yielding
`~/.cache`; with none set it falls back to the current directory. Projections
live at `<cache root>/rift/<id>/`. The config root resolves the same way with
`RIFT_CONFIG_DIR`, `XDG_CONFIG_HOME`, then `HOME` (`APPDATA` on Windows)
yielding `~/.config`, and the project registry and dashboard preferences live
at `<config root>/rift/`.

### Behaviour

| Variable | Effect |
|---|---|
| `RIFT_LOG` | Structured diagnostic logging to **stderr**, standard filter syntax (`RIFT_LOG=debug`, `RIFT_LOG=rift_shadow=trace`). Off by default, because hooks run inside your git output; stdout stays the command's data contract. |
| `RIFT_NO_KEYCHAIN` | Skips the OS keychain and uses the `0600` salt file. The keychain can raise an interactive authorization prompt for a freshly built binary, and a headless run cannot answer one. |
| `RIFT_CAPTURE_SYNC` | Any value (`=1` by convention) runs the condense **inline** in the hook instead of the detached worker. Slower; for a caller that must observe the checkpoint before it returns. It chooses *who* does the work and *when* — never whether capture or redaction happen. |
| `RIFT_HOOK_TIMING` | One per-phase timing line per hook invocation: the instrument behind the hook latency budget; see [03-capture.md](03-capture.md). |
| `RIFT_DEVELOPER` | Explicit identity override for the developer signal, for machines with no usable git identity. |
| `RIFT_FAMILY_DIR` | Relocates the family manifest root (keyed by a hash of the canonical common directory) so a hermetic run can mint a family identity without touching a repository it does not own. |

### Dashboard

| Variable | Effect |
|---|---|
| `RIFT_HEADLESS` | Any value forces the non-interactive path even on a terminal: render one frame, print it, exit. |
| `RIFT_HEADLESS_WAIT_MS` | How long the headless frame drains the live data bus before rendering. Defaults to 1200 ms for the dashboard, 3000 ms for the viewer. |
| `RIFT_SPLASH` | `never` skips the startup splash. The persistent preference lives in `canopy.toml`; this is the one-run override. |
| `RIFT_CANOPY_ASCII` | Any value forces the ASCII render tier. |
| `RIFT_CANOPY_NO_MOUSE` | Any value disables mouse capture, overriding the stored preference — useful when you want your terminal's native drag-to-select. |
| `NO_COLOR` | Honoured as a forced-ASCII signal, alongside `RIFT_CANOPY_ASCII`. |
| `COLORTERM` | Read as a true-colour hint on POSIX hosts, and when the richer terminal probe is not compiled in. Not consulted on Windows. |
| `TERM`, `WT_SESSION`, `TERM_PROGRAM` | On Windows the console itself is asked to enable VT processing, and that decides colour; any of these three present selects the Unicode glyphs (Windows Terminal, VS Code, mintty). Not read this way off Windows. |

### Read, but not yours to set

Rift also reads `GIT_DIR` and `GIT_INDEX_FILE` (git exports them to every hook,
so their presence is how `rift session attach` and `rift session import` refuse
to run from inside one), the reserved `RIFT_HOOK` marker for the same refusal,
`PATH` and `HOME`, `CODEX_HOME` — read **only**, because Rift never writes a
client's global configuration — and `RIFT_HOOK_TEST_PANIC`, fault injection
proving a panicking hook cannot fail your git operation.

> **Tip:** The shipped binaries read **no API keys**, and no build of them
> links an HTTP client: no workspace crate depends on one, and a repository
> check reads the resolved dependency graph of both binaries to keep it that
> way. If you are auditing egress, that is the sentence to check.

---

## 2.9 Machine-global files

### `~/.config/rift/projects.toml`

The list of repositories the dashboard watches — an index, not policy, since
policy is per repository.

```toml
[[projects]]
name = "my-service"
path = "/Users/you/code/my-service"
family_id = "b6caa84d…"
```

Three rules govern it. **Unknown keys are an error**, as everywhere else,
because the alternative is a project silently vanishing from the dashboard.
**One row per repository, deduplicated twice**: first by *canonical* path, so
`/repo`, `/repo/.`, `/repo/../repo` and a symlinked spelling are one directory
rather than four projects; then by family, so a linked worktree whose family
already holds the seat is refused and told where the family lives. And **rows
written before families existed carry no family id**, which never matches:
"unknown" is not "the same", and treating it as a match would let one legacy
row swallow every unrelated repository. Saves are atomic — a sibling temporary
file, renamed — so a crash never leaves a half-written registry, and
registering a member records the *family root*, not the member.

### `~/.config/rift/canopy.toml`

Per-user dashboard preferences, global rather than per repository: whether you
have seen the opening animation is a fact about you, not a project.

| Key | Default | Meaning |
|---|---|---|
| `seen_splash` | `false` | Whether the first-run splash has played. |
| `splash_mode` | `"first-run"` | `always`, `first-run` or `never`. Set with `rift configure splash-mode <mode>`. |
| `reduced_motion` | `false` | Honour reduced-motion preferences: static rendering, no animation. |
| `mouse` | `true` | Mouse capture in the live dashboard. `RIFT_CANOPY_NO_MOUSE` overrides at launch. |

Reads here are **default-safe**: a missing, unreadable or malformed file yields
the defaults rather than an error. It is the one place the strictness rule is
deliberately relaxed, for proportionality — the worst outcome of misreading
this file is an animation playing once more than you wanted.

**Leftover from older installs.** `.rift/rewrite.toml` configured a routing
layer that no longer exists; nothing parses it, but the capture path leaves a
queue note (overwritten by key, so one line, not one per commit) saying the
file does nothing. Deleting the file is the whole fix.

---

## Related chapters

[03-capture.md](03-capture.md) · [07-serving-and-injection.md](07-serving-and-injection.md)
· [09-worktrees-and-families.md](09-worktrees-and-families.md) ·
[10-agent-integrations.md](10-agent-integrations.md), which is where the
per-client wiring these keys govern is written down ·
[13-storage-and-schema.md](13-storage-and-schema.md) ·
[14-sync-readiness.md](14-sync-readiness.md). The field-by-field specification
of every serialized artifact named in §2.7 is a working note kept outside this
guide and outside the repository.
