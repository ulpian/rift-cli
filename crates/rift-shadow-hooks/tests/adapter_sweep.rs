//! The **adapter completeness sweep**, as an executable table.
//!
//! Nine adapters, checked against each agent's *current* hook surface: which
//! verbs it really publishes, what its payload calls the session, where the
//! model and the token counts actually live, how a subagent is signalled, and
//! where a compaction boundary falls. Every finding below was verified against
//! a real payload schema, not inferred from a plausible name — and the ones
//! that could **not** be verified say so and pin nothing, because a shape
//! invented for an unverifiable schema is the failure this program has already
//! paid for once.
//!
//! It is a test rather than a document for the reason the redaction parity
//! table is: a document describing capture drifts away from the capture, and
//! nothing fails when it does. Here [`OPEN_GAPS`] is pinned, so closing a gap
//! forces the row to be edited in the same commit, and the golden event streams
//! at the bottom fail the moment an adapter's session *shape* changes — which
//! is the class of regression no per-verb assertion can see.
//!
//! **How to change it.** Fixing a gap: change the adapter, flip the row's
//! verdict, decrement `OPEN_GAPS`, and say what you verified in `evidence`.
//! Making an agent supported (adding it to `ALPHA_AGENTS`): its rows in
//! [`UNSUPPORTED_ADAPTER_ROWS`] stop being true in the same commit, and
//! [`every_unsupported_adapter_row_names_an_agent_the_product_refuses_to_wire`]
//! reddens until they are dealt with.
//! Adding an agent: add its rows and its stream; the coverage test refuses a
//! registered adapter with no rows. Either way [`SWEEP_ROWS`] and
//! [`UNVERIFIABLE_ROWS`] move with the table, and every source you cite must
//! sit under a root a reviewer can fetch ([`CITATION_ROOTS`]) — sources from
//! the pinned tag use the token-free `upstream: …` citation namespace, and the bare
//! `agent/…` shorthand this table used at first is a 404 there.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use rift_agent::conformance::{check_stream, Stream};
use rift_core::EventKind::{
    FileEdit, PromptResponse, PromptSubmit, SessionCompaction, SessionEnd, SessionStart, ToolCall,
    ToolResult, TurnEnd, TurnStart,
};
use rift_shadow_hooks::AdapterRegistry;

/// What the sweep concluded about one behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// Rift captures what the agent publishes.
    AtParity,
    /// Rift captures more, or more safely, than the retired reference
    /// implementation did.
    Stronger,
    /// A deliberate difference from the retired reference implementation, in
    /// Rift's favour or neutral, that a reader would otherwise mistake for a bug.
    Divergence,
    /// Something the agent publishes that Rift does not yet capture. Each one
    /// names the work that closes it — or, for the four rows in
    /// [`UNSUPPORTED_ADAPTER_ROWS`], says plainly that this alpha does not
    /// capture that agent at all and no work is planned. `OPEN_GAPS` counts
    /// both: an unsupported agent's gap is withdrawn as a promise, not fixed.
    Gap,
    /// Not verifiable from the sources available here. Nothing is
    /// pinned for these — deliberately.
    Unverifiable,
}

/// One row of the sweep.
struct Row {
    /// `<agent>.<behaviour>`; the prefix must name a registered adapter (or
    /// `all`, for a property every adapter shares).
    id: &'static str,
    verdict: Verdict,
    /// What is true today.
    finding: &'static str,
    /// Where it was verified, or why it could not be.
    evidence: &'static str,
}

/// Gap rows open right now. Pinned so a gap cannot be closed silently, and so
/// "no `Gap` row remains" can be read as a number.
///
/// **Re-derived on 2026-08-20 when four adapters became unsupported-this-alpha,
/// and it did not move.** The obvious next step — dropping them out of this
/// count — is the one the decision forbids: *a gap is not fixed by declaring
/// the agent unsupported*. Cursor
/// still sends seven camelCase events Rift's table does not name; that is as
/// true behind a refusal as it was in front of one. What changed is the
/// sentence beside the verdict, not the verdict.
///
/// **Re-derived again on 2026-08-24 when pinned surfaces landed: 17 → 15**, and both
/// closures are corrections rather than work — the two rows were **false of the
/// agents they described**, each graded against the Go wrapper's view instead
/// of the agent's own published surface:
///
/// * `claude-code.subagent_boundary` — Claude Code publishes `SubagentStart`;
/// * `codex.verbs_beyond_the_surface` — Codex publishes eleven events, not five.
///
/// Neither could have been found by re-reading this table, which is the point
/// of the rule: a supported agent's own surface is now pinned into the tree
/// (see [`PINNED_SURFACES`]) and the tables are measured against it.
const OPEN_GAPS: usize = 15;

/// The rows whose subject is an agent this alpha does not capture at all.
///
/// These stay `Gap` — see [`OPEN_GAPS`]. What this list adds is the binding
/// the prose alone cannot make: a row may only *describe* an agent as
/// not-yet-supported if the product actually refuses to wire it. Without it,
/// adding an adapter to [`ALPHA_AGENTS`] would leave four rows telling a
/// reader that Rift declines to install something it now installs.
/// **ORDER IS TABLE ORDER, and the two Gemini rows insert in the MIDDLE.** The
/// assertion at the bottom of
/// [`every_unsupported_adapter_row_names_an_agent_the_product_refuses_to_wire`]
/// compares this list against the marked rows *in the order the table declares
/// them*, so appending Gemini after `factory-droid.verb_surface` — the obvious
/// edit — reds. Gemini's rows sit above Cursor's in `SWEEP`, so they sit above
/// Cursor's here.
const UNSUPPORTED_ADAPTER_ROWS: &[&str] = &[
    "gemini.model_fact_is_dropped",
    "gemini.missing_verbs",
    "cursor.verb_surface",
    "copilot-cli.verb_surface",
    "opencode.verb_surface",
    "factory-droid.verb_surface",
];

/// The phrase [`UNSUPPORTED_ADAPTER_ROWS`] rows carry, so the marker is a
/// token a test can find rather than a turn of phrase.
const UNSUPPORTED_MARKER: &str = "UNSUPPORTED-ADAPTER FACT";

/// Rows in the table, and how many of them concede they could not be verified.
/// Pinned for the same reason `OPEN_GAPS` is: the previous cycle's commit
/// message, design note and ledger line all said "36 rows, 3 unverifiable" while
/// the table held 39 and 4. A number nobody can check is a number that drifts,
/// so the table now states its own size and a test proves it.
const SWEEP_ROWS: usize = 44;
/// See [`SWEEP_ROWS`]. Down one this cycle: `copilot-cli.session_key` was never
/// unverifiable — the tag stated the answer and the row did not look.
const UNVERIFIABLE_ROWS: usize = 3;

/// Where a cited source may live: the pinned tag's agent packages, or Rift's
/// own tree.
///
/// Evidence is worth exactly as much as a reader's ability to fetch it. This
/// table's first draft cited `agent/<pkg>/<file>.go`; at the tag that path is a
/// **404**. The token-free `upstream: …` namespace denotes paths rooted at the
/// pinned source tree, and an uncheckable citation
/// is precisely how `copilot-cli.session_key` spent a whole cycle recorded as
/// "unverifiable" while `copilotcli/compat.go:74` stated the answer outright.
const CITATION_ROOTS: &[&str] = &["upstream: ", "crates/", "bins/"];

/// Sources cited from neither tree, named one by one so "elsewhere on purpose"
/// can never hide a typo.
///
/// **Every entry here now owes a committed pin** — see [`PINNED_SURFACES`].
const NON_TAG_SOURCES: &[&str] = &[
    "codex-rs/hooks/src/schema.rs",
    "src/claude_agent_sdk/types.py",
];

