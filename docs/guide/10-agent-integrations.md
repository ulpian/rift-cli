# 10. Agent integrations

Which files Rift writes into your repository, which it only ever *reads*, how it
proves a file is its own before touching it again, and why six of the nine
adapters it ships are refused. [01-quickstart.md](01-quickstart.md) wires a
repository end to end.

---

## 10.1 The shape: registered but refusing

Rift ships nine adapters — `claude-code`, `codex`, `gemini`, `antigravity`, `cursor`, `copilot-cli`,
`opencode`, `factory-droid`, `ollama` — all **registered** (visible in `rift recall status`, Project
Settings and every integration command), of which exactly three can be **enabled**: `claude-code`,
`codex`, `antigravity`. The rest are refused by name, *before a single byte is written*, because both
easier options are worse.
**Hiding the six** turns "not supported" into "does not exist", so a user with
Cursor installed gets silence — which is also what a broken integration looks
like. **Installing them** produces a file that looks like a working integration
and captures nothing, and a ledger with quiet holes is worse than none, because
you trust it.

So the refusal is loud and the row stays visible. Detection is not eligibility: an installed Ollama
does not fail `init`, but naming an unsupported agent explicitly makes `init` refuse the whole run
**up front**, naming every offender.

---

## 10.2 The three wired clients

| Client | Descriptor | Client hook file Rift writes | Recall instructions |
|---|---|---|---|
| Claude Code | `.rift/agents/claude-code.json` | `.claude/settings.local.json` | `.claude/skills/rift-recall/SKILL.md` |
| Codex | `.rift/agents/codex.json` | `.codex/hooks.json` | a managed block in `AGENTS.md` |
| Antigravity | `.rift/agents/antigravity.json` | `.agents/hooks.json` (reserved `rift-recall` bundle) | `.agents/rules/rift-recall.md` |

Each then needs one act from you, printed as a follow-up and never performed by Rift (§10.3): approve
the hooks and restart, or trust the project in Codex. Until then the wiring is present and inert,
which `rift recall status` reports.

The **descriptor** is Rift's generated record of what it wired, safe to delete (`rift enable`
recreates it): the agent, an absolute `rift_bin`, its `hooks` (each `{verb, command}`) and `compat`.
**The binary path is absolute** because agents launch with environments you did not choose, and an
absolute path is the one spelling true in all of them
([09-worktrees-and-families.md](09-worktrees-and-families.md)). **The verb list is not a second
table**: the descriptor and the client file both project the adapter's one declared list, because a
second copy drifts silently and advertises captures no hook was registered for. Antigravity's bundle
lives under the reserved `rift-recall` key with `"enabled": true`, the contract its hook API
publishes ([03-capture.md](03-capture.md) has the per-verb breakdown).

---

## 10.3 What Rift never touches

**Global client configuration** (`~/.codex/config.toml`, home-directory settings) is read in
places (§10.5) and never written. **Trust and approvals**: Rift never marks its own hooks
approved. **Authentication, credentials, binaries, versions** are never touched, and a needed
restart is a printed follow-up. **Anything Rift did not write** is §10.4. These are your
decisions and the client's own state, and a capture tool that silently grants itself trust is
one you cannot audit ([17-invariants.md](17-invariants.md)).

---

## 10.4 Custody: proving a file is Rift's own

Every write answers one question first: *is the thing already there mine?* The ownership manifest
`.rift/state/native-integrations.json` records a content fingerprint for every artefact Rift
installed. A file whose bytes still hash to it is Rift's own and may be rewritten; one that does not
is a **conflict**: reported (`Codex hooks are not safely Rift-managed; repair .codex/hooks.json and
retry`), never replaced. Unrelated entries are preserved: Rift rewrites only the groups it recognises
as its own, Codex's `.codex/hooks.json` groups included.

