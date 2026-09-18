# 17. Invariants

Every chapter before this one explains a mechanism. This chapter is the register
of the **rules those mechanisms exist to keep** — the things Rift must always do
and must never do, stated once, in plain language, with the reason each exists
and the thing that fails if it is broken.

Nothing here is new. Each rule is already at work somewhere in the guide; this
is where you look when you want the rule itself rather than the machinery, or
when you are about to change something and want to know which line you are
standing near. There are thirty-seven — a lot to hold in your head, which is
why they are written down.

> Tip: If one of these feels inconvenient while you are reviewing a change, that
> is normally the entry doing its job: every one was written because the
> alternative was tried, or costed out before it was tried.

---

## 17.1 How to read an entry

Each entry gives the **Rule** (the sentence that binds), the **Why** (a rule
whose reason is forgotten becomes a ritual, and rituals get relaxed by whoever
is in a hurry), **Enforced by** (the type, test or check that fails when it is
broken, or a statement that there is none) and **Read more**.

Two standing properties of the register itself. **A rule is either guarded or
honestly unguarded** — by a type that makes the violation a compile error, by a
test that reds, or by review discipline; the entries distinguish those, because
"we have a rule about that" and "the build stops you" are different assurances.
And **where a rule and the code disagree, the code wins and the entry says
so**, because a register describing intentions rather than behaviour teaches
you to trust a claim the machine is not making.

---

## 17.2 The record

Six rules protecting the property the toolchain is sold on: that the record of
what your agents did is worth keeping, and worth trusting later.

### 1. Canonical tables are append-only

**Rule.** No `UPDATE` and no `DELETE` ever runs against a canonical table. A
correction is a new row that supersedes an older one; the older one stays.

**Why.** A record you can quietly edit is not evidence, it is a note. The
verification story in [Chapter 5](05-ledger-and-verification.md) rests on the
rows underneath it never moving, and append-only writers are idempotent, so a
rebuild running twice cannot corrupt anything.

**Enforced by** three layers: the **type layer** (canonical entities implement a
marker trait and expose no mutating API), the **SQL layer** (`BEFORE UPDATE` and
`BEFORE DELETE` triggers raising an abort on every canonical table, generated
from the registry that creates the table), and a **property test** —
`every_db_table_is_registered_or_infra` and `forgotten_registration_is_caught`
in `rift-store`'s immutability suite, which open a real database and refuse a
table with no guard.

**Read more.** [13](13-storage-and-schema.md).

### 2. Durable capture bytes have exactly one door

**Rule.** Nothing reaches durable capture storage except as the output type of
the redaction pipeline, and that type has exactly one constructor, called in
exactly one function. No second constructor is ever added.

**Why.** "Remember to redact before writing" is a review rule, and review rules
fail on a tired Friday. As a type it is a compile error rather than an incident,
and it survives refactors by people who never read this chapter.

**Enforced by** the type itself (private buffer, no conversion from a plain byte
vector, no `Debug`), plus two source-level tests in `rift-shadow-redact`:
`seal_is_minted_only_in_the_redaction_crate`, which walks the workspace and pins
the call sites, and `no_other_fn_seal_is_defined_in_the_workspace`.

**Read more.** [04](04-redaction-and-secrets.md).

### 3. Anything that reshapes capture bytes does it before that door

**Rule.** Compaction, condensation and any other transform of captured content
happens on the **raw** side, on the secret-bytes type, and the result is handed
to the redaction pipeline like any other capture. No transform runs after
redaction.

**Why.** This is rule 2 read forwards. A transform applied *after* redaction
would need a second door to construct the durable type from its own output, and
the moment that exists the guarantee is a convention again. The cost is real
(some bytes are redacted more than once) and was accepted knowingly.

**Enforced by** the same single-constructor tests as rule 2: no API lets a
post-redaction transform produce a storable value, so the rule is kept by the
absence of a mechanism rather than by a check.

**Read more.** [04](04-redaction-and-secrets.md),
[03](03-capture.md).

### 4. A shipped migration is frozen

**Rule.** Once a migration has shipped its SQL text is never edited — not to fix
a comment, rename a column or tidy whitespace. Change arrives as a new
migration.

**Why.** Each migration's bytes are checksummed into every deployed projection,
so editing one byte makes every existing database refuse to open with an error
that looks like corruption and is really a diff. The frozen text keeps names
Rift no longer uses, because a name in a shipped migration is a fact about
databases that already exist.

**Enforced by** the checksum comparison at open time — a mismatch is a hard
refusal — plus the schema snapshot tests pinning the shape a fresh database
comes out with.