/// A source outside the fetchable roots, and the artifact in **this** tree that
/// pins it .
///
/// The rule makes a supported agent's own published hook surface the
/// authority for its verb table, and admits that authority **only once the
/// surface is pinned into the tree as a committed fixture** carrying
/// repository, tag, path, blob sha and a verbatim excerpt. [`NON_TAG_SOURCES`]
/// on its own is a spelling allowlist: it stops a typo, and stops nothing else.
/// A reviewer holding only this repository still could not check the claim.
///
/// So the two lists are joined. An entry may sit in `NON_TAG_SOURCES` only if a
/// fixture here quotes it, and the fixture must name the upstream path it
/// quotes — which is what makes the citation resolve for a reader who never
/// leaves the tree.
const PINNED_SURFACES: &[(&str, &str)] = &[
    (
        "codex-rs/hooks/src/schema.rs",
        "crates/rift-agent-codex/fixtures/codex-hook-surface.rs",
    ),
    (
        "src/claude_agent_sdk/types.py",
        "crates/rift-agent-claude-code/fixtures/claude-code-hook-surface.md",
    ),
];

/// The phrase a row uses when the search that came back empty was the whole
/// tag, not one file. It counts as a searched source in
/// [`every_unverifiable_row_names_the_sources_it_searched`] because "I looked
/// everywhere" is a stronger claim than any single path — and therefore has to
/// be made explicitly, not implied by silence.
const TREE_WIDE_SEARCH: &str = "a tree-wide search of the pinned tag";

/// Every source file cited in `text`: tokens containing one of
/// [`SOURCE_EXTENSIONS`], cut at the extension so a trailing `:74` or `,` does
/// not hide the path.
///
/// **Bare filenames count.** An earlier draft dropped any token without a `/`,
/// which meant a citation written as `lifecycle.go:17` was silently exempt from
/// the root check — the one shape most likely to be wrong, because a filename
/// with no directory is exactly what a reader cannot fetch. Every source-looking
/// token now has to name a path under [`CITATION_ROOTS`] or be declared in
/// [`NON_TAG_SOURCES`].
fn cited_sources(text: &str) -> Vec<String> {
    let mut sources = Vec::new();
    let mut tokens = text.split_whitespace();
    while let Some(token) = tokens.next() {
        let token = token.trim_start_matches(['(', '`', '"', '[', ',']);
        let (prefix, candidate) = if token == "upstream:" {
            ("upstream: ", tokens.next().unwrap_or_default())
        } else {
            ("", token)
        };
        let candidate = candidate.trim_start_matches(['(', '`', '"', '[', ',']);
        let Some((end, ext)) = SOURCE_EXTENSIONS
            .iter()
            .filter_map(|ext| Some((candidate.find(ext)?, *ext)))
            .min_by_key(|(at, _)| *at)
        else {
            continue;
        };
        sources.push(format!("{prefix}{}", &candidate[..end + ext.len()]));
    }
    sources
}

/// The extensions [`cited_sources`] recognises.
///
/// `.py` joins `.go`/`.rs` with this cycle, and the reason is worth stating
/// because the omission was load-bearing: Claude Code's own published hook
/// surface is a Python type union in Anthropic's Agent SDK, and a checker that
/// only knew about Go and Rust would have let that citation through unbound —
/// which is the exact hole `codex-rs/hooks/src/schema.rs` sat in while a Gap
/// row graded Codex's verb table against a third party's `HookNames()`. The set
/// stays deliberately narrow: `.md` is NOT here, because rows legitimately name
/// documents an agent *reads* (`AGENT.md`) rather than sources this table
/// cites, and conflating the two would demand a pin for a subject.
const SOURCE_EXTENSIONS: &[&str] = &[".go", ".rs", ".py"];