`.rift/state/` is not committed, so a file Rift wrote can have no entry (a fresh clone with committed
descriptors, a wiped state directory); refusing it would strand users on files Rift produced, so
other proofs suffice. **Descriptors** are recognised by *shape* (exactly the key set Rift writes, an
absolute binary, every hook precisely `{verb, command}`), ignoring the verb list, since the
alternative breaks the day a verb table is corrected, when you most need it to work. **Hook
fragments** are *re-emitted*: the recogniser takes the absolute binary from the first Rift command
and requires every group to equal what Rift would write for that event and binary, so a fragment from
a smaller verb table recognises but one altered byte does not. **Committing descriptors is not
recommended**: each names the `rift` of the machine that wrote it, so `rift init` ignores `agents/`
beside `state/`.

**Symlinks.** The hook file **and the project directory containing it** are checked before anything
is written through them, and a symlink at either is refused — that is how a write meant for your
repository lands elsewhere.

**Tracked files are never written automatically.** Antigravity's `.agents/hooks.json` is routinely
committed. Explicit `rift enable --agent antigravity` fingerprint-merges only the reserved
`rift-recall` bundle and preserves unrelated named hooks; malformed files, symlinks and edited Rift
bundles are refused without a partial write. Worktree adoption only verifies and reports. A committed
bundle from another machine is **stale, never foreign**: the quoted binary is tested as absolute on
the platform that *wrote* it, so a macOS path is not a conflict on Windows, and it reads as `hooks
need refresh`. Re-pointing stays explicit — `rift init` and the wizard report the foreign path; only
`rift enable --agent antigravity` or the Project Settings toggle rewrites it — otherwise two machines
would commit the path back and forth forever.

---

## 10.5 Detection: one rule, four kinds of evidence

"Is this agent installed here?" is **one predicate** every surface reads — `rift init`'s auto-wire,
Canopy's pre-ticked rows, `rift doctor` and `rift recall status` (`detect` in `rift-agent-install`).
Separate scanners let one clone read "Codex: detected" in the dashboard and "no new selected agents"
from `init`, and hide a Claude Code that lives only in the VS Code extension, whose bundled `claude`
is on no `PATH`. Four kinds of evidence, in this order: a **`PATH command`** (`claude`, `codex`,
`antigravity`/`agy`; bare, `.exe` or the npm `.cmd` shim; never run, so no version); a **`vendor
install`** (Claude Code's native installer in `~/.local/bin`; Antigravity's and Codex's application
bundles, verified by `product.json` and bundle identifier); a **`<host> extension`** verified under
a VS Code-compatible root (`~/.vscode`, `~/.vscode-insiders`, `~/.cursor`, `~/.windsurf`) —
`Anthropic.claude-code` or `openai.chatgpt` — whose bundled binary is the runnable reported; and a
**`JetBrains plugin`** for Claude Code (a jar, no binary, so only "installed"). "Verified" means the
package manifest declares the expected publisher and name; symlinks are skipped and the highest of
several versions wins. `PATH`, home and application roots are injected, so no test reads the machine
it runs on.

Detected is any of the four, spelled by one sentence everywhere — `present via VS Code extension
2.1.267`, `present via PATH command + vendor install`, or `absent` — so when `init` wires an agent you
did not expect, the reason is on screen in the same words. It claims availability, not use.

**Codex's third fact: hook trust.** Codex trusts hooks **per absolute path, per hook and per hash**,
recorded in its own configuration (`[hooks.state]` in `~/.codex/config.toml`, or under `$CODEX_HOME`),
and silently skips any hook whose hash differs — so a hook Rift rewrites is skipped until you trust it
again. Rift reproduces Codex's normalised hook hash for the versions its fixtures cover and compares
it read-only: each hook is **trusted**, **modified**, **untrusted** or **unverifiable** (another
version, or no Codex session recorded in the checkout yet; Rift says so rather than guessing). The
comparison runs at every write of the hook file and every status read, never on a hook's decision
path. When any hook is modified or untrusted the Codex row reads `error` with one remedy sentence
(`codex_trust_remedy` in `--json`): run `codex` in the repository, type `/hooks`, trust the hooks
marked 'Modified since last trusted' (in VS Code, then reload the window). Rift never trusts a hook
for you or writes Codex's configuration. A new linked worktree's `.codex/hooks.json` is a **new
absolute path**, so it starts untrusted though perfectly wired
([09-worktrees-and-families.md](09-worktrees-and-families.md)).

