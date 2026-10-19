# 17. Invariants

The **rules the mechanisms exist to keep**: what Rift must always do and must
never do, stated once, each with its reason and what fails if it is broken. Thirty-seven
rules; each has a **Rule**, a **Why** (a rule whose reason is forgotten becomes a
ritual, and rituals get relaxed by whoever is in a hurry) and **Enforced by**: the
type, test or check that fails, or a statement that there is none. 

---

## 17.1 The record

### 1. Canonical tables are append-only

**Rule.** No `UPDATE` or `DELETE` runs against a canonical table; a correction is a
new superseding row. **Why.** A record you can quietly edit is a note, not
evidence, and append-only writers are idempotent, so a rebuild running twice
corrupts nothing. **Enforced by** no mutating API, abort triggers generated from
the table registry, and `every_db_table_is_registered_or_infra`
([13](13-storage-and-schema.md), [5](05-ledger-and-verification.md)).

### 2. Durable capture bytes have exactly one door

**Rule.** Nothing reaches durable capture storage except as the output type of the
redaction pipeline, which has one constructor in one function; none is ever added.
**Why.** "Remember to redact" is a review rule, and review rules fail on a tired
Friday; as a type it is a compile error. **Enforced by** the type (private buffer,
no `Debug`) and `seal_is_minted_only_in_the_redaction_crate`
([4](04-redaction-and-secrets.md)).

### 3. Anything that reshapes capture bytes does it before that door

**Rule.** Compaction, condensation and every other transform of captured content
runs on the raw type and then goes through redaction; none runs after. **Why.** A post-redaction transform would need a second door to build the durable type, and the guarantee would become a convention; the cost, some bytes redacted twice, is accepted.
**Enforced by** rule 2's tests: no API lets a later transform produce a storable value
([3](03-capture.md)).

### 4. A shipped migration is frozen

**Rule.** A shipped migration's SQL is never edited, not even whitespace; change is
a new migration. **Why.** Each migration's bytes are checksummed into every
deployed projection, so one edited byte makes every existing database refuse to open
with an error that looks like corruption. **Enforced by** the checksum comparison at
open ([13](13-storage-and-schema.md)).

### 5. The two hash families never interchange

**Rule.** Where bytes must match what git produces or a consumer re-derives, the
hash is SHA-256; fast internal handles use blake3. **Why.** The chain and sync
artifacts promise something outside the process and need one algorithm forever.
**Enforced by** distinct types with no conversion, so a swap does not compile, and
golden fixtures of each family's wire bytes ([13](13-storage-and-schema.md)).

### 6. Sealed capture is never rewritten

**Rule.** An archived checkpoint and the refs holding capture are never edited;
unwanted content is answered by a superseding record and a policy change going
forward.  **Why.** The shards are the leaves ids and roots derive from, so an edit verifies as tampered. **Enforced by** `rift verify` and the chain ([5](05-ledger-and-verification.md)).

### 7. Errors carry reasons, never bytes

**Rule.** An error carries identifiers, paths and reasons, never transcript, file or
key content.  **Why.** Errors travel into logs and pasted bug reports. **Enforced by** the raw and durable
byte types having no `Debug`, then review ([4](04-redaction-and-secrets.md),
[../../SECURITY.md](../../SECURITY.md)).

---

## 17.2 Capture

Rules 2, 3, 6, 8, 10, 11, 12 and 14 together are what capture never does: no
network, no model, no daemon, no blocking, no guessing, no mutation.

### 8. A hook never blocks git or your agent

**Rule.** Every commit-path hook returns success whatever happens inside it,
including a panic; the one exception is the pre-push screen, which may reject a push
only to protect Rift's own refs. An agent hook is bound the same way, and a declined
read is a response the client's schema documents, not a block.  **Why.** A tool that watches your work must never stop it, or you remove it. **Enforced by** the dispatcher's zero exit on unwind and
`commit_path_hook_never_blocks_even_on_panic` ([3](03-capture.md)).

### 9. The commit path stays inside a hundred milliseconds of git latency

**Rule.** The budget is the time `git commit` waits beyond a bare commit, summed over
its three hooks, at a hard 100 ms ceiling that is set rather than derived.  Agent hooks have a separate budget (median under 100 ms, p95 under 500 ms); never quote one against the other. **Why.**  Work that need not finish before git returns moves off the path. **Enforced by** the harness bars and the artifact audit; nothing in CI measures
latency live ([3](03-capture.md), [15](15-benchmarks.md)).