const SWEEP: &[Row] = &[
    // ---------------------------------------------------------------- global
    Row {
        id: "all.replay_safety",
        verdict: Verdict::Stronger,
        finding: "Every adapter is proven to map a re-delivered hook identically, \
                  and Rift's stamping keeps one hook's fan-out keyed distinctly, \
                  so redelivery is a true no-op rather than a doubled turn.",
        evidence: "rift_agent::conformance::check_replay_safe (per adapter) \
                   + every_registered_adapter_is_replay_safe_through_dispatch (registry-wide, \
                   over the real dispatch path). The retired reference implementation \
                   had no analogue.",
    },
    Row {
        id: "all.unknown_verb_is_countable",
        verdict: Verdict::Stronger,
        finding: "A verb an adapter does not know is recorded as a \
                  `skipped-unknown-verb` receipt, never dropped. A new agent \
                  release that adds a hook is therefore visible as a count \
                  before anyone notices missing capture.",
        evidence: "rift_shadow::bridge::HookReceipt; the retired reference \
                   implementation returned a nil event for an unknown hook and wrote \
                   nothing.",
    },
    Row {
        id: "all.session_less_hook_is_countable",
        verdict: Verdict::Stronger,
        finding: "A hook that names no session leaves a `deferred-no-session` \
                  receipt instead of nothing at all, so a capture loss is a row \
                  rather than an absence.",
        evidence: "Added this cycle. It is the instrument that makes the \
                   Cursor row below observable in production, and — now that \
                   Copilot's session key is bound — the one that keeps a refused \
                   subagent lifecycle hook countable rather than merely dropped \
                   (copilot-cli.subagent_session_prefix).",
    },
    // ----------------------------------------------------------- claude-code
    Row {
        id: "claude-code.session_key",
        verdict: Verdict::AtParity,
        finding: "`session_id`, bound by the wire's native fallback.",
        evidence: "Claude Code's own hook stdin; pinned by \
                   wire::every_verified_agent_session_spelling_binds_its_own_id.",
    },
    Row {
        id: "claude-code.file_mutating_tools",
        verdict: Verdict::AtParity,
        finding: "Write, Edit, MultiEdit, NotebookEdit, mcp__acp__Write, mcp__acp__Edit \
                  all fan out to a FileEdit beside the ToolCall.",
        evidence: "Closed this cycle; the last three were missing, so notebook \
                   edits and every ACP-client edit recorded no working-tree change.",
    },
    Row {
        id: "claude-code.subagent_boundary",
        verdict: Verdict::AtParity,
        finding: "`SubagentStart`/`SubagentStop` are declared, registered and \
                  mapped, and the child id they carry (`agent_id`) becomes \
                  HookFacts::child_session, so a subagent's work nests under \
                  the parent turn instead of being attributed flat to it.",
        evidence: "CLOSED 2026-08-24 against Anthropic's OWN published surface, \
                   pinned at crates/rift-agent-claude-code/fixtures/ (Agent SDK \
                   src/claude_agent_sdk/types.py, blob \
                   308b76cb7fd928d124666c255b253c92c343f15d). This row used to \
                   read *Rift declares SubagentStart, which Claude Code does not \
                   publish* — cited to the Go wrapper, and FALSE of the agent: \
                   the SDK's HookEvent union lists SubagentStart outright. What \
                   WAS true is the second half, for a field the row named wrong: \
                   the child id is `agent_id`, required on SubagentStartHookInput \
                   and SubagentStopHookInput, not `tool_response.agentId` on \
                   PostToolUse[Agent]; the default facts reader knew three \
                   spellings and none of them was it. Both halves are now tests: \
                   pinned_surface::every_verb_rift_declares_for_claude_code_is_one_claude_code_publishes \
                   and tests::the_subagent_id_on_the_wire_becomes_the_child_session.",
    },
    Row {
        id: "claude-code.compaction",
        verdict: Verdict::Stronger,
        finding: "`PreCompact` maps to SessionCompaction, so a compaction \
                  boundary is a durable event.",
        evidence: "The retired reference implementation installed no compaction hook \
                   for Claude Code at all (six hook types in its settings writer, none \
                   of them PreCompact), so its checkpoints could not see one.",
    },
    Row {
        id: "claude-code.turn_tokens",
        verdict: Verdict::AtParity,
        finding: "Provider counts from the hook payload's `usage` object, plus a \
                  transcript-tail reader that sums a turn back to its prompt.",
        evidence: "Same four Anthropic field names the retired reference \
                   implementation read from the transcript (`input_tokens`, \
                   `output_tokens`, `cache_read_input_tokens`, \
                   `cache_creation_input_tokens`).",
    },
    Row {
        id: "claude-code.stop_sentinel",
        verdict: Verdict::Divergence,
        finding: "The stop-hook sentinel is structurally correct but unreachable \
                  during Rift's own wait, so a turn end settles on the quiet \
                  window. Kept deliberately, documented at the const.",
        evidence: "The remediation: the marker record carries each hook's own \
                   durationMs, so it provably post-dates the hook.",
    },
    // ------------------------------------------------------------------ codex
    Row {
        id: "codex.session_key",
        verdict: Verdict::AtParity,
        finding: "`session_id`.",
        evidence: "The `session_id` json tag on all four Codex hook payload \
                   structs in upstream: codex/types.go at the pinned tag.",
    },
    Row {
        id: "codex.file_mutating_tools",
        verdict: Verdict::AtParity,
        finding: "`apply_patch` (canonical) plus the `Write`/`Edit` aliases.",
        evidence: "Closed this cycle. `apply_patch` was absent, so the tool Codex \
                   actually edits with recorded no working-tree change; the tag \
                   names it canonical and Write/Edit only compatibility aliases.",
    },
    Row {
        id: "codex.model",
        verdict: Verdict::AtParity,
        finding: "A top-level `model` string on every Codex hook payload.",
        evidence: "upstream: codex/types.go: `model` on sessionStartRaw, \
                   userPromptSubmitRaw, postToolUseRaw and stopRaw alike.",
    },
    Row {
        id: "codex.verbs_beyond_the_surface",
        verdict: Verdict::AtParity,
        finding: "There are none. Codex publishes eleven hook events and Rift \
                  declares ten of them; the eleventh is deliberate and has its \
                  own row (codex.permission_request_undeclared).",
        evidence: "CLOSED 2026-08-24 against Codex's OWN published schema, pinned \
                   at crates/rift-agent-codex/fixtures/ (codex-rs/hooks/src/schema.rs \
                   at tag rust-v0.149.1, blob 3847cc91818a9a141c066e1fc6d734adb60f9bde). \
                   This row used to read *Rift declares nine verbs; Codex \
                   publishes five … PreCompact, PostCompact, SubagentStart, \
                   SubagentStop can never fire*, cited to a third party's Go \
                   wrapper and its HookNames() rather than to OpenAI. That \
                   sentence was true of that wrapper and FALSE of the agent: \
                   the pinned schema generates one command-input file per \
                   published event and all four are among them. Grading a \
                   supported agent's table against a third party's view of it is \
                   the defect the pinned-surface rule exists to stop. Pinned by \
                   rift_agent_codex::pinned_surface (both directions).",
    },
    Row {
        id: "codex.session_end_unregistered",
        verdict: Verdict::Stronger,
        finding: "`SessionEnd` maps to the canonical SessionEnd, so a Codex \
                  session has a durable end.",
        evidence: "OPENED AND CLOSED IN THE SAME COMMIT, 2026-08-24, and it is \
                   only visible because the surface was pinned: Codex publishes \
                   SessionEndCommandInput, Rift declared no SessionEnd verb, and \
                   the adapter's own compat_notes asserted *no explicit \
                   session-end event* — a note copied from a wrapper that never \
                   registered the hook. The retired reference implementation had no \
                   analogue: its own HookNames() ended at five and never saw a \
                   session close.",
    },
    Row {
        id: "codex.permission_request_undeclared",
        verdict: Verdict::Divergence,
        finding: "Codex publishes `PermissionRequest` and Rift does not register \
                  it. Every fact it carries — session_id, turn_id, model, \
                  permission_mode, tool_name, tool_input — is already on \
                  PreToolUse, which Rift does register, and it names a tool that \
                  may then be denied.",
        evidence: "DELIBERATE, not missed: mapped to ToolCall it double-counts \
                   (its input carries no tool_use_id, so the dedup key cannot \
                   see the pair); mapped to nothing it installs a hook on \
                   Codex's approval path that records nothing. Recording the \
                   decision honestly needs a canonical permission event, which \
                   is a schema change and reserved to the owner. Nothing is \
                   lost by not registering: Codex calls only the hooks named in \
                   .codex/hooks.json. Enumerated and pinned by \
                   rift_agent_codex::pinned_surface's \
                   the_codex_events_rift_does_not_declare_are_enumerated_with_their_reason.",
    },
    Row {
        id: "codex.turn_id_unread",
        verdict: Verdict::Gap,
        finding: "Codex states the turn on the wire and Rift derives an ordinal \
                  from the event stream instead, so a hook lost mid-turn \
                  silently shifts every later turn's ordinal. WIDER THAN THIS \
                  ROW SAID: `turn_id` is not two payloads, it is a required \
                  field on EVERY turn-scoped Codex hook input.",
        evidence: "Re-measured 2026-08-24 against the pinned schema \
                   (codex-rs/hooks/src/schema.rs, blob \
                   3847cc91818a9a141c066e1fc6d734adb60f9bde): PreToolUse, \
                   PermissionRequest, PostToolUse, PreCompact, PostCompact, \
                   UserPromptSubmit, SubagentStart, SubagentStop and Stop all \
                   carry `pub turn_id: String`, each above the same upstream \
                   comment *Codex extension: expose the active turn id to \
                   internal turn-scoped hooks*. The row previously cited only \
                   userPromptSubmitRaw and stopRaw, from the Go wrapper's \
                   partial view. NOT CLOSED, and the reason is an ownership \
                   boundary rather than a missing fact: the adapter half is \
                   where a fact \
                   is read, but the ordinal is advanced in \
                   crates/rift-shadow/src/bridge/turns.rs and the reconciliation map \
                   is written from there — a file held elsewhere at the \
                   time of this edit. Recorded, not deferred silently.",
    },
    Row {
        id: "codex.usage_unverifiable",
        verdict: Verdict::Unverifiable,
        finding: "The adapter reads `usage.{input_tokens,output_tokens,\
                  cached_input_tokens}` and declares `surfaces_token_usage: true`, \
                  but no Codex hook payload in the pinned tag carries a `usage` \
                  object at all. The reader is harmless (absent stays absent) — \
                  the *claim* is what is unsupported.",
        evidence: "SEARCHED at the pinned tag: \
                   upstream: codex/types.go (all four hook payload \
                   structs, token-free), upstream: codex/hooks.go and \
                   upstream: codex/lifecycle.go (no usage read on any \
                   hook path). Where the tag DOES count Codex tokens it is not a \
                   hook: upstream: codex/reviewer.go:98 and \
                   upstream: codex/review_tokens.go:35 parse `usage` \
                   and `token_count.total_token_usage` out of the CLI's own \
                   `--json` stdout and rollout file. The tag's own schema \
                   citation, codex-rs/hooks/src/schema.rs, lives in another \
                   repository and is outside the sources available here. \
                   Nothing pinned either way.",
    },
    // ----------------------------------------------------------------- gemini
    Row {
        id: "gemini.session_key",
        verdict: Verdict::AtParity,
        finding: "`session_id`.",
        evidence: "upstream: geminicli/types.go: `session_id` on sessionInfoRaw, \
                   agentHookInputRaw and beforeModelRaw.",
    },
    Row {
        id: "gemini.compaction",
        verdict: Verdict::AtParity,
        finding: "`PreCompress` maps to SessionCompaction.",
        evidence: "upstream: geminicli/lifecycle.go maps PreCompress to its \
                   Compaction event over the same payload shape.",
    },
    Row {
        id: "gemini.file_mutating_tools",
        verdict: Verdict::AtParity,
        finding: "write_file, edit_file, save_file, replace.",
        evidence: "Closed this cycle; edit_file and save_file were missing, so \
                   two of the four ways Gemini writes a file recorded no change.",
    },
    Row {
        id: "gemini.model_fact_is_dropped",
        verdict: Verdict::Gap,
        finding: "Gemini states its model in exactly one place — \
                  `llm_request.model` on BeforeModel — and Rift maps BeforeModel \
                  to *zero* canonical events. Payload facts are projected per \
                  event, so with no event there is no session row, no model \
                  observation, and no record of which model ran. Every Gemini \
                  session is captured model-less.",
        evidence: "Adapter maps BeforeModel to an empty vec; \
                   rift_shadow::project::project_event is the only caller of \
                   record_model_observation and runs once per event. Confirmed \
                   by the golden stream below, which is deliberately written to \
                   show the empty step. UNSUPPORTED-ADAPTER FACT: this alpha \
                   does not capture Gemini CLI at all — `enable` refuses it \
                   before writing a descriptor. THE \
                   REASON IS NOT THIS DEFECT. Gemini is out of scope by owner \
                   ruling, and this gap is an ordinary one that would be fixed \
                   if it were in scope; the four rows above it are unsupported \
                   because the agents do not send what Rift declares, which is a \
                   different sentence. The gap is NOT closed: a Gemini session \
                   is still captured model-less, and it stays counted. What is \
                   withdrawn is the promise to fix it in this alpha.",
    },
    Row {
        id: "gemini.missing_verbs",
        verdict: Verdict::Gap,
        finding: "Gemini publishes eleven hook events; Rift declares nine. \
                  `BeforeToolSelection` and `Notification` are undeclared, so \
                  each lands as a skipped-unknown-verb receipt.",
        evidence: "upstream: geminicli/{types,lifecycle}.go: GeminiHooks has eleven \
                   keys and HookNames() returns eleven. Visible, not silent \
                   (see all.unknown_verb_is_countable), which is why this is a \
                   gap and not an incident. UNSUPPORTED-ADAPTER FACT: this \
                   alpha does not capture Gemini CLI at all — `enable` refuses \
                   it before writing a descriptor. THE REASON IS NOT THIS \
                   DEFECT: Gemini is out of scope by an owner scope decision, \
                   not by a measurement of its surface. Re-worded, \
                   never closed — the two undeclared verbs are as real as they \
                   were and stay counted.",
    },
    Row {
        id: "gemini.usage_shape_unverified",
        verdict: Verdict::Unverifiable,
        finding: "The adapter reads `llm_response.usageMetadata.*` on AfterModel. \
                  The pinned tag acknowledges AfterModel without ever parsing its \
                  payload, so the wrapper name is not verifiable; and where the \
                  retired reference implementation *did* count Gemini tokens it used \
                  a different shape fully — the transcript's \
                  `messages[].tokens.{input,output,cached}` on `gemini` messages.",
        evidence: "SEARCHED at the pinned tag: \
                   upstream: geminicli/types.go (`llm_request` at :73 \
                   is the only llm_* payload the tag ever parses; there is no \
                   `llm_response` anywhere in it), \
                   upstream: geminicli/hooks.go (AfterModel appears \
                   only as an installer/matcher key) and \
                   upstream: geminicli/lifecycle.go:93 (AfterModel is \
                   grouped with the hooks it acknowledges and does not parse), \
                   plus a tree-wide search of the pinned tag for `usageMetadata` \
                   — zero hits. What the tag counts instead is the transcript's \
                   own shape: CalculateTokenUsage at \
                   upstream: geminicli/lifecycle.go:111 over \
                   geminiMessageTokens at \
                   upstream: geminicli/types.go:97. Nothing pinned: \
                   the reader stays (an absent field yields no count) but no \
                   test asserts the shape.",
    },
    // ----------------------------------------------------------------- cursor
    Row {
        id: "cursor.session_key",
        verdict: Verdict::AtParity,
        finding: "`conversation_id` — bound as of this cycle.",
        evidence: "upstream: cursor/types.go: `conversation_id` on all seven hook \
                   payload structs. Before this cycle the wire knew only \
                   `session_id`/`conversationId`, so every Cursor hook was \
                   captured under no session at all.",
    },
    Row {
        id: "cursor.verb_surface",
        verdict: Verdict::Gap,
        finding: "Rift declares three snake_case verbs (session_start, edit, \
                  session_end) that Cursor does not publish. Cursor's real \
                  surface is seven camelCase events: sessionStart, sessionEnd, \
                  beforeSubmitPrompt, stop, preCompact, subagentStart, \
                  subagentStop. Nothing Cursor sends currently maps.",
        evidence: "upstream: cursor/types.go CursorHooks (the .cursor/hooks.json \
                   schema, version 1). UNSUPPORTED-ADAPTER FACT: this alpha \
                   does not capture Cursor at all — \
                   `enable` refuses it before writing a descriptor, so the \
                   mismatch is dead configuration rather than a wiring Rift \
                   advertises and does not honour. The gap is NOT closed: it is \
                   as real as it was, and it stays counted. What is withdrawn is \
                   the promise to rewrite this table, which the ruling retired.",
    },
    Row {
        id: "cursor.stop_tokens",
        verdict: Verdict::Gap,
        finding: "Cursor's stop hook carries `input_tokens`, `output_tokens`, \
                  `cache_read_tokens` and `cache_write_tokens` per turn, and the \
                  adapter declares `surfaces_token_usage: false`. Cursor is the \
                  one agent whose tokens exist *only* on a hook.",
        evidence: "upstream: cursor/types.go stopHookInputRaw, whose own comment \
                   states these are \"the only authoritative source of token \
                   accounting — the JSONL transcript does not include usage data\".",
    },
    Row {
        id: "cursor.subagent_hooks",
        verdict: Verdict::Gap,
        finding: "Cursor publishes a fully-formed subagent pair \
                  (subagentStart/subagentStop with subagent_id, subagent_type, \
                  subagent_model, parent_conversation_id, and the child's own \
                  agent_transcript_path). None of it is mapped, so parallel \
                  workers' spend folds into the parent with no nesting.",
        evidence: "upstream: cursor/types.go subagentStartHookInputRaw + \
                   subagentStopHookInputRaw.",
    },
    Row {
        id: "cursor.precompact_context",
        verdict: Verdict::Gap,
        finding: "Cursor's preCompact hook states the context window directly \
                  (context_tokens, context_window_size, context_usage_percent, \
                  messages_to_compact, is_first_compaction) — a measured \
                  context-pressure fact no other agent gives us, unmapped.",
        evidence: "upstream: cursor/types.go preCompactHookInputRaw.",
    },
    // ---------------------------------------------------------------- copilot
    Row {
        id: "copilot-cli.session_key",
        verdict: Verdict::AtParity,
        finding: "`sessionId`, with `session_id` as the VS Code host's alternate \
                  spelling. Both are bound by the wire as of this cycle; before \
                  it, every Copilot hook landed session-less.",
        evidence: "upstream: copilotcli/compat.go:74 builds the hook \
                   envelope with `SessionID: firstString(raw, \"sessionId\", \
                   \"session_id\")` (firstString at :116), and that package's \
                   AGENT.md carries captured payloads with `sessionId` on all \
                   five hooks plus \"Session ID extraction: sessionId field in \
                   hook payloads (UUID format)\". CORRECTION: the previous cycle \
                   filed this as Unverifiable and shipped Copilot session-less \
                   on that premise. The row was wrong, not the tag — the \
                   spelling was stated in the sources available here all \
                   along, at a path (upstream: …) the row cited in a \
                   shorthand that does not resolve.",
    },
    Row {
        id: "copilot-cli.verb_surface",
        verdict: Verdict::Gap,
        finding: "Rift declares four verbs (session_start, prompt, tool, \
                  session_end). Copilot publishes eight camelCase events: \
                  userPromptSubmitted, sessionStart, agentStop, sessionEnd, \
                  subagentStop, preToolUse, postToolUse, errorOccurred. Binding \
                  the session (this cycle) did not by itself buy capture: every \
                  Copilot hook now lands under the RIGHT session id and is still \
                  a skipped-unknown-verb receipt, because none of the four names \
                  is one Copilot sends.",
        evidence: "upstream: copilotcli/types.go CopilotHooks \
                   (.github/hooks/*.json, version 1) + HookNames() at \
                   upstream: copilotcli/lifecycle.go:42. \
                   UNSUPPORTED-ADAPTER FACT: this \
                   alpha does not capture Copilot CLI at all — `enable` refuses \
                   it before writing a descriptor. The gap stays open and stays \
                   counted; only the promise to rewrite this table is withdrawn.",
    },
    Row {
        id: "copilot-cli.session_shutdown_totals",
        verdict: Verdict::Gap,
        finding: "Copilot's authoritative token totals are a transcript event, \
                  not a hook payload: `session.shutdown` carries \
                  `data.modelMetrics[<model>].usage.{inputTokens,outputTokens,\
                  cacheReadTokens,cacheWriteTokens}` plus a per-model request \
                  count. Rift reads none of it and declares no usage.",
        evidence: "upstream: copilotcli/transcript.go sessionShutdownData/modelMetric. \
                   Note the aggregate is session-scoped: the retired reference \
                   implementation fell back to summing assistant.message outputTokens \
                   for a checkpoint slice, because session.shutdown would overcount.",
    },
    Row {
        id: "copilot-cli.subagent_session_prefix",
        verdict: Verdict::Stronger,
        finding: "Copilot fires the *session* lifecycle hooks for subagent turns \
                  too, reusing the subagent's tool-use id (a `toolu_` prefix) as \
                  the session id. The wire refuses to bind one on either shape, \
                  so a subagent turn leaves a countable receipt instead of an \
                  ownership marker and a cache file for a session that never \
                  receives an end.",
        evidence: "upstream: copilotcli/lifecycle.go:17 \
                   (subagentSessionIDPrefix) + the guard at :92, whose comment \
                   describes exactly this failure in the retired reference \
                   implementation. Stronger twice over: the refusal is agent-wide \
                   (a tool-use id names no \
                   session for anyone, so the next agent to reuse one inherits \
                   the guard, not the bug) and it leaves a deferred-no-session \
                   row where the retired reference implementation dropped the hook \
                   to a debug log. Closed in the same commit as \
                   copilot-cli.session_key: binding \
                   the spelling without this refusal would have traded one \
                   visible loss for an invisible leak.",
    },
    Row {
        id: "copilot-cli.subagent_work_uncaptured",
        verdict: Verdict::Gap,
        finding: "Refusing the phantom id keeps the session ledger clean but \
                  captures nothing of the subagent turn itself: Copilot \
                  publishes `subagentStop` and Rift declares no verb for it, so \
                  a subagent's work is neither nested under its parent nor \
                  attributed anywhere. The retired reference implementation kept its \
                  own subagentStop running for exactly this reason — it was the one \
                  lifecycle hook it did not drop on a toolu_ session id.",
        evidence: "upstream: copilotcli/lifecycle.go:92 \
                   (`hookName != HookNameSubagentStop && \
                   isSubagentSessionID(env.SessionID)`) and the subagentStop row \
                   of that package's AGENT.md. The remedy this row used to name \
                   as closing it — copilot-cli.verb_surface — is an \
                   unsupported-adapter fact in this alpha, so nothing \
                   closes this in the alpha. The design is kept because it is \
                   the answer whenever Copilot is supported: map subagentStop to \
                   a SubagentStart/End pair addressed to the PARENT session, and \
                   exempt that one verb from the tool-use-id refusal so the \
                   child id arrives as a child handle, never as a session.",
    },
    // --------------------------------------------------------------- opencode
    Row {
        id: "opencode.session_key",
        verdict: Verdict::AtParity,
        finding: "`session_id`.",
        evidence: "upstream: opencode/types.go sessionInfoRaw/turnStartRaw/turnEndRaw.",
    },
    Row {
        id: "opencode.verb_surface",
        verdict: Verdict::Gap,
        finding: "Rift declares session.start, message, tool.execute, session.end. \
                  OpenCode's plugin fires session-start, session-end, turn-start, \
                  turn-end and compaction — so Rift has no turn end and maps \
                  nothing to the compaction event OpenCode does publish.",
        evidence: "upstream: opencode/lifecycle.go HookNames() + ParseHookEvent, which \
                   maps `compaction` to a Compaction event. \
                   UNSUPPORTED-ADAPTER FACT: this \
                   alpha does not capture OpenCode at all — `enable` refuses it \
                   before writing a descriptor. The gap stays open and stays \
                   counted.",
    },
    Row {
        id: "opencode.file_mutating_tools",
        verdict: Verdict::Gap,
        finding: "OpenCode names its three file-mutating tools (edit, write, \
                  apply_patch) but publishes no tool hook at all — its plugin \
                  fires five events and none of them is a tool boundary — so \
                  nothing ever reaches a fan-out. Rift's `tool.execute` verb is \
                  a name OpenCode does not send and it emits no FileEdit \
                  either way, so every OpenCode file write is invisible to the \
                  working-tree signal.",
        evidence: "upstream: opencode/types.go:122 \
                   FileModificationTools (edit/write/apply_patch, with the \
                   apply_patch-vs-edit+write model split documented above it), \
                   used only at upstream: opencode/transcript.go:124 \
                   over an EXPORT part (`part.Tool`) and never over a hook \
                   payload; HookNames() at \
                   upstream: opencode/lifecycle.go:55 is \
                   session-start, session-end, turn-start, turn-end, \
                   compaction. Closed by the offline export ingest that also \
                   closes opencode.tokens_are_an_export: read \
                   `messages[].parts[].tool` against that list. Deliberately \
                   NOT closed by hanging a FileEdit off tool.execute — that \
                   keys capture to a verb and a field nobody publishes, which \
                   is the guess this table exists to prevent.",
    },
    Row {
        id: "opencode.tokens_are_an_export",
        verdict: Verdict::Gap,
        finding: "OpenCode's token counts live in `opencode export` output \
                  (`messages[].info.tokens.{input,output,reasoning,cache.{read,\
                  write}}`), not in any hook payload, and Rift reads none of it.",
        evidence: "upstream: opencode/types.go Tokens/Cache. Capturing it means \
                   shelling out to the agent's own CLI, which the capture path \
                   must not do inline — it belongs in the offline ingest.",
    },
    // --------------------------------------------------------- factory-droid
    Row {
        id: "factory-droid.verb_surface",
        verdict: Verdict::Gap,
        finding: "Rift declares five snake_case verbs (session_start, task_start, \
                  tool_use, task_end, session_end). Factory Droid's surface is \
                  Claude-Code-shaped PascalCase: SessionStart, SessionEnd, Stop, \
                  UserPromptSubmit, PreToolUse, PostToolUse, SubagentStop, \
                  PreCompact, Notification. Nothing it sends maps, PreCompact \
                  included.",
        evidence: "upstream: factoryaidroid/{lifecycle,types}.go: HookNames() is those \
                   nine and the settings schema carries seven PascalCase keys. \
                   UNSUPPORTED-ADAPTER FACT: this \
                   alpha does not capture Factory Droid at all — `enable` \
                   refuses it before writing a descriptor. The gap stays open \
                   and stays counted; only the promise to rewrite this table is \
                   withdrawn.",
    },
    Row {
        id: "factory-droid.file_mutating_tools",
        verdict: Verdict::Gap,
        finding: "Droid's five file-mutating tool names (Create, Write, Edit, \
                  MultiEdit, NotebookEdit) are recognized as of this cycle and \
                  fan out a FileEdit beside the ToolCall — but no Droid tool \
                  hook reaches the mapper yet, because Droid sends \
                  PreToolUse/PostToolUse and Rift declares `tool_use`. A Droid \
                  file write is therefore still captured as nothing at all.",
        evidence: "upstream: factoryaidroid/types.go:98 \
                   FileModificationTools, and the `tool_name` json tag on the \
                   hook payload structs at :63-65 and :72-75 — the list and the \
                   field the adapter reads are both the tag's, not a guess. \
                   The recognition added here survives a future rewrite unchanged, which is why it \
                   landed when it did. The remedy this row used to name — \
                   factory-droid.verb_surface — is an unsupported-adapter fact \
                   in this alpha, so nothing closes this in the alpha; \
                   the recognition sits ready for the cycle that supports Droid.",
    },
    Row {
        id: "factory-droid.session_key",
        verdict: Verdict::AtParity,
        finding: "`session_id`, already bound by the wire.",
        evidence: "upstream: factoryaidroid/types.go: `session_id` on every hook \
                   payload struct. So a Droid hook is captured under the right \
                   session today even though its verb does not map — the receipt \
                   records the skip.",
    },
    // ------------------------------------------------------------ antigravity
    Row {
        id: "antigravity.no_upstream_reference",
        verdict: Verdict::Unverifiable,
        finding: "Antigravity is a Rift-only adapter: the retired reference \
                  implementation did not support it, so nothing about it can be \
                  cross-checked against the pinned tag. Its `conversationId` \
                  spelling, its camelCase \
                  toolCall shape and its synthetic session start rest on \
                  Antigravity's own documentation and Rift's own tests.",
        evidence: "SEARCHED at the pinned tag: \
                   upstream: registry.go:164-177, whose AgentName / \
                   AgentType tables are claude-code, codex, copilot-cli, cursor, \
                   factoryai-droid, gemini, opencode and pi — no antigravity; \
                   and a tree-wide search of the pinned tag, case-insensitive, \
                   for `antigravity` — zero hits, so the absence is the whole \
                   tree's and not one file's. Recorded so the absence of parity \
                   evidence is never mistaken for parity.",
    },
    Row {
        id: "antigravity.synthetic_session_start",
        verdict: Verdict::Divergence,
        finding: "Antigravity publishes no session-start hook, so the ingest \
                  synthesizes exactly one, reserving ordinal u32::MAX and \
                  querying canonical history (not the deletable cache) to decide \
                  whether it already exists.",
        evidence: "rift_shadow::bridge::ingest_hook. Deliberate: it keeps every \
                   native event's idempotency key unchanged under retransmission.",
    },
    // ----------------------------------------------------------------- ollama
    Row {
        id: "ollama.no_agent_lifecycle",
        verdict: Verdict::AtParity,
        finding: "A local model runner fires no agent-lifecycle hooks at all. The \
                  adapter maps only what a proxy can observe — a request and a \
                  response — and its compat_notes say so rather than implying a \
                  session shape it cannot see.",
        evidence: "Rift-only adapter; not in the retired reference \
                   implementation's registry. Honest absence is the correct outcome \
                   here, not a gap.",
    },
    Row {
        id: "ollama.usage",
        verdict: Verdict::AtParity,
        finding: "`prompt_eval_count`/`eval_count` from the runtime's own \
                  response metadata, on chat_response only.",
        evidence: "Ollama's documented response fields; absent usage stays absent.",
    },
];