A skipped Codex hook does not stop the session: it opens on its first captured event
(`synthesized-start`) and Codex's own `turn_id` is the turn's identity
([03-capture.md](03-capture.md) §3.6).

---

## 10.6 The six that are refused, and exactly why

Two groups, two reasons. **Four are refused by measurement**: each declares a hook surface the agent
does not send (Cursor three verbs against seven events, Copilot CLI four against eight, OpenCode four
against five plugin events with no tool boundary, Factory Droid five against nine), so an install would
write files, report green and capture *nothing*. **Ollama has no lifecycle**: a model runner has no
sessions, turns or tool boundaries, and absence is the correct outcome. **Gemini CLI is refused by
scope decision**, not measurement; its adapter declares the eleven events Gemini publishes. The two are
kept apart because a refusal stated as a defect outlives its facts the moment the defect is fixed.

---

## 10.7 Deterministic context delivery

Rift computes one transport-independent `ContextPlan` at native hook boundaries. The dispatcher —
not the model, not a rules-file prompt — chooses between a pending read replacement, recovery map,
session pointer and delta manifest in a fixed priority order; each adapter only encodes the plan into
its client's documented response schema.

**A channel exists only where a fixture pins it.** An adapter may answer on a verb only when a
committed fixture carries that client's **published response schema** for it (repository, tag or
commit, file path, blob hash, a verbatim excerpt), and a conformance test asserts the bytes emitted
per client and verb against those rows. The rule is *no response on a channel no published artifact
pins*; being wrong is bounded, since a client that does not understand a response ignores it.

**Per-family capability caps** (`resolve` in `rift-recall-policy`) read that table, and
`[interception.agents]` may set a mode per family that replaces the repository `mode` before the cap
applies ([07-serving-and-injection.md](07-serving-and-injection.md)):