### 10. Capture makes no network call

**Rule.** Nothing on a capture path opens a socket, and the shipped binaries link no
HTTP client. **Why.** A network call in a millisecond-budget hook is a hang waiting
to happen, and "we do not upload your data" is a promise while "the binary contains
no client" is a property you can check. **Enforced by**
`no_network_crate_in_the_resolved_graph`, which resolves both binaries' graph
transitively ([16](16-development.md)).

### 11. Rift runs no language model

**Rule.** Rift contains and calls no model and needs no API key. **Why.** A model in the capture path makes the record non-deterministic, and every verification property needs the same input to give the same output forever; redaction uses deterministic detectors. **Enforced by** rule 10: a
model call needs an HTTP client ([4](04-redaction-and-secrets.md)).

### 12. There is no resident process

**Rule.** Rift runs no daemon or background service; the detached capture worker is
a one-shot child that exits.  **Why.** A daemon is a support and trust burden: it runs when you are not looking and holds your repository open. Reopening the question needs measurements showing the alternatives (caching, reading git's index directly, keeping the async runtime off the hot path) failed. **Enforced by**
`no_process_spawn_is_reachable_from_the_dashboard_crate` and the worker's shape
([3](03-capture.md)).

### 13. Condensation is anchored to an act of yours

**Rule.** A condense happens because of something you did (by default a commit; else
a push, session-activity interval or touched sensitive path), never a clock of
Rift's own; a session end requests one and an idle sweep condenses sessions left unclosed. **Why.** Capture on an independent schedule yields records nobody can place in a story. The sweep exists because an abandoned session would be lost work, and "sweep and count it" beat "sweep but never count it": a recovery path nobody trusts goes unused. **Enforced by** the entry points: structural, not guarded
([3](03-capture.md) §3.7).

---

## 17.3 What Rift serves back

### 14. Absence is stated, never filled in

**Rule.** When Rift does not know something it says so, naming the reason, and never
substitutes a default, a guess or a plausible figure.  **Why.** An agent told "unchanged" about a file Rift never saw skips a read it needed; absence costs a re-read, a fabrication costs correctness. **Enforced by** the validity
types (a valid summary is unconstructible outside its crate) and
`changed_file_is_never_known_unchanged` ([6](06-recall.md)).

### 15. Uncertainty never widens scope

**Rule.** When Rift cannot prove something is in scope it is out of scope.  **Why.** One store serves all of a repository's checkouts, so the wrong answer is the generous one: another branch's work teaches the agent things untrue of its tree. Every uncertain case (no anchor commit, unknown `HEAD`, a walk at its cap, a commit missing from a shallow clone) excludes and is counted. **Enforced by** the one containment step and the counters
`omitted_out_of_branch` and `unknown_head`
([9](09-worktrees-and-families.md)).

### 16. An unreadable policy denies everything

**Rule.** A serve policy file that exists but cannot be parsed means deny-all for
exact retrieval.  **Why.** A typo must not turn "somebody wrote rules we cannot read" into no rules. **Enforced by** `unparseable_policy_fails_closed_and_denies_all_exact`
([7](07-serving-and-injection.md)).

### 17. Exact content needs a reason, a policy pass and an audit row

**Rule.** Serving exact bytes needs a non-empty reason, a policy check of the path
before any read (sensitive paths included) and an audit row; a denied path stays
denied even with a reason.  **Why.** The policy must see a readable path, so a retrieval name resolves to a path, not a hash.
**Enforced by** `denied_path_is_refused_even_with_reason` and the audit rows
([7](07-serving-and-injection.md) §7.14).

### 18. Untrusted text is never served as fact

**Rule.** Content from outside the committed material (tool output, agent-authored
text) is never given to an agent as exact content, and packs cite evidence by a
pack-local ordinal plus a content address, never attacker-chosen text. **Why.** What
Rift serves reads as instruction as readily as data; an attacker's text quoted with
Rift's authority makes the memory plane a delivery mechanism. **Enforced by**
`untrusted_content_is_never_exact`, the trust class on every artifact and the
red-team corpus ([7](07-serving-and-injection.md)).

### 19. One tokenizer decides, and every budget is enforced

**Rule.** One crate decides what a token is; per-turn and per-response budgets and
caps are enforced, not advisory. **Why.** Two counters give two answers, and an
unbounded response evicts something the agent needed without failing loudly.
**Enforced by** the single counting entry point and `budget_caps_calls_and_tokens`;
over-budget content degrades to a handle ([8](08-cost-and-savings.md),
[7](07-serving-and-injection.md)).

### 20. A read is declined only where the client documents a reason, a run confirmed it, and the record proves the lines are held

**Rule.** One function composes every decline, for a client and verb whose response
shape a committed fixture pins and whose reason field a recorded run showed reaching
the model, only when the window's own record proves the lines are held at the current
hash, and once per key (the repeat runs). It acts at `enforce`; at `observe` (where
the narrowing runs) it
decides and records and the call runs as written. A family's `[interception.agents]`
entry replaces the repository `mode` for that family, and the pinned reason channel
still caps it. Any error or unknown yields the neutral response. **Why.**  A decline takes away something the agent asked for, so each clause is a way of not being wrong: no visible reason teaches nothing, an unconfirmed channel bets a turn on an unchecked schema, a repeat decline is a loop.
**Enforced by** `xtask gate-intercept`, which fails on a decision literal outside
that function ([7](07-serving-and-injection.md) §7.4, [10](10-agent-integrations.md)).

### 21. Nothing a hook decides with opens a store or starts a tokenizer

**Rule.** A tool-call hook's decision path opens no database, initialises no
tokenizer and never walks a directory tree; counting and audit rows happen
afterwards in the detached worker. **Why.** Somebody is waiting: a projection open costs tens of milliseconds and the tokenizer about 234 ms, so either spends the whole budget and a budget met by luck on a small repository is not met; a walk costs the size of somebody's repository, so a stale index is rebuilt by the worker and the call simply runs; an unavailable store then costs a row, never a turn. **Enforced by** a call-graph leg of the gate
([7](07-serving-and-injection.md) §7.4).

### 22. A tool call is amended only in a pinned form, and the amendment is verified

**Rule.** Rift changes an agent's tool input only in a closed set of forms the
client's schema documents (two additive fields, and one bounded replacement of a
strict redundant shell read), never on a decline, and afterwards compares what the
tool ran with against what Rift sent. **Why.**  Amending is the most invasive act on this path, since the agent believes it read what it asked for; verifying afterwards tells Rift when an amendment lost (several hooks may answer one call, last to finish wins), and a dropped one falls back to a channel that cannot be dropped, or records only, never a guess. **Enforced by** the gate's rewrite-form leg
([7](07-serving-and-injection.md) §7.6).