**Read more.** [13](13-storage-and-schema.md).

### 5. The two hash families never interchange

**Rule.** Where bytes must match what git itself would produce or what a future
consumer will re-derive, the hash is SHA-256. Fast internal handles use blake3.
The two are distinct types and one is never substituted for the other.

**Why.** The chain, the content digests inside a checkpoint and the sync
artifacts are promises to something outside this process — a verifier, another
checkout, a future reader — and need the same algorithm forever; internal
handles need speed. Separate types mean a mistaken swap does not compile, rather
than being wrong in a way nothing notices until a verify fails months later.

**Enforced by** the type system (distinct identifier types with no conversion)
and by the golden fixtures pinning the wire bytes of each family's artifacts.

> **Where the code and an older description disagree.** Some source comments call
> worktree content hashing blake3. It is not: a file read from a checkout is
> hashed with plain SHA-256, matching the capture recorded at read time. The code
> is the authority; the stale phrasing is listed in [18](18-known-gaps.md).

**Read more.** [13](13-storage-and-schema.md).

### 6. Sealed capture is never rewritten

**Rule.** A checkpoint that has been archived is never edited, and the refs
holding capture are never rewritten. If content in a sealed checkpoint is
unwanted, the answer is a new superseding record and a policy change going
forward — never a scrub of what is already sealed.

**Why.** The archived shards *are* the leaves checkpoint identifiers and roots
are derived from. Editing one does not remove information; it produces a record
whose own verification calls it tampered — correct, but it makes a well-
intentioned cleanup and an attack indistinguishable from outside, so neither is
allowed.

**Enforced by** `rift verify`, which recomputes the leaves and roots and reports
tamper when they disagree, and by the chain, which makes a rewritten branch
visible as an orphaned position rather than a silent gap.

> **One historical exception, stated plainly.** This repository's own git
> history was collapsed to a single fresh root once, by the project owner, on
> the day the design corpus was folded into this guide. That was a deliberate,
> one-time act on the *source* history, taken with verified backups, and it is
> why several published benchmark bars are recorded as decisions rather than
> derivations ([15](15-benchmarks.md)). It was not a rewrite of captured
> content, and the rule holds from that day forward with no further exceptions.

**Read more.** [05](05-ledger-and-verification.md).

### 7. Errors carry reasons, never bytes

**Rule.** An error value carries identifiers, paths, keys and reasons. It never
carries transcript content, file content, or key material.

**Why.** Errors travel further than any other value in a program: into logs,
scrollback, a bug report pasted into a chat window. A redaction wall a panic
message walks around is not a wall, and the rule costs nothing — a reason plus an
identifier is usually *more* useful than a dump.

**Enforced by** the types more than by a test: the raw-bytes type has no `Debug`
and no `Display`, and the durable type derives no `Debug` either, so the usual
accidental route into a message does not compile. Beyond that it is **review
discipline**, supported by the error enum's documentation naming the
constraint.

**Read more.** [04](04-redaction-and-secrets.md),
[../../SECURITY.md](../../SECURITY.md).

---

## 17.3 Capture

Capture runs inside somebody else's critical path — your `git commit`, your
agent's turn — and six rules make that acceptable.

### 8. A hook never blocks git or your agent

**Rule.** Every commit-path hook returns success to its caller, whatever
happened inside it: a capture failure, a configuration problem, a missing
database, even a panic. The single exception is the pre-push screen, which may
reject a push, and only to protect Rift's own refs. An agent hook is bound the
same way, and a **declined read is not an exception to it**: the decline is a
response the client's own schema documents, the hook still exits successfully
and within budget, the repeat of the declined call runs, and a family whose
reason channel is unconfirmed is never declined to at all (rule 20). Blocking
is a hook that stops the caller; deciding is a hook that answers it.

**Why.** A tool that watches your work must never become one that stops your
work: if a bug in capture can fail your commit you will remove the tool —
correctly — and the record dies with it. So capture is best-effort by
construction, and what it missed is reported later by `rift doctor`, where a
failure costs nothing.

**Enforced by** the hook dispatcher, which wraps every handler and exits zero on
the unwind path, and by `commit_path_hook_never_blocks_even_on_panic`, which
drives the real binary through a deliberately panicking handler.

**Read more.** [03](03-capture.md).

### 9. The commit path stays inside a hundred milliseconds of git latency