/// Every registered adapter carries at least one row, and every row names a
/// registered adapter. A new agent cannot be added without being swept.
#[test]
fn the_sweep_covers_every_registered_adapter() {
    let registry = AdapterRegistry::with_builtin();
    let agents: Vec<String> = registry
        .agents()
        .iter()
        .map(|a| a.as_str().to_owned())
        .collect();
    assert_eq!(agents.len(), 9, "the sweep is written for nine adapters");
    for agent in &agents {
        assert!(
            SWEEP
                .iter()
                .any(|row| row.id.starts_with(&format!("{agent}."))),
            "{agent} is registered but has no sweep row"
        );
    }
    for row in SWEEP {
        let (prefix, behaviour) = row.id.split_once('.').expect("id is <agent>.<behaviour>");
        assert!(
            prefix == "all" || agents.iter().any(|a| a == prefix),
            "sweep row {:?} names an agent that is not registered",
            row.id
        );
        assert!(!behaviour.is_empty(), "row {:?} has no behaviour", row.id);
        assert!(
            !row.finding.is_empty() && !row.evidence.is_empty(),
            "row {:?} must state a finding and its evidence",
            row.id
        );
    }
    let mut ids: Vec<&str> = SWEEP.iter().map(|r| r.id).collect();
    ids.sort_unstable();
    let before = ids.len();
    ids.dedup();
    assert_eq!(before, ids.len(), "sweep row ids must be unique");
}

