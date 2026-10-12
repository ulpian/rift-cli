# 2. Configuration

Rift is configured by two files you own and commit, plus a few environment
variables; everything else (identities, queues, projections, caches) lives
outside your working tree and is rebuildable or minted once. This chapter is the
reference: each key, why its default is what it is, and where state lives.

---

## 2.1 The shape of configuration

**`.rift/config.toml`** says what may be captured, what is redacted, what is
sensitive and how long things are kept: tracked, reviewable, authoritative.
**`.rift/policies/serve.toml`** says what Recall may hand back and spend; it is
separate because it is read on a different path, by a different plane, under a
different failure rule. **Everything else is Rift's own state**, kept under the
git directory or your cache because it is a fact about *this machine*.

TOML, not YAML: the TOML crate is maintained and clean under dependency
auditing, the obvious YAML crate is not. Both files load with **unknown keys as
hard errors**, because a typo would otherwise silently disable a redactor, and
validation **aggregates** (four problems report four), because policy files are
edited rarely and under pressure and a whack-a-mole loader is one people work
around.

---

## 2.2 `.rift/config.toml` — the full reference

Every table is optional; only `schema_version` (`1`; any other value is refused
rather than guessed at) and `[project] name` are required, and Rift scaffolds
both. `repo_id` is an explicit identifier, almost always absent (Rift derives
one, §2.7).

### `[features]`

`total_recall` (default `true`) gates dashboard auto-ingest and the recall hooks
and skills. It is **on unless declared off** — a recall plane nobody turned on
saves nobody anything — so off is a written `total_recall = false`, never an
omission. It wires nothing by itself: skills reach an agent only through `rift init`,
Project Settings or `rift recall init` (`--no-recall` skips it).

### `[capture]`

| Key | Default | Meaning |
|---|---|---|
| `tool_calls` | `true` | Capture tool-call events. |
| `token_usage` | `true` | Capture provider-reported token usage. |
| `raw_transcripts` | `false` | Allow raw-transcript retrieval. |
| `acknowledge_risk` | `false` | Required `true` companion to `raw_transcripts`. |
| `checkpoint_strategy` | `"manual-commit"` | Base archiving strategy. |
| `schedule_interval_minutes` | `30` | Interval for `scheduled` (≥ 1). |
| `idle_sweep_minutes` | `5` | How long a live session may go without a hook before the idle sweep condenses and ends it (≥ 1). |

**Raw transcripts take two keys.** `raw_transcripts = true` without
`acknowledge_risk = true` is a validation error: it is the one setting that can
hand an agent unredacted developer text, so a reviewer sees an acknowledgement,
not a boolean. Switching it off clears the acknowledgement in the same edit, so a
stale `true` cannot re-arm it.

**Strategies:** `manual-commit`, `on-pr`, `scheduled`, `policy-driven`
([03-capture.md](03-capture.md)). Policy-driven archiving composes *on top* of
the base, so a touched sensitive path archives under any strategy and there is no
"off". The import marker is not configurable: a repository cannot declare its
own commits to be imports.

**The idle sweep** is read at the family's main checkout, so every member sweeps
at one floor; zero is refused, as it would end a session between two of its own
hooks. It moves only *when* a quiet session is condensed, so it is outside the
policy hash ([03-capture.md](03-capture.md) §3.6).

### `[redaction]`

No keys. `entropy_threshold` (fixed in the redactor: 4.0 bits per
character on tokens of at least 20 characters) and `extra_patterns` (use a rule
pack, [04](04-redaction-and-secrets.md) §4.8) are refused by name: a key that
moved the hash without changing the masking would say the setting was seen and
then ignore it.

### `[paths]`

`sensitive_paths` (array of globs, default `[]`) forces conservative serving and
exact-source-required handling. Nine globs are unioned into every policy and
cannot be removed by any file:

```
**/.env*        **/*secret*      **/*credentials*
**/id_rsa       **/id_dsa        **/id_ecdsa
**/id_ed25519   **/*.pem         **/*.key
```

Yours are added, sorted and de-duplicated; nothing drops one, because a
repository that could ship "stop treating private keys as sensitive" could turn
its own guard rails off.

### `[models]`

`allow` (empty means allow-all) and `deny` (always wins) are glob arrays; a model
in both is a validation error, not a precedence puzzle.

### `[retention]`