**Rule.** The budget is **git latency** — the time your `git commit` waits
beyond a bare commit, summed over all three of the commit's hooks. The bar is
100 ms: a hard ceiling the project's owner set rather than one derived from a
measurement, so it does not move when the machine has a good day. The
agent-side hooks have a **separate** budget — the wall time of one `rift hooks`
process including its ingest, aimed at a median under 100 ms and a 95th
percentile under 500 ms — and quoting either against the other is the mistake
this pair of sentences exists to prevent.

**Why.** The definition is the interesting half: what a developer notices is the
pause before the prompt comes back, so that is what gets budgeted, and work that
need not finish before git returns is moved off the path rather than optimised.
The number is a judgement about where a tool starts feeling like a tax.

**Enforced by** the bars the harness carries — `BUDGET_MS` in
`benches/native-baseline/src/hooklat.rs:70`, the agent-hook pair in
`benches/native-baseline/src/measure.rs:110-111`, and the `enable-displacement`
row in [`benchmarks/floors.toml`](../../benchmarks/floors.toml) (lines 93-97) —
and by the artifact audit, which raises a violation for a measured latency over
its budget and is asserted clean by
`the_committed_benchmarks_tree_is_internally_consistent` in the required
benchmark CI job. Note what that does and does not mean: **nothing in CI
measures latency live.** The committed artifacts are held to the bar, and
agent-side arms measured above their budget are recorded openly rather than
hidden.

**Read more.** [03](03-capture.md),
[15](15-benchmarks.md).

### 10. Capture makes no network call

**Rule.** Nothing on a capture path opens a socket. The shipped binaries do not
link an HTTP client at all.

**Why.** Two reinforcing reasons. A network call inside a git hook is a hang
waiting to happen — a hook budgeted in milliseconds cannot tolerate a stalling
name lookup. And a system recording everything your agents do is only
trustworthy if it demonstrably cannot send that anywhere: "we do not upload your
data" is a promise, "the binary contains no client" is a property you can check.

**Enforced by** two checks in the workspace metadata pass:
`no_network_deps_in_capture_crates` and `no_network_crate_in_the_resolved_graph`,
the second resolving the actual dependency graph of the shipped binaries and
failing if a network crate appears anywhere in it, including transitively —
which is where this kind of thing normally arrives.

**Read more.** [03](03-capture.md),
[16](16-development.md).

### 11. Rift runs no language model

**Rule.** Rift contains no model, calls no model, and needs no API key to do
anything described in this guide. It records what your agents do and computes
over the record.

**Why.** People assume the opposite, so it is worth stating as a rule. The
reasoning is rule 10's plus one more: a model in the capture path would make the
record non-deterministic — the same bytes classified differently on two runs —
and every verification property in [Chapter 5](05-ledger-and-verification.md)
depends on the same input producing the same output forever. The one place a
model was considered, a heavy batched privacy pass over captured content, was
deliberately not built; the pipeline redacts with deterministic detectors first.

**Enforced by** rule 10's graph check: a model call needs an HTTP client, no
workspace crate declares one, and the resolved dependency graph of both shipped
binaries is read to prove it.

**Read more.** [04](04-redaction-and-secrets.md),
[08](08-cost-and-savings.md).

### 12. There is no resident process

**Rule.** Rift runs no daemon, no background service, and nothing that survives
the command that started it. The detached capture worker is a one-shot child
process that does its work and exits.

**Why.** A daemon is a support burden and a trust problem: something running
when you are not looking, holding your repository open, with its own crash modes
and upgrade story. The question may only be reopened with measurements showing
the alternatives failed — the levers without one are caching, reading git's own
index directly, and keeping the async runtime out of the hot path.

**Enforced by** `no_process_spawn_is_reachable_from_the_dashboard_crate` and by
the shape of the detached worker: spawned by a hook, journals its work first,
exits. Nothing installs a service.

**Read more.** [03](03-capture.md),
[12](12-canopy-architecture.md).

### 13. Condensation is anchored to an act of yours

**Rule.** A condense happens because of something you did, never because a clock
of Rift's own said so. By default that act is a commit; under the other
configured strategies it is a push, an elapsed interval driven by your session
activity, or a touched sensitive path — which always archives immediately
whatever the base strategy. The end of a session always requests one, and
`rift doctor` sweeps sessions that went idle and were never closed.

**Why.** Capture on an independent schedule produces records nobody can place
in a story; anchoring every archive to an act gives each checkpoint something
it belongs to, which makes the ledger readable a month later. The sensitive-
path overlay is where urgency wins. The sweep exists because an abandoned
session would otherwise be lost work — "sweep and count it" beat "sweep but
never count it", since a recovery path nobody trusts is unused.