/// The open-gap count is pinned, so a gap cannot be closed — or opened —
/// without the table being edited in the same commit.
#[test]
fn the_open_gap_count_is_pinned() {
    let gaps = SWEEP
        .iter()
        .filter(|r| r.verdict == Verdict::Gap)
        .collect::<Vec<_>>();
    assert_eq!(
        gaps.len(),
        OPEN_GAPS,
        "open gaps changed; edit the table and OPEN_GAPS together. Currently: {:?}",
        gaps.iter().map(|r| r.id).collect::<Vec<_>>()
    );
    // Every gap must name the work that closes it, or it is a complaint rather
    // than a finding.
    for row in gaps {
        assert!(
            row.finding.len() > 60,
            "gap {:?} must describe what is lost",
            row.id
        );
    }
}

/// **A row may only call an agent not-yet-supported if the product refuses to
/// wire it** — and every row that does say so is still counted as a gap.
///
/// An owner scope decision turned four defects into unsupported-adapter facts,
/// and the tempting reading of that is "so they are no longer open". The
/// decision says
/// the opposite in as many words: *a gap is not fixed by declaring the agent
/// unsupported*. So this pins both halves. The `Gap` half stops the count being
/// laundered; the `ALPHA_AGENTS` half stops the prose outliving the product,
/// which is this programme's signature defect — a sentence describing a
/// behaviour the code no longer has.
#[test]
fn every_unsupported_adapter_row_names_an_agent_the_product_refuses_to_wire() {
    for id in UNSUPPORTED_ADAPTER_ROWS {
        let row = SWEEP
            .iter()
            .find(|r| r.id == *id)
            .unwrap_or_else(|| panic!("{id} is listed as unsupported but is not in the table"));
        let agent = id.split_once('.').expect("id is <agent>.<behaviour>").0;
        assert!(
            !rift_shadow_hooks::is_alpha_supported(agent),
            "{id} describes {agent} as not-yet-supported, but this alpha wires \
             it: {:?}",
            rift_shadow_hooks::ALPHA_AGENTS
        );
        assert!(
            row.evidence.contains(UNSUPPORTED_MARKER),
            "{id} is listed as an unsupported-adapter fact and its evidence \
             does not say so"
        );
        assert_eq!(
            row.verdict,
            Verdict::Gap,
            "{id} stays a gap — declaring the agent unsupported withdraws the \
             promise, it does not capture the events"
        );
    }
    // …and the marker is not used anywhere the list does not account for, so
    // the enumeration stays the deliverable rather than the count.
    let marked: Vec<&str> = SWEEP
        .iter()
        .filter(|r| r.evidence.contains(UNSUPPORTED_MARKER))
        .map(|r| r.id)
        .collect();
    assert_eq!(
        marked, *UNSUPPORTED_ADAPTER_ROWS,
        "every row carrying the marker must be enumerated in \
         UNSUPPORTED_ADAPTER_ROWS, in table order"
    );
}