---

## 17.4 Your repository

### 23. Rift never writes a file it does not own

**Rule.** Rift rewrites an integration file only if a recorded fingerprint proves it
wrote it; unrelated content is preserved, a conflicting file is refused, and a
symlinked target or parent directory is refused outright.  **Why.** A tool that clobbers a hand-written block is not installed twice. **Enforced by** the
ownership fingerprint and negative-case tests ([10](10-agent-integrations.md)).

### 24. A repository identity is minted by an act, never re-minted on its own

**Rule.** A family's identity is minted once, by an explicit command, into the shared
git directory; a checkout that cannot prove it is the main work tree refuses to
mint, and durable state is keyed by family, never by a checkout's path. **Why.** A
second identity is a second, empty world, and the user concludes the tool lost their
data. **Enforced by** the single manifest writer and the named-reason refusals
([9](09-worktrees-and-families.md)).

### 25. The supported set is closed, and the rest refuse before writing

**Rule.** Three agent integrations are supported (Claude Code, Codex, Antigravity);
the other six adapters stay registered and refuse to enable before writing anything.
 **Why.** They declare hook surfaces the agents never send: a green install capturing nothing. **Enforced by** `every_alpha_agent_still_enables`,
which asserts the count ([10](10-agent-integrations.md)).

### 26. An unknown configuration key is an error; an absent one is the safe default

**Rule.** Every configuration structure rejects unknown fields, and a missing key
takes the conservative default. **Why.** A silently ignored key is the cruellest bug,
and a user who never wrote a key did not choose to disable a safeguard. **Enforced
by** `deny_unknown_fields` on every `rift-config` structure ([2](02-configuration.md)).