`default_class` (`"standard"`) and `classes` (`[]`; `["standard"]` when the table
is absent); a non-empty `classes` must contain `default_class`. The storage layer
enforces what a class means.

### `[checkpoint]`

`remote` is the git remote that carries this repository's capture references.
Set it with `rift configure --checkpoint-remote <name-or-url>`.

- **No default.** Every consumer reads an unset remote as *do not ship*, never
  "fall back to `origin`", which is whichever project you happened to clone and
  would publish this ledger into someone else's ([14](14-sync-readiness.md)).
- **The value is screened**, since it becomes a word in a `git` command line: a
  leading `-`, whitespace, control bytes or a NUL are errors.
- **It is outside the policy hash**: a destination is not policy, so clones with
  identical rules hash identically even when only one ships.

### Glob syntax

`**` matches any sequence including `/` (`**/x` matches a bare `x`), `*` any
sequence except `/`, `?` one character except `/`.
Patterns are capped at **512 bytes** and **64 wildcards**. The matcher has no
backtracking, so a hostile pattern cannot wedge the policy gates.

### The policy hash

Every effective configuration carries a hash of its content (schema version,
project name, capture flags, strategy, interval, the fixed entropy floor,
retention default, recall flag, and the sorted model, sensitive-path and
retention-class lists). Captured records reference it, so "which policy was in force" is
answerable. The checkpoint remote and *where the
policy came from* (§2.4) are excluded, since either would make identical
policies hash differently.

---

## 2.4 When a policy file is present but not applied

The loader ends in one of three states. **Declared**: the file was applied.
**Default**: no file, so the secure defaults apply and nothing was lost.
**Refused**: a file exists and was *not read* — the defaults apply and nothing
you declared is running. That happens when `.rift` is not a directory or
`.rift/config.toml` is a symlink or not a regular file.

**Why refuse rather than follow.** Both names are repository-shippable and git
stores symlinks, so a clone could point its policy file at your credentials: a
parse error quotes the offending line (once a credential), and a linked `.rift`
directory that parses cleanly loads policy from outside the working tree
silently. The loader inspects the link, not its target. **On the capture path a broken config never blocks git**: hooks fall back
to the secure defaults, since a file mid-edit must not fail your commit
(`rift doctor` reports it).

---

## 2.5 How the files get written

You can edit both by hand. Rift's own writes (`init`, `configure`, `enable` / `disable`, Project Settings)
go through one editor that is
comment-preserving, conflict-checked (a file changed since it was loaded is
refused with "refresh and retry"), symlink-refusing and same-directory atomic
(validated, written to a sibling temporary file, renamed), so a crash leaves no half policy. Project Settings changes only `features.total_recall`,
`capture.tool_calls`, `capture.token_usage` and `capture.raw_transcripts` (with
`acknowledge_risk`): a dashboard that could rewrite arbitrary policy could do it
by accident.

---

## 2.6 `.rift/policies/serve.toml` — the serving overlay

Every key is optional, and so is the file: absent means the defaults, and a key
you delete is a key you never wrote.

| Key | Default | Meaning |
|---|---|---|
| `denied_paths` | `[]` | Globs never served — not even by handle. |
| `raw_transcript_globs` | `[]` | Globs where raw-transcript retrieval is permitted, if enabled at all. |
| `injection_turn_budget` | `400` | Token ceiling for one turn's injected manifest; `0` disables per-turn injection. |

**The injection budget.** 400 is small on purpose: a manifest is a *pointer at a
delta*, and the agent pulls detail with the verbs it names, so a budget big
enough to inline content would spend the context recall exists to save. `0` means off. It covers the **per-turn manifest only**; the
once-per-session pointer is gated on whether recall is enabled
([07](07-serving-and-injection.md) has both tiers).

### The read gate: `[interception]`

The lever that declines a re-read of lines a context window already holds
([07-serving-and-injection.md](07-serving-and-injection.md)) reads one table:

| Key | Default | Meaning |
|---|---|---|
| `mode` | `enforce` | `off` never consults the lever; `observe` records the decision while the call runs as written; `enforce` declines once, with a reason. |
| `max_slice_bytes` | `98304` | Largest slice one decision may cover. |
| `min_slice_lines` | `28` | Shortest slice one decision may act on. |
| `max_inline_hash_bytes` | `1048576` | Largest file hashed on the gate path. |
| `recovery_budget` | `800` | Tokens a post-compaction recovery map may cost. |
| `basis_ttl_days` | `30` | How long `rift clean` keeps a basis snapshot. |
| `inherit_by_name` | `observe` | Decline on lines a subagent inherited because its prompt named the path. |
| `first_contact` | `observe` | Answer a window's first read of a large file once with its bound outline. |
| `first_contact_min_lines` / `_min_bytes` | `400` / `16384` | "Large": either floor is enough. |
| `decline_family_held` | `observe` | Decline once a read of bytes another live window holds, offering a cheaper route. |
| `[interception.agents]` | absent | One mode per family (`claude-code`, `codex`, `antigravity`). |

The three levers are never above `mode` or the family's cap.
`[interception.agents]` replaces `mode` for that family, and the pinned channel
caps the result: a family whose reason channel no recorded run has confirmed
resolves to `observe` however the table is written, and nothing lifts a family
above what its rows allow — a cap, not a grant. An unknown family name fails the
load. `enforce` is the default because all three channels are pinned and
confirmed; an explicit `off` or `observe` stays
([10](10-agent-integrations.md)).