/// **No row may name an unsupported-adapter row as the work that closes it.**
///
/// Two rows did: `factory-droid.file_mutating_tools` said "Closed by
/// factory-droid.verb_surface" and `copilot-cli.subagent_work_uncaptured` said
/// "Closed with copilot-cli.verb_surface". An owner scope decision retired that
/// work, so both
/// sentences became promises with nothing behind them — the exact shape a
/// re-word is supposed to remove and the exact shape that comes back the next
/// time someone copies a neighbouring row.
#[test]
fn no_row_is_closed_by_work_this_alpha_retired() {
    for row in SWEEP {
        for unsupported in UNSUPPORTED_ADAPTER_ROWS {
            for claim in [
                format!("Closed by {unsupported}"),
                format!("Closed with {unsupported}"),
            ] {
                assert!(
                    !row.evidence.contains(&claim),
                    "{} claims {claim} — that work is retired for this alpha; \
                     say what is true instead",
                    row.id
                );
            }
        }
    }
}

/// An `Unverifiable` row exists to stop someone re-deriving a shape from a
/// plausible name, so it must name **the specific sources it searched** — not
/// merely be long enough to look considered.
///
/// The instrument this replaces was `evidence.len() > 60`, and it passed
/// happily on the row that claimed Copilot's session key could not be verified
/// from the permitted sources. It could not tell a search from a sentence. Two
/// named sources are the floor; a tree-wide search counts as one, spelled out
/// ([`TREE_WIDE_SEARCH`]), because that is the strongest claim a row can make
/// and it should be the hardest to make by accident.
#[test]
fn every_unverifiable_row_names_the_sources_it_searched() {
    for row in SWEEP.iter().filter(|r| r.verdict == Verdict::Unverifiable) {
        let mut searched = cited_sources(row.evidence);
        searched.sort_unstable();
        searched.dedup();
        let tree_wide = usize::from(row.evidence.contains(TREE_WIDE_SEARCH));
        assert!(
            searched.len() + tree_wide >= 2,
            "unverifiable row {:?} must name the specific sources it searched \
             and came back empty from — found {searched:?} (+{tree_wide} \
             tree-wide). Say where you looked, or the verdict is a guess with \
             better prose.",
            row.id
        );
        assert!(
            row.evidence.len() > 60,
            "unverifiable row {:?} must also explain what could not be checked",
            row.id
        );
    }
}