**Enforced by** the entry points rather than a dedicated check — the commit-
path hooks, the session-end job the state machine journals and the worker
drains, and the doctor sweep. This one is **structural, not guarded**.

**Read more.** [03](03-capture.md) §3.7 for the strategy table.

---

## 17.4 What Rift serves back

The recall plane hands text to an agent that will act on it, and now declines
some of what it asks for. Nine rules keep that honest.

### 14. Absence is stated, never filled in

**Rule.** When Rift does not know something it says so in the response, with a
name for the reason. It never substitutes a default, a guess, or a
plausible-looking figure.

**Why.** An agent told "this file is unchanged" when Rift has never seen it is
confidently wrong and will skip a read it needed: honest absence costs a re-
read, a fabricated answer costs correctness. So the envelope has four states
rather than two, an unbound summary reports itself unbound, and a serve that
could not decide says so.

**Enforced by** the validity types — the "valid summary" value cannot be
constructed outside its crate, so no caller can assert freshness the store did
not establish — and by `changed_file_is_never_known_unchanged` and
`seen_but_invalid_is_never_known_unchanged`.

**Read more.** [06](06-recall.md),
[07](07-serving-and-injection.md),
[08](08-cost-and-savings.md).

### 15. Uncertainty never widens scope

**Rule.** When Rift cannot prove that something is in scope, it is out of scope.
Not knowing is never a licence to show more.

**Why.** One store serves a repository and all of its checkouts, so "which of
these records may this caller see" has a wrong answer available, and it is
generous: handing back a neighbouring branch's work because the branch axis could
not be decided leaks context between lines of work and teaches an agent things
untrue of the tree in front of it. So every uncertain case — no anchor commit, an
unknown `HEAD`, a walk that hit its cap, a commit object missing from a shallow
clone — excludes and is counted.

**Enforced by** the containment step in the serving path (one place,
deliberately), the ancestry walk's cap-and-exclude behaviour, and the counters
`ScopeSet::omitted_out_of_branch` and `unknown_head` in the response's
containment block, which make a silent widening visible in the answer itself.
Widening requires an explicit grant, and grants are audited.

**Read more.** [09](09-worktrees-and-families.md).

### 16. An unreadable policy denies everything

**Rule.** If the serve policy file exists but cannot be parsed, the effective
policy is deny-all for exact retrieval. A broken policy is never treated as an
absent one.

**Why.** The two cases look similar and mean opposite things. No policy file
means "nobody has restricted this yet" and the secure default applies; a
malformed one means "somebody wrote rules here and we cannot read them", and
treating that as no rules is what a typo must not be able to cause.

**Enforced by** the policy loader, which returns a fail-closed snapshot for the
refused case, and by `unparseable_policy_fails_closed_and_denies_all_exact` plus
`an_unparseable_policy_injects_nothing` in `rift-recall-policy`.

**Read more.** [07](07-serving-and-injection.md),
[02](02-configuration.md) for the file itself.

### 17. Exact content needs a reason, a policy pass and an audit row

**Rule.** Serving the exact bytes of a file is a gated act. The caller supplies a
non-empty reason, the path is checked against the policy — including the
sensitive-path rules — before any bytes are read, and the resolution is written
to the audit record. A denied path stays denied even with a reason.

**Why.** Deltas and summaries are derived, bounded and cheap to be wrong about;
exact content is the real thing. The reason field is the audit line answering
"why did this file leave the repository", and the ordering matters: the policy
sees the human-readable path *before* any read, which is why a retrieval name
resolves to a path rather than to a hash the gate cannot interpret.

**Enforced by** `exact_requires_reason`,
`sensitive_path_requires_reason_specifically`,
`denied_path_is_refused_even_with_reason` and
`traversal_dressed_path_cannot_dodge_a_denied_glob` in `rift-recall-policy`, and
by the audit rows the serving path writes for every resolution.

**Read more.** [07](07-serving-and-injection.md).

### 18. Untrusted text is never served as fact

**Rule.** Content originating outside the repository's own committed material —
tool output, agent-authored text, anything an attacker could influence — is
never handed to an agent as exact content, and the agent-facing pack cites
evidence by a pack-local ordinal plus a content address, never by attacker-
chosen text.

**Why.** Anything Rift serves lands in an agent's context, where it is read as
instruction as readily as data: if an attacker gets text into a repository and
has Rift quote it back with Rift's authority, the memory plane is a delivery
mechanism. Filtering for suspicious phrases was tried and rejected as
unwinnable; the answer is structural — the surface reaching an agent is
enumerated, what rides on it bounded and content-addressed.