The slice ceiling (96 KiB) and floor (28 lines, from a 111-token reason frame) are
measurements; [07 §7.4](07-serving-and-injection.md#74-tier-three-the-decision-at-the-read)
gives their reasons. The 1 MiB hash ceiling also bounds a basis snapshot: past it
nothing is diffed, so keeping the bytes would buy no answer.

At `observe`, `inherit_by_name` records lines a subagent was *pointed at*
([07 §7.3](07-serving-and-injection.md#what-a-subagents-window-inherits)),
`first_contact` the first read of a large file with a bound outline
([07 §7.4](07-serving-and-injection.md#a-first-read-of-a-large-file)) — the build
caps it at `observe`, so `enforce` behaves as `observe` — and
`decline_family_held` the decline it would have made
([07 §7.4](07-serving-and-injection.md#a-read-another-window-holds)). No read is
declined on any of them. `basis_ttl_days` governs the one durable artifact: a
revision edited after reading has no blob, so the delta frame needs its
**redacted** bytes kept; thirty days matches converted documents, since losing
one costs a delta frame, never an answer.

### Search: `[search]`

| Key | Default | Meaning |
|---|---|---|
| `mode` | `enforce` | The "native search" row of Project Settings toggles `enforce`/`off`. |
| `grep_head_limit` | `40` | Bound added to a content search that named none. |
| `glob_top_n` | `8` | Ranked paths an answered name search carries. |
| `answer_budget_tokens` | `160` | What one answered listing may cost. |

Two levers of different kinds share the table. **Bounding a content search is an
amendment**: the search still runs inside a documented bound and states its true
total, so it needs only a family whose schema documents amending that call.
**Answering a name search is a decline** — the model gets the ranked head and the
total instead — so it needs a confirmed reason channel and resolves to `observe`
elsewhere. The numbers bound how wrong each can be: **40** is a count the
client's own documentation carries (the follow-up for the rest runs unbounded),
**8** paths stand in for a listing the client cuts at a hundred, and **160
tokens** is a ceiling: an answer costing more than the listing it replaces is
not made, so the weakest case is *no lever*, never a loss. A search
*inside* file contents is never answered from an index of names, which does not
know the answer.

### Documents: `[doc]`

| Key | Default | Meaning |
|---|---|---|
| `mode` | `enforce` | The "documents as Markdown" row toggles `enforce`/`off`. |
| `formats` | `"all"` | Closed set: a typo fails to load rather than silently narrowing it. |
| `channel` | `rewrite` | `rewrite` amends the path to the sidecar; `redirect` declines once naming it. |
| `table_rows` | `200` | Rows kept per sheet. |
| `max_input_bytes` | `16777216` | Largest document converted. |
| `pdf_min_chars_per_page` | `200` | Thinner, and a PDF has no text layer: the read is left alone. |
| `convert_budget_ms` | `250` | Gate-path deadline; past it the read runs and the worker converts. |
| `ttl_days` | `30` | How long an unused sidecar is kept. |

`channel` exists because an amendment can be lost (the last hook to answer a call
wins): Rift verifies it and falls back to `redirect` for that window, and the key
makes that primary.

### Fail-closed

A present but unparseable file — or a symlink or special file — is
**fail-closed**: deny all exact retrieval, inject nothing, turn **every lever
off**, because acting on an agent's calls under a policy Rift cannot read has no
defensible reading. No file means the defaults.

---

## 2.7 Where state lives

Nothing below is configuration. *Family state* (hooks, minted identity, push
queue) lives in the git common directory, shared by every checkout;
*per-checkout progress* in the git directory; *working-tree state* in `.rift/`:
policy (yours) and run markers (Rift's)
([09-worktrees-and-families.md](09-worktrees-and-families.md)).

**"Initialized" is a fact about this machine, not about `.rift/`.** A repository
is set up where its family's capture projection exists under the cache root.
`.rift/config.toml` travels with every clone, so treating it as proof would make
a fresh clone look set up everywhere and never offer the setup that wires that
machine's agents; the CLI refuses a config-only clone and the dashboard shows
`◌ not set up here` ([11-canopy-tour.md](11-canopy-tour.md)). The dashboard also
requires the family's **binary of record** (`binary.json`) to name a binary that
is still there, since an agent hook creates the projection on first contact.
Hook gates ask only for the projection: a running hook is its own proof.

| Location | Holds | Safe to delete? |
|---|---|---|
| `.rift/config.toml`, `policies/serve.toml` | Repository policy; serving overlay. | Policy: no. Overlay: means "defaults". |
| `.rift/policies/redactors/` | Shared rule packs. | Yes — you lose the extra rules. |
| `.rift/.gitignore` | Ignores `state/` and `agents/`, nothing else. Written only where none exists; an existing file or symlink is never rewritten or followed. | Yes — `init` rewrites it. |
| `.rift/agents/` | One capture descriptor per wired agent, plus recall install and probe records. | Yes — `rift enable`. |
| `.rift/state/` | `native-integrations.json` (ownership fingerprints), `project-setup.json`, `adoption.json`, `injection/` and `manifest/` (per-window markers), `search/index.bin` (path index). | Yes — each costs a regeneration; without the integrations file fragments read as unowned. |
| `.rift/state/reads/` | One append-only file per window: ranges handed, declines, write hashes, evictions. Never content. | Yes — costs declines, never a wrong decision. |
| `.rift/state/doc/`, `doc/w/<window>/`, `search/w/<window>/` | This member's view of the family's sidecars; what a window was told about documents or did about searches. | Yes — at most one repeated redirect, bound or answer. |
| `<git_dir>/rift-sessions/` | Session ownership markers and usage sidecars. | Yes; live sessions lose attribution context. |
| `<git_dir>/rift-capture-queue/`, `rift-agent-queue/` | Commits awaiting condensing; agent-hook events for the detached worker. | No — pending capture work. |
| `<common_dir>/hooks/` | The five git hooks, as links, plus markers. | No — capture stops. |
| `<common_dir>/rift-family/` | `family.json` (minted identity: **never hand-edit**), `family.history.jsonl` (the audit trail), `binary.json` (the hooks' binary), `savings-window.json` (the `rift savings reset` boundary). | Only `binary.json` and `savings-window.json`. |
| `<common_dir>/rift-push-queue/` | References waiting to be shipped. | No. |
| `<common_dir>/rift-family/doc/<key>.md` | Converted-document sidecars, one per (bytes, converter version, output caps). | Yes — collected at `[doc] ttl_days`. |
| `<common_dir>/rift-family/basis/<content-sha256>.bin` + `.json` | Basis snapshots: the **redacted** bytes of a recorded revision, plus masked line runs, policy fingerprint and rule packs. | Yes — collected at `basis_ttl_days`; costs a delta frame, never an answer. |
| `~/.cache/rift/<family-id>/projection.db`, `<recall-id>/recall.db` | The rebuildable capture and recall projections. | Yes — `rift rebuild`. |
| `…/recall.db.set-aside-<UTC stamp>` | A recall projection `rift-memd rebuild` set aside: the only copy of serves no checkpoint carries (`rift backfill --recall-from <file>` reads it back). | No — only you remove it. |
| `~/.cache/rift/canopy/ui-state.db` | Machine-global dashboard state. | Yes — layout and profiles. |
| `~/.cache/install-salt`, `machine-id.json` | Per-install salt (`0600`, when the keychain is unused) and derived machine id, at the cache **root**. | Salt: changes derived identity. Id: re-derived. |
| `~/.config/rift/projects.toml`, `canopy.toml` | Repository list; preferences (§2.9). | Yes — re-register / defaults. |

**The push queue is family state; the capture queues are not.** A queue entry
names a *reference*, and references live in the shared object store; kept per
checkout, a linked worktree's push shipped nothing and removing it deleted its
queue. The capture and agent queues stay per checkout because they are one
checkout's private progress: a session in A must never be credited to a commit
in B.

**The family identity is minted once and then only read.** The first process
that can *prove* which checkout is main writes the manifest (a hard link, so a
concurrent minter loses cleanly); afterwards identity is read, never re-derived
from a path, so moving a repository is survivable. Repair is one explicit command
that appends its reason to the history; an unparseable manifest is a `rift
doctor` finding with hooks doing nothing durable, since Rift records nothing
rather than guess which repository this is.

---

## 2.8 Environment variables

The cache root is the first of `RIFT_CACHE_DIR` (the test suite sets it so a test
never touches your cache), `XDG_CACHE_HOME`, then `HOME` (`LOCALAPPDATA` on
Windows) yielding `~/.cache`. The config root resolves the same way through
`RIFT_CONFIG_DIR`, `XDG_CONFIG_HOME`, `HOME` (`APPDATA` on Windows).

| Variable | Effect |
|---|---|
| `RIFT_LOG` | Logging to **stderr**, off by default: hooks run inside your git output, and stdout is the data contract. |
| `RIFT_NO_KEYCHAIN` | Use the `0600` salt file; the keychain can prompt where a headless run cannot answer. |
| `RIFT_CAPTURE_SYNC` | Condense **inline** in the hook, not the detached worker; never changes whether capture or redaction happen. |
| `RIFT_HOOK_TIMING` | A per-phase timing line per hook ([03-capture.md](03-capture.md)). |
| `RIFT_DEVELOPER` | Developer identity where git has none. |
| `RIFT_FAMILY_DIR` | Relocates the family manifest root, so a hermetic run mints without touching a repository it does not own. |
| `RIFT_HEADLESS`, `RIFT_HEADLESS_WAIT_MS` | Render one frame, print, exit, after draining the data bus (1200 ms dashboard, 3000 ms viewer). |
| `RIFT_SPLASH` | `never` skips the splash for one run. |
| `RIFT_CANOPY_ASCII`, `NO_COLOR` | Force the ASCII render tier. |
| `RIFT_CANOPY_NO_MOUSE` | Disable mouse capture over the stored preference. |
| `COLORTERM`, `TERM`, `WT_SESSION`, `TERM_PROGRAM` | Colour and glyph hints. |

Also read, not yours to set: `GIT_DIR`, `GIT_INDEX_FILE` and `RIFT_HOOK` (how
`rift session attach` / `import` refuse to run inside a hook), `PATH`, `HOME`,
`CODEX_HOME` (read **only**: Rift never writes a client's global configuration)
and `RIFT_HOOK_TEST_PANIC` (fault injection proving a panicking hook cannot fail
your git operation).

The shipped binaries read **no API keys** and link no HTTP client; a repository
check reads both binaries' resolved dependency graph to keep it so.

---

## 2.9 Machine-global files

**`~/.config/rift/projects.toml`** lists the repositories the dashboard watches
(an index, not policy): `[[projects]]` rows of `name`, `path`, `family_id`.
Unknown keys are an error, not a project silently vanishing. Rows are deduplicated by *canonical* path, then by family (a linked
worktree whose family holds the seat is refused and told where it lives); a row
with no family id never matches, since "unknown" is not "the same" and a match
would let one swallow every unrelated repository. A member registers as its *family root*.

**`~/.config/rift/canopy.toml`** holds per-user dashboard preferences:
`seen_splash` (`false`), `splash_mode` (`"first-run"`; `always`, `first-run` or
`never`, set with `rift configure splash-mode <mode>`), `reduced_motion`
(`false`) and `mouse` (`true`). Reads are **default-safe** (a malformed file
yields defaults): the one relaxed place, since the worst outcome is an animation
playing once more.

---

## Related chapters

[03-capture.md](03-capture.md) · [07-serving-and-injection.md](07-serving-and-injection.md)
· [09-worktrees-and-families.md](09-worktrees-and-families.md) ·
[10-agent-integrations.md](10-agent-integrations.md) ·
[13-storage-and-schema.md](13-storage-and-schema.md) ·
[14-sync-readiness.md](14-sync-readiness.md).