/// **A source this tree cannot fetch is admissible only if this tree pins it.**
///
/// The cost of the weaker rule is on record twice in this very table. A
/// citation nobody could check in one step shipped Copilot session-less for a
/// cycle; and `codex.verbs_beyond_the_surface` graded Codex's verb table
/// against a *third party's* `HookNames()` — a sentence that was true of the Go
/// wrapper and false of the agent — while the row's own evidence pointed at
/// `codex-rs/hooks/src/schema.rs`, a file `NON_TAG_SOURCES` merely permitted to
/// be named. Naming is not evidence. This test makes the naming carry an
/// artifact: the fixture, its upstream path, and a blob sha a reviewer can
/// reproduce with `git hash-object` over the bytes upstream still serves.
#[test]
fn every_non_tag_source_is_backed_by_a_pin_committed_to_this_tree() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<crate> has a workspace root two levels up");
    for source in NON_TAG_SOURCES {
        let (_, fixture) = PINNED_SURFACES
            .iter()
            .find(|(upstream, _)| upstream == source)
            .unwrap_or_else(|| {
                panic!(
                    "{source} is cited from outside the fetchable roots with no \
                     pin in this tree. Commit the surface as a fixture and list \
                     it in PINNED_SURFACES, or stop citing it."
                )
            });
        let path = root.join(fixture);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{fixture} pins {source} but cannot be read: {e}"));
        assert!(
            text.contains(source),
            "{fixture} is registered as the pin for {source} and never names it"
        );
        for field in ["repository:", "tag:", "blob-sha:"] {
            assert!(
                text.contains(field),
                "{fixture} states no {field} — a pin without provenance is a copy"
            );
        }
    }
    // …and no pin may be registered for a source the table does not cite, so
    // the pair of lists cannot drift into decoration.
    for (upstream, fixture) in PINNED_SURFACES {
        assert!(
            NON_TAG_SOURCES.contains(upstream),
            "{fixture} pins {upstream}, which no row cites"
        );
    }
}

/// Every source this table cites must resolve where it says it does.
///
/// A citation a reviewer cannot fetch is not evidence, and the cost is on
/// record: `copilot-cli.session_key_unverifiable` cited `agent/copilotcli/…`,
/// which 404s at the pinned tag, and shipped a whole agent session-less on a
/// claim nobody could check in one step.
#[test]
fn every_cited_source_resolves_where_it_claims_to() {
    for row in SWEEP {
        for source in cited_sources(row.evidence)
            .into_iter()
            .chain(cited_sources(row.finding))
        {
            assert!(
                CITATION_ROOTS.iter().any(|root| source.starts_with(root))
                    || NON_TAG_SOURCES.contains(&source.as_str()),
                "row {:?} cites {source:?}, which is under none of the roots a \
                 reader can fetch {CITATION_ROOTS:?}. Sources in the pinned \
                 tag use upstream: … — the bare `agent/…` \
                 shorthand is a 404 — and anything genuinely elsewhere belongs \
                 in NON_TAG_SOURCES by name.",
                row.id
            );
        }
    }
}

/// The extractor's own regression, so the check above cannot be quietly
/// re-holed: a **bare** filename is a citation. Filtering on `contains('/')`
/// exempted precisely the citations a reader cannot resolve, and the sweep
/// carried one (`lifecycle.go`) past a test written to forbid it.
#[test]
fn a_bare_filename_is_still_a_citation() {
    assert_eq!(
        cited_sources("HookNames() in its lifecycle.go, plus hooks.rs"),
        vec!["lifecycle.go".to_owned(), "hooks.rs".to_owned()]
    );
    assert_eq!(
        cited_sources("(upstream: copilotcli/compat.go:74)"),
        vec!["upstream: copilotcli/compat.go".to_owned()]
    );
    assert!(cited_sources("no sources here at all").is_empty());
}

/// The table states its own size, and the size is checked. Every count this
/// sweep has ever been described by — in a commit message, a design note, a
/// ledger line — came from a human counting rows by eye, and every one of them
/// was wrong.
#[test]
fn the_table_is_the_size_it_says_it_is() {
    assert_eq!(
        SWEEP.len(),
        SWEEP_ROWS,
        "row count changed; update SWEEP_ROWS"
    );
    assert_eq!(
        SWEEP
            .iter()
            .filter(|r| r.verdict == Verdict::Unverifiable)
            .count(),
        UNVERIFIABLE_ROWS,
        "unverifiable count changed; update UNVERIFIABLE_ROWS"
    );
}

// --- golden event streams ---------------------------------------------------
//
// One realistic session per adapter, asserted end to end. These catch the
// class of drift a per-verb fixture cannot: a session that opens twice, a
// compaction that replaces a turn instead of sitting between two, a subagent
// whose end never arrives, a fan-out that changes shape with delivery order.

fn stream_for(agent: &str, stream: &Stream) {
    let registry = AdapterRegistry::with_builtin();
    let adapter = registry
        .get(&rift_core::AgentId::new(agent))
        .unwrap_or_else(|| panic!("{agent} is not registered"));
    // `check_stream` is generic over AgentAdapter; the registry hands out a
    // trait object, so play it through the same helper by reference.
    check_stream(adapter, stream).unwrap_or_else(|e| panic!("{agent}: {e}"));

    // **Both directions, for the supported set only** (owner scope decisions).
    //
    // `check_stream` already refuses a step whose verb the adapter does not
    // declare — that is `stream ⊆ verbs()`. The other direction was nobody's:
    // an adapter could declare a verb, have `rift enable` register a hook for
    // it, and never once have the verb played through a recorded session shape.
    // Codex was exactly that, for five of its ten verbs, right up to this
    // commit.
    //
    // Scoped to `ALPHA_AGENTS` because the alpha's scope puts the obligation
    // there.
    // The unsupported five keep their deliberately thin streams — a stream is a
    // claim about what an agent really sends, and for an agent this alpha
    // refuses to wire, that claim is the one the sweep's Gap rows say Rift
    // cannot yet make.
    if rift_shadow_hooks::is_alpha_supported(agent) {
        for verb in adapter.verbs() {
            assert!(
                stream.steps.iter().any(|(step, _)| step == verb),
                "{agent} declares {verb} and its golden stream never plays it: a \
                 verb Rift registers a hook for and has never driven end to end"
            );
        }
    }
}

#[test]
fn claude_code_golden_stream() {
    stream_for(
        "claude-code",
        &Stream::new(
            "session with an edit, a subagent and a compaction",
            &[
                ("SessionStart", serde_json::json!({"source": "startup"})),
                ("UserPromptSubmit", serde_json::json!({"prompt": "fix it"})),
                (
                    "PreToolUse",
                    serde_json::json!({"tool_name": "Edit", "tool_input": {"file_path": "src/a.rs"}}),
                ),
                ("PostToolUse", serde_json::json!({"tool_name": "Edit"})),
                // The subagent dispatch: a tool call on `Agent`, which is what
                // Claude Code really publishes (see claude-code.subagent_boundary).
                ("PreToolUse", serde_json::json!({"tool_name": "Agent"})),
                ("SubagentStart", serde_json::json!({})),
                ("SubagentStop", serde_json::json!({})),
                (
                    "PostToolUse",
                    serde_json::json!({"tool_name": "Agent", "tool_response": {"agentId": "child-1"}}),
                ),
                ("PreCompact", serde_json::json!({"trigger": "auto"})),
                ("Stop", serde_json::json!({"usage": {"input_tokens": 10}})),
                ("SessionEnd", serde_json::json!({"reason": "exit"})),
            ],
            &[
                SessionStart,
                PromptSubmit,
                ToolCall,
                FileEdit,
                ToolResult,
                ToolCall,
                rift_core::EventKind::SubagentStart,
                rift_core::EventKind::SubagentEnd,
                ToolResult,
                SessionCompaction,
                TurnEnd,
                SessionEnd,
            ],
        ),
    );
}