**Enforced by** `untrusted_content_is_never_exact` and
`untrusted_artifact_is_never_exact_included`, by the trust classification on
every artifact, and by the injection corpus in the red-team suite, which replays
known attack shapes through the real serving path.

**Read more.** [07](07-serving-and-injection.md).

### 19. One tokenizer decides, and every budget is enforced

**Rule.** One crate is the authority on what a token is, and everything counting
tokens calls it. Budgets and caps — per-turn call counts, per-turn token
allowances, per-response size ceilings — are enforced, not advisory.

**Why.** Two token counters in one system produce two answers, and somebody
then decides which the budget meant; one authority also means the accounting in
[Chapter 8](08-cost-and-savings.md) and the caps in
[Chapter 7](07-serving-and-injection.md) speak of the same unit. The caps exist because the serving path
spends the caller's context: an unbounded response does not fail loudly, it
evicts something the agent needed.

**Enforced by** the single counting entry point every serving and cost crate
calls, and by `budget_caps_calls_and_tokens` plus
`token_counter_never_overflows_under_the_ceiling` in `rift-recall-policy`.
Over-budget content degrades to a handle rather than truncating mid-thought.

**Read more.** [08](08-cost-and-savings.md),
[07](07-serving-and-injection.md).

### 20. A read is declined only where the client documents a reason, a run confirmed it, and the record proves the lines are held

**Rule.** One function composes every decline. It emits one only for a client
and verb whose response shape a committed fixture pins, whose reason field a
recorded run showed reaching the model, when the context window's own record
proves the requested lines are held at the file's current hash, and then only
once per key — the identical repeat is allowed. The same function, under the
same conditions, is the only thing that answers a name search in place of a
listing; a search *inside* file contents is never answered this way and never
substituted by a search over names. Any error, any unknown span,
any unpinned family, any unreadable policy: the neutral response, and the call
runs as written.

**Why.** A decline is the only thing on this path that takes away something an
agent asked for, so every clause of it is a way of not being wrong. Declining
without a visible reason teaches nothing and looks like a broken tool;
declining on an unconfirmed channel bets an agent's turn on a schema nobody
checked; declining twice for the same request is a loop with the agent's
context as fuel. One composer exists so those conditions are checked in one
place rather than in each adapter, and a client Rift cannot decline to keeps
every other lever. Substituting a content search would break the same rule from
the other side: the answer to *which lines contain this* is not in any index
Rift holds, so standing in for it would send a model looking somewhere the
answer is not.

**Enforced by** the read-interception build gate, which fails when a decision
literal appears anywhere but that one function, when the answered-channel table
and the fixture rows disagree, or when a reason frame is composed outside the
one module that writes the sentences a model reads; and by the tests asserting
that an unpinned family never declines, that an off mode never declines, and
that the second identical request runs.

**Read more.** [07](07-serving-and-injection.md) §7.4 and §7.7,
[10](10-agent-integrations.md).

### 21. Nothing a hook decides with opens a store or starts a tokenizer

**Rule.** The decision path a tool-call hook runs opens no database and
initialises no tokenizer, and never walks a directory tree. Hashing is bounded
by a ceiling, the state it reads is files — including the path index, which is
read under a size bound or not at all — and the counting and the audit rows
happen afterwards in the detached worker.

**Why.** Somebody is waiting. A projection open costs tens of milliseconds and
the tokenizer costs roughly 234 ms before it sees a byte, so either one on this
path spends the whole budget on infrastructure and leaves the decision
unaffordable — and a budget met by luck on a small repository is not met.
Pushing the counting to the worker also means an unavailable store costs a row,
never a turn. A walk is the same argument in a different unit: its cost is the
size of somebody's repository, which is not a number this path is allowed to
depend on, so a stale index is rebuilt by the worker and the call it could not
answer simply runs.

**Enforced by** a call-graph leg of the same build gate, which walks every gate
entry point for a store open or a tokenizer construction, and by the published
hook rows in [03](03-capture.md) §3.8.

**Read more.** [07](07-serving-and-injection.md) §7.4 and §7.7,
[03](03-capture.md) §3.8.

### 22. A tool call is amended only in a pinned additive form, and the amendment is verified

**Rule.** Rift changes an agent's own tool input only by adding or substituting
a field the client's published schema documents for that purpose — a closed,
pinned set of additive forms, today a document read's path and a content
search's missing result bound — never on a decline, never any other field, and
never on a client that documents no such thing. What the tool actually ran with
is compared afterwards against what Rift sent.