### 27. The built-in sensitive paths cannot be dropped

**Rule.** Rift ships path patterns always treated as sensitive; your configuration is
unioned into the list and no key removes an entry. **Why.** One repository's
convenience edit must not disable a protection every other rule assumes. **Enforced
by** `builtin_sensitive_paths_cannot_be_dropped` ([2](02-configuration.md)).

### 28. Nothing leaves your machine unless you name where it goes

**Rule.** No sync by default, no telemetry, no fallback destination; the shipper
never guesses a target from your git remotes. **Why.** A tool holding a complete
record of your work must make this unambiguous; inferring a destination is
convenient once and catastrophic the first time it is wrong. **Enforced by** the
explicit-destination requirement and rule 10 ([14](14-sync-readiness.md)).

---

## 17.5 The code

### 29. No `unsafe`, no warnings

**Rule.** No crate uses `unsafe`, clippy runs pedantic with warnings denied,
and a crate-root blanket allow is refused. **Why.** A blanket allow silences a lint
for everything below it forever, including code nobody has written. **Enforced by**
the ci-meta checks ([16](16-development.md)).

### 30. A new canonical table brings its guard; coverage floors never move down

**Rule.** A canonical table arrives with its registry row and guard in one change,
and the coverage floors are never lowered to make a change pass.  **Why.** A floor that moves when inconvenient is a preference. **Enforced by** rule 1's tests and the coverage check
([16](16-development.md)).

### 31. Tests inject the clock, the randomness and the identity

**Rule.** A test injects its time, random and identity sources. No test reads the
network, and no assertion depends on the wall clock: a test may read it only to bound
a wait or to seed a fixture, never to decide its verdict. **Why.** A suite whose
verdicts depend on the clock fails at midnight or on a slow machine. **Enforced by** the injected source traits and the
fixtures; the limit is that it holds for assertions, not every line of test code
([16](16-development.md) §16.7).

### 32. Every modal surface joins the parity fixtures

**Rule.** Anything drawn as a centred modal frame carries the shared close
affordance, and every act its hints teach is reachable by key and by pointer.  **Why.** A modal that cannot close is a trap. **Enforced by** `no_new_modal_surface_was_introduced` and
`every_hint_key_is_handled_by_the_reducer` ([12](12-canopy-architecture.md)).

### 33. A check that cannot fail says so

**Rule.** An instrument that publishes a verdict without being able to fail the build
says so beside the verdict. **Why.**  A comparison that reds on machine weather trains everyone to ignore a red light. **Enforced by** the instrument printing its report-only status
([15](15-benchmarks.md)).

### 34. A number without a tracked artifact is not a result

**Rule.** Every published performance figure comes from a tracked artifact written
under the pinned protocol.  **Why.** An unqualified number describes the machine and the moment. **Enforced by** the artifact audit, which reds on a missing
or stale artifact or prose that disagrees with its data
([15](15-benchmarks.md)).

### 35. Deleted things stay deleted

**Rule.** Deliberately removed paths (two reversed scaffold crates, the retired
server-transport entry point, the helper document binary, the deleted wrapper crate
and others) are guarded against returning. **Why.** Each came back at least once by an editor, merge or tidy-up. **Enforced by**
`deleted_paths_stay_deleted` ([16](16-development.md)).

### 36. Nothing stages the whole tree

**Rule.** No script or automation runs a stage-everything git command. **Why.** An
un-membered directory under `crates/` reds the membership check, and on a synced
filesystem a blanket stage hangs on evicted files. **Enforced by** the membership
check for the first; the second is a working rule ([16](16-development.md) §16.13).

### 37. A claim names which kind of evidence it has

**Rule.** Documentation and reports distinguish verified locally, observed in
operation, deliberately accepted and left open, and not run at all. **Why.** A
document that reads uniformly confident cannot be audited. **Enforced by** review,
the artifact audit and the honesty ladder ([8](08-cost-and-savings.md),
[18](18-what-rift-does-not-do.md)).

---

## 17.6 What this register is not

It is not the security model ([../../SECURITY.md](../../SECURITY.md) is, and wins where the two touch) and not a list of everything Rift does. A relaxed rule gets its entry rewritten with the new reasoning.