#[test]
fn codex_golden_stream() {
    stream_for(
        "codex",
        &Stream::new(
            "a turn that patches a file",
            &[
                (
                    "SessionStart",
                    serde_json::json!({"session_id": "s", "model": "gpt-5.4-codex"}),
                ),
                (
                    "UserPromptSubmit",
                    serde_json::json!({"session_id": "s", "turn_id": "t1"}),
                ),
                (
                    "PreToolUse",
                    serde_json::json!({"session_id": "s", "tool_name": "apply_patch"}),
                ),
                (
                    "PostToolUse",
                    serde_json::json!({"session_id": "s", "tool_name": "apply_patch"}),
                ),
                // The subagent pair, keyed by the child id Codex's own schema
                // names (`agent_id`) rather than the `subagent_id` this adapter
                // used to read.
                (
                    "SubagentStart",
                    serde_json::json!({"session_id": "s", "turn_id": "t1",
                                       "agent_id": "a1", "agent_type": "review"}),
                ),
                (
                    "SubagentStop",
                    serde_json::json!({"session_id": "s", "turn_id": "t1", "agent_id": "a1"}),
                ),
                (
                    "PreCompact",
                    serde_json::json!({"session_id": "s", "turn_id": "t1"}),
                ),
                (
                    "PostCompact",
                    serde_json::json!({"session_id": "s", "turn_id": "t1"}),
                ),
                (
                    "Stop",
                    serde_json::json!({"session_id": "s", "turn_id": "t1"}),
                ),
                // SessionEnd, which this stream used to end without — under a
                // comment reading *"Codex publishes none, and the adapter does
                // not invent one"*. Codex's own pinned schema generates
                // `session-end.command.input`, so the comment was false and the
                // absence it justified was a capture hole: a Codex session had
                // an end nothing recorded.
                (
                    "SessionEnd",
                    serde_json::json!({"session_id": "s", "reason": "other"}),
                ),
            ],
            &[
                SessionStart,
                PromptSubmit,
                ToolCall,
                FileEdit,
                ToolResult,
                rift_core::EventKind::SubagentStart,
                rift_core::EventKind::SubagentEnd,
                SessionCompaction,
                SessionCompaction,
                TurnEnd,
                SessionEnd,
            ],
        ),
    );
}

#[test]
fn gemini_golden_stream() {
    stream_for(
        "gemini",
        &Stream::new(
            "a turn whose model is stated and then dropped",
            &[
                ("SessionStart", serde_json::json!({"session_id": "s"})),
                (
                    "BeforeAgent",
                    serde_json::json!({"session_id": "s", "prompt": "go"}),
                ),
                // The only hook that states the model — and it produces nothing.
                // The empty step is the gemini.model_fact_is_dropped gap, made
                // visible rather than described.
                (
                    "BeforeModel",
                    serde_json::json!({"session_id": "s", "llm_request": {"model": "gemini-2.5-pro"}}),
                ),
                ("AfterModel", serde_json::json!({"session_id": "s"})),
                (
                    "BeforeTool",
                    serde_json::json!({"session_id": "s", "tool_name": "edit_file"}),
                ),
                ("AfterTool", serde_json::json!({"session_id": "s"})),
                ("PreCompress", serde_json::json!({"session_id": "s"})),
                ("AfterAgent", serde_json::json!({"session_id": "s"})),
                ("SessionEnd", serde_json::json!({"session_id": "s"})),
            ],
            &[
                SessionStart,
                PromptSubmit,
                PromptResponse,
                ToolCall,
                FileEdit,
                ToolResult,
                SessionCompaction,
                TurnEnd,
                SessionEnd,
            ],
        ),
    );
}

#[test]
fn antigravity_golden_stream() {
    stream_for(
        "antigravity",
        &Stream::new(
            "the first invocation is the session start",
            &[
                (
                    "PreInvocation",
                    serde_json::json!({"conversationId": "c", "invocationNum": 0}),
                ),
                (
                    "PreToolUse",
                    serde_json::json!({"toolCall": {"name": "write_to_file"}}),
                ),
                ("PostToolUse", serde_json::json!({"stepIdx": 1})),
                ("PostInvocation", serde_json::json!({"invocationNum": 0})),
                // A later invocation is not a second session start.
                (
                    "PreInvocation",
                    serde_json::json!({"conversationId": "c", "invocationNum": 1}),
                ),
                ("Stop", serde_json::json!({"fullyIdle": true})),
            ],
            &[
                SessionStart,
                ToolCall,
                FileEdit,
                ToolResult,
                PromptResponse,
                TurnEnd,
            ],
        ),
    );
}

#[test]
fn cursor_golden_stream() {
    stream_for(
        "cursor",
        &Stream::new(
            // Deliberately thin: this is what the current adapter can express,
            // and the gap rows above record the seven verbs it cannot.
            "everything the current Cursor adapter can express",
            &[
                ("session_start", serde_json::json!({})),
                ("edit", serde_json::json!({})),
                ("session_end", serde_json::json!({})),
            ],
            &[SessionStart, FileEdit, SessionEnd],
        ),
    );
}

#[test]
fn copilot_golden_stream() {
    stream_for(
        "copilot-cli",
        &Stream::new(
            "everything the current Copilot adapter can express",
            &[
                ("session_start", serde_json::json!({})),
                ("prompt", serde_json::json!({})),
                ("tool", serde_json::json!({})),
                ("session_end", serde_json::json!({})),
            ],
            &[SessionStart, PromptSubmit, ToolCall, SessionEnd],
        ),
    );
}

#[test]
fn opencode_golden_stream() {
    stream_for(
        "opencode",
        &Stream::new(
            "everything the current OpenCode adapter can express",
            &[
                ("session.start", serde_json::json!({"session_id": "s"})),
                ("message", serde_json::json!({"session_id": "s"})),
                ("tool.execute", serde_json::json!({"session_id": "s"})),
                ("session.end", serde_json::json!({"session_id": "s"})),
            ],
            &[SessionStart, PromptSubmit, ToolCall, SessionEnd],
        ),
    );
}

#[test]
fn factory_droid_golden_stream() {
    stream_for(
        "factory-droid",
        &Stream::new(
            "a task is a turn",
            &[
                ("session_start", serde_json::json!({"session_id": "s"})),
                ("task_start", serde_json::json!({"session_id": "s"})),
                // Droid's own tool-name spelling and one of its five
                // file-mutating tools (factory-droid.file_mutating_tools).
                (
                    "tool_use",
                    serde_json::json!({"session_id": "s", "tool_name": "Write"}),
                ),
                ("task_end", serde_json::json!({"session_id": "s"})),
                ("session_end", serde_json::json!({"session_id": "s"})),
            ],
            &[
                SessionStart,
                TurnStart,
                ToolCall,
                FileEdit,
                TurnEnd,
                SessionEnd,
            ],
        ),
    );
}

#[test]
fn ollama_golden_stream() {
    stream_for(
        "ollama",
        &Stream::new(
            "a proxied exchange, with no session shape to invent",
            &[
                ("chat_request", serde_json::json!({"model": "llama3"})),
                (
                    "chat_response",
                    serde_json::json!({"prompt_eval_count": 20, "eval_count": 7}),
                ),
            ],
            &[PromptSubmit, PromptResponse],
        ),
    );
}