**Why.** Amending a call is the most invasive thing on this path: the agent
believes it read what it asked for. Keeping the forms closed and additive means
the call still does what the agent intended, only cheaper; verifying afterwards
means Rift finds out when its amendment lost — several hooks may answer one
call and the last to finish wins — instead of assuming a lever is working. A
dropped amendment falls back to a channel that cannot be dropped, or records
only, and never to a guess.

**Enforced by** the amendment-form leg of the build gate, which reads every
construction site and fails on a field outside the closed set, and by the tests
covering a dropped amendment on each channel that has a fallback.

**Read more.** [07](07-serving-and-injection.md) §7.6 and §7.7,
[02](02-configuration.md).

---

## 17.5 Your repository

Rift writes files into a repository you own, wires itself into agents you
chose, and knows things about your work; six rules bound that.

### 23. Rift never writes a file it does not own

**Rule.** Rift only rewrites an integration file it can prove it wrote,
identified by a recorded fingerprint. Unrelated content in a shared file is
preserved; a conflicting or unrecognised file is refused with an explanation,
never overwritten; a symlinked target, or a symlinked directory on the way to
it, is refused outright.

**Why.** These files live in your repository and are often shared with tooling
unrelated to Rift; a tool that clobbers a hand-written block once will not be
installed twice. The symlink refusal is the security half: a write that follows
a link is a write to somewhere you did not name.

**Enforced by** the ownership fingerprint recorded per artifact, the symlink
refusals on both the file and its parent directory, and tests asserting the
negative cases — including one pinning that enabling a particular agent does
*not* create a tracked hook file Rift only ever reads.

**Read more.** [10](10-agent-integrations.md).

### 24. A repository identity is minted by an act, never re-minted on its own

**Rule.** A repository family's identity is minted once, by an explicit command,
into the shared git directory. Nothing re-mints it automatically. A checkout
that cannot prove it is looking at the main work tree refuses to mint rather
than minting something plausible. The same rule shapes every new piece of
durable state: it is keyed by the family and lives in the shared git directory,
or it is per member under that checkout's own state directory — never by a
checkout's path, which is the derivation that produced a second empty world in
the first place.

**Why.** That identity keys the projection, the recall store and the registry
row. A second identity produces not an error but a second, empty world, and a
user who concludes the tool lost their data. The refusal case matters most: a
vantage point that cannot prove what it sees would mint an identity for the
wrong directory, silently. So the failure arms are refusals with instructions,
and repair is an explicit act.

**Enforced by** the single writer module for the identity manifest, the
unminted-state refusals with their named reasons, and the explicit re-mint
command being the only path that clears a minted identity.

**Read more.** [09](09-worktrees-and-families.md).

### 25. The supported set is closed, and the rest refuse before writing

**Rule.** Three agent integrations are supported. The other six adapters stay
registered and visible, and refuse to enable — before writing anything — with a
message saying they are not supported yet.

**Why.** Those adapters declare hook surfaces the real agents never send, so an
install that appeared to succeed would capture nothing: a green setup line and
an empty ledger, with the user believing they are covered. Keeping the
unsupported ones listed matters too: an adapter that quietly disappears looks
like it was never considered.

**Enforced by** the supported-set constant, `every_alpha_agent_still_enables`
(which asserts the count, so a silent widening cannot pass as "the loop ran"),
and `an_unsupported_selection_reports_every_offender_in_order_and_once_each`,
pinning that a wiring run reports *all* refusals at once.

**Read more.** [10](10-agent-integrations.md).

### 26. An unknown configuration key is an error; an absent one is the safe default

**Rule.** Every configuration structure rejects unknown fields. A missing key
resolves to the conservative default, never to "off" for a protection or "on"
for a disclosure.

**Why.** A silently ignored key is the cruellest configuration bug: you wrote
the setting, the file parsed, the behaviour never changed. Refusing the file
tells you at once, and the default direction applies the same reasoning to
omission — a user who never wrote a key did not choose to disable a safeguard.

**Enforced by** the deserialization attribute on every configuration structure
in `rift-config`, and by the defaults being defined on the structures themselves
rather than assembled by callers.

**Read more.** [02](02-configuration.md).

### 27. The built-in sensitive paths cannot be dropped

**Rule.** Rift ships a list of path patterns always treated as sensitive. Your
configuration is unioned *into* that list. There is no key that removes an entry
from it.

**Why.** The list is short and uncontroversial — key material, credential files,
environment files — and shortening it lets one repository's convenience edit
silently disable a protection every other rule assumes. Union keeps the extension
point without the foot-gun.