| Fact | Claude Code | Codex | Antigravity | Unknown family | Without it |
|---|---|---|---|---|---|
| Confirmed reason channel (a pinned row declines with a reason *and* a recorded run shows it reaching the model) | yes | yes | yes | no | `enforce` resolves to `observe` — a decline would look like a tool failure, which a model is right to retry |
| Document-sidecar rewrite (a read's path amended to a Rift-owned sidecar) | yes | no | no | no | lever `off` (*no native document/search amendment channel*) |
| Grep head-limit rewrite | yes | no | no | no | lever `off` |
| Pinned edit echo (an artifact quoting the edit result as telling the model the file is current) | yes | no | no | no | no decline rests on an edit line, since the read that checks an edit is the model's only sight of it |

An unknown family gets every lever `off`. Where the documents differ, so do the clients: **only Claude
Code documents amending its file-read input**, so document substitution happens there alone; Codex's
amendment replaces a redundant shell read with the bounded result the plan chose, Antigravity amends
nothing, and documents reach both as existing sidecars plus a decline naming them. Codex's boundary
channel declares a size limit that Rift writes at install and fits its text under.

**Measured on the installed clients** before a channel shipped: the decline's reason reaches the model
on all three (wrapped front and back on Antigravity, so the assertion there is containment, not an
exact frame); the repeat escape differs by family (one client re-issued nothing in eleven runs, the
others re-issued the identical call at once in ten of ten), so the loop is bounded to one extra round
trip. **A subagent has one boundary**: on Claude Code the hook opening its window fires with the
*parent's* session id beside the child's agent id and neither session start nor prompt submission
fires, so the pointer's claim, manifest baseline and window journal are keyed by (session, agent).
**A reason that is seen is not a reason that is followed**: where documents reach a client as a decline
one of three runs read the sidecar and none booked a saving, so nothing is defaulted on a channel's
existence.

**The instructions file.** Every wired agent gets a managed block in the file its client reads (§10.2),
delimited by `<!-- RIFT-MANAGED:codex:begin -->` and its `:end` twin and pointing to `rift agent-help`
and `rift recall status --json`. Re-installing rewrites **only between the markers**; removal deletes
the file only when Rift created it and nothing else is left; an unowned, unmarked file at the path
makes initialization **stop** rather than adopt it. Nothing in capture, delivery or interception
depends on it.

**Native boundary hooks.** Claude Code and Codex plan context at `SessionStart` and `UserPromptSubmit`,
Antigravity at `PreInvocation`, using its persistent `userMessage` step after the raw call is declined
(its tool-call step semantics are not pinned); an undeliverable replacement lets the identical call
through once. Hook failures fail open and mark support degraded. Codex `PreToolUse` rewrites only a
strict recognised redundant shell read into a shell-safe `printf`; the first configured `updatedInput`
wins, so enablement places Rift's group first, preserving unrelated groups' order.

---

## 10.8 Enabling and disabling

```bash
rift enable  --agent claude-code
rift disable --agent claude-code
```

`enable` refuses an unknown, verb-less or unsupported agent **before** any file or directory exists;
writes the descriptor and client hook file, refusing on the first conflict so a refusal leaves the
tree as it was; writes the Recall block if Recall is enabled; records an append-only enablement event;
and carries the wiring to every other family worktree. `disable` removes **only** fingerprint-matching
artefacts, refuses an edited descriptor, and leaves captured data untouched: it stops future capture,
it does not erase the past ([05-ledger-and-verification.md](05-ledger-and-verification.md)).

**Why enabling reaches other worktrees.** Enablement is one relation for the whole family, under three
rules: the binary written into another member is the family's **binary of record**, never the
executable that ran the command (a throwaway build must not wire the family to a build that will be
deleted); a member that refuses is **named, not overwritten**, and does not fail an enable that
succeeded locally; tracked files and git hooks are untouched. `disable` is symmetric, so an agent
cannot stay live in a worktree nobody is looking at.

---

## 10.9 The same lifecycle from Canopy

Project Settings (the Repository card, `s`, or `/settings`) lists every adapter in registry
order and toggles them through **exactly the same** code path as the CLI — no second
installer sits behind the dashboard. A `coming soon` row says so rather than implying a
wiring the toggle is guaranteed to refuse, and a leftover `conflict` stays visible beside
it. The refusal is predicted in the row and checked before the projection opens, so the
toggle fails as a plain precondition, not a storage error naming a file nothing wrote
([11-canopy-tour.md](11-canopy-tour.md)).

---

## 10.10 Reading the state

**`rift recall status`** prints one row per adapter — all nine, in registry order — under three
independent columns: *descriptor* (`missing`, `managed`, `conflict`, `invalid`), *hooks* (the same plus
`stale`, an earlier Rift bundle a re-run refreshes; `conflict` needs you) and *installed* (the §10.5
sentence). A descriptor alone is never `full` support: the native hook must be configured, a runtime
receipt observed and a boundary response observed; until then the row is `configured-unverified`, and
a hook failure makes it `degraded`. The refused adapters report `coming-soon`. `rift doctor`'s
integrations leg renders the same table (`integrations: W/9 wired`); a Codex hook file Codex is
skipping reads `error` and does not count as wired.

**Integration health** asks what the wiring columns cannot: is what Rift wrote being run, and does it
reach the ledger? `rift status`, `rift recall status` and `rift doctor` read the same facts through one
function and print a headline per finding with its remedy, or `integration health: none`. It only reads. The
reasons: `codex-hooks-skipped` and `codex-trust-unverifiable` (§10.5), `lifecycle-hooks-missing` (tool
calls arrived with no session start or prompt; restart the client, then `rift enable --agent <agent>`),
`hook-binary-missing` (re-run `rift enable` with the build the hooks should run), `hook-binary-disagrees`
(both builds run) and `session-behind-events` (Rift's own defect, repaired by the next hook). `rift enable` alone re-points an agent's hook file at a build; `rift hooks
install --repoint` moves the git hooks and binary of record, not the agents' files; no remedy has Rift
change another client's trust or configuration. `rift agent-help` renders the agent-facing verbs from
the binary's own command tree and is what the instructions block points agents at.

---

## Related chapters

[01-quickstart.md](01-quickstart.md) ·
[02-configuration.md](02-configuration.md) ·
[03-capture.md](03-capture.md) ·
[07-serving-and-injection.md](07-serving-and-injection.md) ·
[09-worktrees-and-families.md](09-worktrees-and-families.md)