**Enforced by** the union in the configuration loader and by
`builtin_sensitive_paths_cannot_be_dropped`, which asserts the built-ins survive
a configuration that tries to replace them.

**Read more.** [02](02-configuration.md) and
[04](04-redaction-and-secrets.md).

### 28. Nothing leaves your machine unless you name where it goes

**Rule.** Rift is local-first. There is no sync-by-default, no telemetry, no
phone-home, and no fallback destination. The shipper refuses to guess a target
from your git remotes; you name one or nothing is sent.

**Why.** A tool holding a complete record of your engineering work has to make
this unambiguous. The absent fallback is the sharp edge: inferring a destination
from an existing remote would be convenient exactly once and catastrophic the
first time a remote was not what the tool assumed. The sync formats are designed
so hosting later is a transport problem, not a format problem, but the transport
is not built and the local halves send nothing on their own.

**Enforced by** the explicit-destination requirement in the push queue, by rule
10's absence of any network client in the shipped binaries, and by the queue
being drained only by a command you run.

**Read more.** [14](14-sync-readiness.md).

---

## 17.6 The code

Nine rules about the workspace itself — the ones a change to Rift meets first.

### 29. No `unsafe`, no warnings

**Rule.** Every crate forbids `unsafe` code at the crate root. Clippy runs with
pedantic lints on and warnings denied. A crate-root blanket allow is not an
escape hatch — it is refused.

**Why.** The first two are ordinary hygiene for a tool handling other people's
secrets. The third matters most: a blanket allow silences a lint for everything
below it forever, including code nobody has written yet, and is invisible in
the diff that introduces the next problem.

**Enforced by** the crate-level attribute in every crate and binary, and by
three checks in the workspace metadata pass:
`unsafe_only_in_sanctioned_modules`, `no_blanket_allow_at_crate_root`, and
`lint_table_matches_baseline`, which locks the workspace lint table against
quiet edits.

**Read more.** [16](16-development.md).

### 30. A new canonical table brings its guard; coverage floors never move down

**Rule.** Adding a canonical table means adding its registry row and its
immutability guard in the same change. Coverage floors — a workspace-wide line
and branch floor, and a higher line floor for the security-critical crates — are
never lowered to make a change pass.

**Why.** Rule 1 is only as good as its weakest table, and the failure mode is a
table added in a hurry that nothing protects: the registry makes the guard
automatic and the property test makes forgetting it loud. The coverage half is the
same principle — a floor that moves when inconvenient is a preference, and the
higher-floor crates are those where an untested branch is a security bug.

**Enforced by** the registry-driven property tests from rule 1, and by the
coverage check, which reads the security-crate list and fails on any crate under
its floor.

**Read more.** [16](16-development.md) and
[13](13-storage-and-schema.md).

### 31. Tests inject the clock, the randomness and the identity

**Rule.** A test injects its time source, its random source and its identity
source rather than reading the real ones. No test reaches the network.

**Why.** A suite that reads the wall clock fails at midnight, in another
timezone, or on a slow machine, and each failure teaches the team to re-run
rather than look. Determinism also makes golden fixtures possible: the same
inputs must produce the same bytes.

**Enforced by** the injected source traits threaded through the constructors, by
the fixtures, and by rule 10's dependency checks for the network half. Note the
honest limit: this holds for **assertions**, not for every line of test code — a
few end-to-end tests use real elapsed time for poll deadlines, and there is no
single workspace-wide clock abstraction.

**Read more.** [16](16-development.md).

### 32. Every modal surface joins the parity fixtures

**Rule.** Anything the dashboard draws as a centred modal frame carries the
shared close affordance, and every act its hints teach is reachable both by key
and by pointer. A new modal surface joins the parity fixtures or the build reds.

**Why.** A modal that can be opened and not closed is a trap, and a hint
advertising a key nothing handles teaches the user that the interface lies. Both
are invisible in a screenshot and easy to introduce, so tests pin them.

**Enforced by** three tests in the dashboard crate:
`every_modal_surface_carries_the_close_affordance`,
`no_new_modal_surface_was_introduced` (which fails when a surface is added
without being classified), and `every_hint_key_is_handled_by_the_reducer`,
which feeds every hinted key to the reducer and asserts something happens.

**Read more.** [12](12-canopy-architecture.md).

### 33. A check that cannot fail says so

**Rule.** An instrument that publishes a verdict without being able to fail the
build states that in the same breath as the verdict.

**Why.** The regression comparison is the live example: at a 10% tolerance it
flags five arms out of five between two identical runs of the same binary.
Wired to fail it would red on machine weather and train everyone to ignore a
red light — the expensive failure, because the next red is real. Wired to
report *and say so* it stays useful, with the enforcing path behind a flag so a
better threshold can switch it on without a rewrite.

**Enforced by** the instrument's own output, which prints its report-only status
alongside its verdict, and by the artifact it writes recording the same fact.

**Read more.** [15](15-benchmarks.md).

### 34. A number without a tracked artifact is not a result

**Rule.** Every performance figure published in this repository comes from a
tracked artifact written by the harness under the pinned protocol — a quiet
machine, a release build, medians of repeated runs, counts printed alongside. A
number taken any other way is not a result and is not published.

**Why.** Two instruments once disagreed several-fold about the same tree and the
disagreement was never reconciled. After that, an unqualified number is evidence
about the machine and the moment rather than the code — and a written protocol
turns "it felt faster" into something another person can check.

**Enforced by** the artifact-consistency audit, which reds on a scenario with
no artifact, on an artifact whose subject predates the last change to the code
it measured, and on a report whose prose fence disagrees with its data. The
record's consistency is checked mechanically; performance is reported, per rule
30.

**Read more.** [15](15-benchmarks.md).

### 35. Deleted things stay deleted

**Rule.** A handful of paths were removed deliberately and are guarded against
returning: two empty scaffold crates for a search engine and a code graph, the
kill-switch file for the retired execution-routing layer, and the retired
serving transport's entry point and its manifest.

**Why.** Each was reintroduced at least once by an editor, a merge or a
well-meant tidy-up, and an empty scaffold crate is easy to re-add and hard to
notice. What was reversed there was a *semantic* index of your code, and it
stays reversed. File search later shipped, and it is not that thing coming back
under guard: it holds path names and modification times, it lives in a
differently named crate carrying no line of either scaffold, and the guarded
paths are still guarded — which is exactly the distinction this rule exists to
keep legible.

**Enforced by** `deleted_paths_stay_deleted`, which matches the ignore-file
entries byte for byte against its own list, so removing the guard from either
side reds.

**Read more.** [16](16-development.md), and
[18](18-known-gaps.md) for what is genuinely deferred as opposed
to reversed.

### 36. Nothing stages the whole tree

**Rule.** No script, task or automation in this repository runs a
stage-everything git command. Paths are staged explicitly.

**Why.** Two independent reasons, either sufficient. The workspace membership
check walks the filesystem, so an un-membered directory under the crates tree
reds the build — and a blanket stage is how one arrives. And on a synced
filesystem a blanket stage hangs on evicted files, looking like a frozen
terminal rather than an error.

**Enforced by** the membership check for the first reason; the second is a
**working rule**, documented in the development chapter's hazards section rather
than guarded.

**Read more.** [16](16-development.md).

### 37. A claim names which kind of evidence it has

**Rule.** Every claim in this repository's documentation and reports
distinguishes four things: verified locally, observed in operation, deliberately
accepted and left open, and not run at all. The fourth is stated, not omitted.

**Why.** This is the rule the other thirty-six lean on. A document that reads
uniformly confident cannot be audited, because nothing says which sentences are
load-bearing. Naming the weak ones is what makes the strong ones worth anything
— it is why savings figures are labelled estimates, why an unmeasured arm is
written down as unmeasured, and why [18](18-known-gaps.md) is a
chapter rather than an apology.

**Enforced by** discipline and review, with two mechanical supports: the artifact
audit from rule 34, and the served-figure honesty ladder in
[08](08-cost-and-savings.md), which degrades a headline
rather than presenting a partial total as a complete one.

**Read more.** [18](18-known-gaps.md) and
[15](15-benchmarks.md).

---

## 17.7 What this register is not

**It is not the security model.** [../../SECURITY.md](../../SECURITY.md) is, and
is the authority where the two touch; several rules above are consequences of it.
**It is not a list of everything Rift does** — only of what it must not stop
doing, since plenty of good behaviour is absent here because breaking it would be
a bug rather than a broken promise. **It is not permanent by decree**: rules
change, and one changed while this guide was being written. What is not allowed
is changing one *quietly* — a relaxed rule gets its entry rewritten with the new
reasoning, and a dropped one leaves a note saying what replaced it, so a guard
removed to make a change pass cannot later read as an accident.

> Tip: If you are about to weaken one of these, the useful question is not "can
> I" but "what did the person who wrote this know that I do not". Most entries
> are shorter than the argument that produced them, and the Why field is the
> compressed version of that argument.
