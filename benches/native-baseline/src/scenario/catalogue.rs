//! What the harness measures: the scenario set, the axes each row is declared
//! on, and the declaration itself.
//!
//! Its own module because these declarations are the published contract — the
//! ids, their order and the arity of every row array are what an artifact is
//! read by — and they must be readable without the machinery that runs them.

use serde::{Deserialize, Serialize};

use crate::measure::{Budget, Protocol};

use super::event::{HookEvent, HOOK_FIXTURE};

/// One measure from the declared set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScenarioId {
    /// Agent hook in, event durably ingested, process gone.
    HookRoundTrip,
    /// The session's first hook.
    HookSessionStart,
    /// A user prompt: opens a turn.
    HookPromptSubmit,
    /// A read-shaped tool call: worktree hashing plus tokenization.
    HookReadTool,
    /// A tool result.
    HookToolResult,
    /// End of turn.
    HookStop,
    /// End of session.
    HookSessionEnd,
    /// The same event delivered twice: the dedup no-op.
    HookReplay,
    /// A read the gate really decides about: the whole predicate, the reason
    /// the model is answered with, and the accounting it journals.
    HookReadGate,
    /// An edit recorded: the file hashed, the sites located in the agent's own
    /// payload, the line appended.
    HookEditRecord,
    /// First-touch identity binding, counted in child processes.
    IdentityAttribution,
    /// `status` against a cold repository.
    ColdStatus,
    /// Listing recorded checkpoints.
    CheckpointList,
    /// Condensation at commit.
    Condense,
    /// Restoring a session on resume.
    ResumeRestore,
    /// Importing an existing capture history.
    ImportThroughput,
    /// Line → prompt provenance latency.
    BlameWhy,
    /// Re-deriving the hash chain and cross-checking the projection.
    VerifyThroughput,
    /// `git worktree add` in a wired family: the checkout that pays adoption.
    WorktreeAddAdopt,
    /// `git switch` in a member already provisioned: the fast path.
    MemberSwitch,
    /// The ancestry walk branch containment added to a serve.
    RecallKnownState,
    /// What a member is served out of the family's shared store.
    RecallCompounding,
    /// What a scripted session's repeat pool is still worth once the gate has
    /// run over it.
    RecallInterceptSavings,
}

impl ScenarioId {
    /// Every scenario, in report order.
    #[must_use]
    pub const fn all() -> [Self; 23] {
        [
            Self::HookRoundTrip,
            Self::HookSessionStart,
            Self::HookPromptSubmit,
            Self::HookReadTool,
            Self::HookToolResult,
            Self::HookStop,
            Self::HookSessionEnd,
            Self::HookReplay,
            Self::HookReadGate,
            Self::HookEditRecord,
            Self::IdentityAttribution,
            Self::ColdStatus,
            Self::CheckpointList,
            Self::Condense,
            Self::ResumeRestore,
            Self::ImportThroughput,
            Self::BlameWhy,
            Self::VerifyThroughput,
            Self::WorktreeAddAdopt,
            Self::MemberSwitch,
            Self::RecallKnownState,
            Self::RecallCompounding,
            Self::RecallInterceptSavings,
        ]
    }

    /// The stable identifier used in reports and on the command line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HookRoundTrip => "hook-round-trip",
            Self::HookSessionStart => "hook-session-start",
            Self::HookPromptSubmit => "hook-prompt-submit",
            Self::HookReadTool => "hook-read-tool",
            Self::HookToolResult => "hook-tool-result",
            Self::HookStop => "hook-stop",
            Self::HookSessionEnd => "hook-session-end",
            Self::HookReplay => "hook-replay",
            Self::HookReadGate => "hook-read-gate",
            Self::HookEditRecord => "hook-edit-record",
            Self::IdentityAttribution => "identity-attribution",
            Self::ColdStatus => "cold-status",
            Self::CheckpointList => "checkpoint-list",
            Self::Condense => "condense",
            Self::ResumeRestore => "resume-restore",
            Self::ImportThroughput => "import-throughput",
            Self::BlameWhy => "blame-why",
            Self::VerifyThroughput => "verify-throughput",
            Self::WorktreeAddAdopt => "worktree-add-adopt",
            Self::MemberSwitch => "member-switch",
            Self::RecallKnownState => "recall-known-state",
            Self::RecallCompounding => "recall-compounding",
            Self::RecallInterceptSavings => "recall-intercept-savings",
        }
    }

    /// Parse an identifier.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::all().into_iter().find(|id| id.as_str() == name)
    }

    /// The hook rows, in session order.
    #[must_use]
    pub fn hooks() -> Vec<Self> {
        Self::all()
            .into_iter()
            .filter(|id| spec(*id).hook_event.is_some())
            .collect()
    }
}

/// Whether a scenario is driven today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// Driven end to end.
    Wired,
    /// Declared, not yet driven; the note names what it waits on.
    Planned(&'static str),
}

/// How the fixture is isolated across iterations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    /// One prepared repository for every iteration (read-only verbs, or where
    /// accumulating state is the realistic shape).
    Shared,
    /// A fresh copy per iteration — the only way a "cold" measure is cold, and
    /// mandatory for verbs that write.
    FreshCopy,
}

/// What each iteration of a hook row delivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// A new event every iteration — what a live session does, and the only
    /// shape whose timing includes ingest.
    Distinct,
    /// The same event again and again. Re-ingest is a designed no-op (the
    /// idempotency key is clock-free), so this row measures the dedup
    /// short-circuit — a real path, worth a number of its own, and never to be
    /// confused with the round-trip it is a fraction of.
    Replay,
}

/// Which session a hook row's timed verb is delivered to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    /// The shared [`BENCH_SESSION`](super::BENCH_SESSION), established before
    /// the first measured iteration — the warm path an agent spends a session
    /// in.
    Established,
    /// A session of its own per iteration, for the once-in-a-lifetime verbs.
    PerIteration,
}

/// A scenario's static declaration.
#[derive(Debug, Clone, Copy)]
pub struct ScenarioSpec {
    /// Its identifier.
    pub id: ScenarioId,
    /// One-line description for the report.
    pub title: &'static str,
    /// Which fixture preset it runs against.
    pub fixture: &'static str,
    /// Iteration counts.
    pub protocol: Protocol,
    /// Per-iteration isolation.
    pub isolation: Isolation,
    /// Part of the headline set the report leads with (the plan's exit
    /// criteria: hook p50, condense time, cold status, import throughput).
    pub headline: bool,
    /// Whether it is wired today.
    pub availability: Availability,
    /// The hook event this row times, when it is a hook row.
    pub hook_event: Option<HookEvent>,
    /// Which session the timed hook is delivered to.
    pub session: Session,
    /// Whether each iteration is a new event or the same one again.
    pub delivery: Delivery,
    /// Whether the run takes the extra untimed pass that counts the child
    /// processes one invocation launches. Only rows whose *claim* is a process
    /// count ask for it: the pass costs a whole extra invocation and its shims
    /// perturb what they measure, so it is opt-in per row rather than free.
    pub spawn_probe: bool,
    /// The budget this measure is held to, when the plan states one.
    pub budget: Option<Budget>,
    /// What one unit of this row's work is (`checkpoints`, `hooks`, `shards`).
    ///
    /// Present only where a probe can actually count them: the unit names the
    /// denominator of the rate, and a unit with nothing counting it would put a
    /// column heading over an empty column.
    pub work_unit: Option<&'static str>,
    /// A sentence that must travel with this row's number, verbatim, into the
    /// artifact and the report.
    ///
    /// Not a footnote and not a title: a caveat that lives only in prose beside
    /// a table is a caveat a reader can quote the number without. This one is a
    /// field of the serialized record, so anything that reads the artifact
    /// reads the qualification with it.
    pub caveat: Option<&'static str>,
    /// Who owes this row its measurement, or `None` for a row already
    /// measured and published.
    ///
    /// A row is **declared** by the change that wires it and **measured** by
    /// the sitting that runs it, and those are not the same commit: the
    /// shadow-worktree rows were declared by one change and measured by a
    /// later one. The two states have to be tellable apart, because the
    /// published fence (`crate::report_facts`) holds every row it carries to
    /// three clean runs — rightly: a published report must not carry an
    /// unmeasured row. An owed row is therefore **absent** from the fence
    /// rather than published as a zero, which is the project's standing rule
    /// that a claim distinguishes what ran from what is owed, applied to the
    /// one place this crate could have blurred it.
    ///
    /// The state retires itself: `an_owed_row_has_no_measurement_to_publish`
    /// reds the moment a clean run for one of these rows exists in the tracked
    /// tree, so the sitting that measures a row is the same change that takes
    /// it off this list.
    pub measurement_owed: Option<&'static str>,
}

/// The reconstruction caveat, in the words the artifact carries.
///
/// The blocker on the import row was recorded as *"needs an `reference/*` dialect
/// fixture"*, which reads as a thing that will arrive. It will not: nothing was
/// ever fetched and nothing is obtainable. What exists is a reconstruction, so
/// the number is about our reader over our own shards — a real and useful
/// number, and **not** a statement about the format.
pub const IMPORT_RECONSTRUCTION_CAVEAT: &str = "This rate is true about our own reconstruction of \
     the pre-Rift on-disk format and silent about the format itself. No legacy artifact was ever \
     fetched — the README beside the golden shards under `legacy-fixtures/` says so in its own \
     words, \"Nothing here was fetched\" — and every \
     spelling in these shards is cited there to a declaration that already lived in this \
     repository, and the corpus is that frozen shard replicated under distinct ids. It measures \
     the reader — the walk, the parse, the root re-derivation. It is never a format-compatibility \
     claim.";

/// The measurement pass that produced the shadow-worktree rows' numbers.
///
/// A label for prose and caveats only: it names nothing a machine reads, and
/// no artifact carries it.
pub const WORKTREE_ROW_MEASUREMENT: &str = "the quiet-machine sitting of 2026-08-31";

/// What the ancestry row's number is, and what it is not.
pub const ANCESTRY_WALK_CAVEAT: &str = "This is the WHOLE serve, not the walk. \
     The branch-containment decision budgets the ancestry walk at <= 20 ms p50 ADDED to this \
     verb, and an added cost is a difference between two measurements: this row is one of them, \
     and the other is the same verb on a build without the branch axis. Quote the difference the \
     measurement reports, never this number, as the cost of containment.";

/// What the compounding row's number is, and how its ratio is formed.
pub const COMPOUNDING_RATIO_CAVEAT: &str = "The work unit is the tokens ONE serve in a linked \
     member avoided out of the family's shared store — the member captured none of those reads \
     itself. The published compounding ratio is that figure over the same serve's figure under \
     the per-path baseline (one store per checkout, which is what a member had before family \
     identity), and the measurement prints both numerators beside it. A ratio without its two \
     numerators is not \
     a measurement, and this row's number is one of them, not the ratio.";

impl ScenarioSpec {
    /// Whether this row's workload is authored by the scripted-session driver
    /// rather than by the harness itself.
    #[must_use]
    pub fn needs_driver(&self) -> bool {
        self.id == ScenarioId::Condense
    }

    /// Whether this scenario is driven today.
    #[must_use]
    pub fn is_wired(&self) -> bool {
        matches!(self.availability, Availability::Wired)
    }

    /// Whether a measured iteration must be able to prove it wrote something
    /// durable. A hook that was supposed to ingest and did not is not a hook
    /// round-trip, and publishing its wall time as a fast number is exactly the
    /// self-flattery this harness exists to prevent. A `Replay` row is the one
    /// case where writing nothing is the correct behaviour, so it is asked for
    /// no such proof — and is never presented as the round-trip.
    #[must_use]
    pub fn requires_ingest(&self) -> bool {
        self.hook_event.is_some() && self.delivery == Delivery::Distinct
    }

    /// Which logical event iteration `i` delivers.
    #[must_use]
    pub fn nth_event(&self, iteration: usize) -> usize {
        match self.delivery {
            Delivery::Distinct => iteration,
            Delivery::Replay => 0,
        }
    }
}

/// The full catalogue, in report order: the hook rows first, then the rest.
#[must_use]
pub fn catalogue() -> [ScenarioSpec; 23] {
    let [round_trip, session_start, prompt_submit, read_tool, tool_result, stop, session_end, replay, read_gate, edit_record, identity] =
        hook_rows();
    let [cold_status, checkpoint_list, condense, resume, import, blame, verify, worktree_add, member_switch, known_state, compounding, intercept_savings] =
        other_rows();
    [
        round_trip,
        session_start,
        prompt_submit,
        read_tool,
        tool_result,
        stop,
        session_end,
        replay,
        read_gate,
        edit_record,
        identity,
        cold_status,
        checkpoint_list,
        condense,
        resume,
        import,
        blame,
        verify,
        worktree_add,
        member_switch,
        known_state,
        compounding,
        intercept_savings,
    ]
}

/// The shape every hook row shares; only the verb, the session, the delivery
/// and the prelude differ.
fn hook_row(
    id: ScenarioId,
    title: &'static str,
    event: HookEvent,
    session: Session,
    delivery: Delivery,
    headline: bool,
) -> ScenarioSpec {
    ScenarioSpec {
        id,
        title,
        fixture: HOOK_FIXTURE,
        protocol: Protocol::latency(),
        isolation: Isolation::Shared,
        headline,
        availability: Availability::Wired,
        hook_event: Some(event),
        session,
        delivery,
        spawn_probe: false,
        budget: Some(Budget::hook()),
        work_unit: None,
        caveat: None,
        measurement_owed: None,
    }
}

/// The two rows the read gate added: the read it decides about and the edit it
/// records.
///
/// Their own function rather than two more entries above, because each needs a
/// prelude the other hook rows do not — a policy file that asks for declines,
/// or a file for an edit to have landed on — and because both are declared
/// here before any sitting has measured them.
fn gate_rows() -> [ScenarioSpec; 2] {
    use Delivery::Distinct;
    use Session::{Established, PerIteration};

    let hook = hook_row;
    [
        ScenarioSpec {
            measurement_owed: Some(OWED_TO_THE_GATE_WINDOW),
            ..hook(
                ScenarioId::HookReadGate,
            "a read the gate DECLINES (`PreToolUse` / `Read`, the lines already held, this repository asking for declines): hash, journal, predicate, the reason the model is answered with, and the accounting journaled for the detached worker",
            HookEvent::ToolCallReadRepeat,
            // A window that has already declined a key is never asked about it
            // again — one deny per key is what bounds a model that answers a
            // decline by asking again — so a shared session would time the
            // allow path from the iteration the fixture runs out of unread
            // files. Each iteration gets its own window, its own first read and
            // its own decline, and the probe below refuses the row if the timed
            // call was answered any other way.
            PerIteration,
            Distinct,
            false,
        )
        },
        ScenarioSpec {
            measurement_owed: Some(OWED_TO_THE_GATE_WINDOW),
            ..hook(
            ScenarioId::HookEditRecord,
            "an edit recorded (`PostToolUse` / `Edit`): the file hashed and the sites located in the agent's own payload, which is what lets a later read be decided at all",
            HookEvent::ToolEditRecord,
            Established,
            Distinct,
            false,
        )
        },
    ]
}

/// The hook rows: one per hook verb, all on the `small` fixture,
/// all held to the same budget.
fn hook_rows() -> [ScenarioSpec; 11] {
    let [round_trip, session_start, prompt_submit, read_tool, tool_result, stop, session_end, replay] =
        verb_rows();
    let [read_gate, edit_record] = gate_rows();
    [
        round_trip,
        session_start,
        prompt_submit,
        read_tool,
        tool_result,
        stop,
        session_end,
        replay,
        read_gate,
        edit_record,
        identity_row(),
    ]
}

/// One row per hook verb a live session fires, in the order it fires them,
/// and the replay of the first.
fn verb_rows() -> [ScenarioSpec; 8] {
    use Delivery::{Distinct, Replay};
    use HookEvent::{
        PromptSubmit, SessionEnd, SessionStart, Stop, ToolCallBash, ToolCallRead, ToolResult,
    };
    use Session::{Established, PerIteration};

    let hook = hook_row;
    [
        hook(
            ScenarioId::HookRoundTrip,
            "agent hook in → event ingested → process exited (`PreToolUse`, warm session)",
            ToolCallBash,
            Established,
            Distinct,
            true,
        ),
        hook(
            ScenarioId::HookSessionStart,
            "the session's first hook (`SessionStart`, new session each iteration)",
            SessionStart,
            PerIteration,
            Distinct,
            false,
        ),
        hook(
            ScenarioId::HookPromptSubmit,
            "a user prompt opens a turn (`UserPromptSubmit`)",
            PromptSubmit,
            Established,
            Distinct,
            false,
        ),
        hook(
            ScenarioId::HookReadTool,
            "a read-shaped tool call (`PreToolUse` / `Read`): worktree hash; the BPE count is journaled for the detached worker, never taken here",
            ToolCallRead,
            Established,
            Distinct,
            false,
        ),
        hook(
            ScenarioId::HookToolResult,
            "a tool result (`PostToolUse`, after its own `PreToolUse`)",
            ToolResult,
            Established,
            Distinct,
            false,
        ),
        hook(
            ScenarioId::HookStop,
            "end of turn (`Stop`, closing a turn opened this iteration; its spend is settled by the next hook, not here)",
            Stop,
            Established,
            Distinct,
            false,
        ),
        hook(
            ScenarioId::HookSessionEnd,
            "end of session (`SessionEnd`, after a whole scripted session; the condense is journaled for the detached worker, never run here)",
            SessionEnd,
            PerIteration,
            Distinct,
            false,
        ),
        hook(
            ScenarioId::HookReplay,
            "the same event delivered again (`PreToolUse`): the dedup no-op, not a round-trip",
            ToolCallBash,
            Established,
            Replay,
            false,
        ),
    ]
}

/// Same verb as `hook-session-start`, different claim. That row publishes the
/// latency of a session's first hook; this one publishes what the latency is
/// *made of* — the child processes first-touch identity binding launches.
///
/// They are separate rows because the count is taken through PATH shims that
/// cost an exec per spawn: folding the probe into the latency row would either
/// perturb the published timing or hide the count behind it.
fn identity_row() -> ScenarioSpec {
    ScenarioSpec {
        spawn_probe: true,
        ..hook_row(
            ScenarioId::IdentityAttribution,
            "first-touch identity binding: child processes per session's first hook (the machine id is read from the salted cache once the install has derived it)",
            HookEvent::SessionStart,
            Session::PerIteration,
            Delivery::Distinct,
            false,
        )
    }
}

/// The shape every non-hook row starts from.
fn other_row(
    id: ScenarioId,
    title: &'static str,
    fixture: &'static str,
    protocol: Protocol,
    isolation: Isolation,
    headline: bool,
    availability: Availability,
) -> ScenarioSpec {
    ScenarioSpec {
        id,
        title,
        fixture,
        protocol,
        isolation,
        headline,
        availability,
        hook_event: None,
        // A per-iteration session for the rows whose prelude plays a whole
        // session: each iteration's commit condenses the session that
        // iteration opened, not a shared one every iteration re-closes.
        session: if matches!(
            id,
            ScenarioId::ResumeRestore
                | ScenarioId::BlameWhy
                | ScenarioId::RecallKnownState
                | ScenarioId::RecallCompounding
                | ScenarioId::RecallInterceptSavings
        ) {
            Session::PerIteration
        } else {
            Session::Established
        },
        delivery: Delivery::Distinct,
        spawn_probe: false,
        budget: None,
        work_unit: None,
        caveat: None,
        measurement_owed: None,
    }
}

/// Everything the catalogue measures that is not a hook verb: the plan's own
/// rows, then the shadow-worktree rows.
fn other_rows() -> [ScenarioSpec; 12] {
    let [cold_status, checkpoint_list, condense, resume, import, blame, verify] = plan_rows();
    let [worktree_add, member_switch, known_state, compounding] = worktree_rows();
    [
        cold_status,
        checkpoint_list,
        condense,
        resume,
        import,
        blame,
        verify,
        worktree_add,
        member_switch,
        known_state,
        compounding,
        intercept_savings_row(),
    ]
}

/// Who owes the three gate rows their first numbers. The state retires itself:
/// the sitting that measures one is the change that takes it off this list.
pub const OWED_TO_THE_GATE_WINDOW: &str =
    "the quiet-machine sitting that follows the read gate landing";

/// What the intercept row's number is, and what it is not.
pub const INTERCEPT_POOL_CAVEAT: &str = "WIRING, NOT SAVING. This row drives a whole scripted      session against a repository that asks for declines and then asks the product what its own      report still classes as a re-read of unchanged bytes. The harness delivers no tool RESULT      for a repeat, so that residue would be zero with every lever switched off as well: the      number proves the session ran and the report answered, and it can NEVER be quoted as a      saving or as a share. The saving is measured where it can be priced — by the product's own      fixture, `read_gate_e2e::the_intercept_fixture_removes_at_least_ninety_percent_of_its_pool`,      which publishes the pool, what the model received instead, the difference and the booked      figures. Quote those, never this one.";

/// What a scripted session's repeat pool is still worth once the gate has run.
///
/// The harness links no rift crate, so it cannot tokenize and cannot price a
/// call the product declined to make. What it CAN do is drive a whole session
/// against a repository that asks for declines and then ask the product what
/// its own report still classes as a re-read of unchanged bytes — the residue.
/// That is a real, falsifiable number about the mechanism working, and the
/// caveat says outright which half of the fraction it is.
fn intercept_savings_row() -> ScenarioSpec {
    ScenarioSpec {
        work_unit: Some("pool-tokens-left"),
        caveat: Some(INTERCEPT_POOL_CAVEAT),
        measurement_owed: Some(OWED_TO_THE_GATE_WINDOW),
        ..other_row(
            ScenarioId::RecallInterceptSavings,
            "a scripted session of reads, re-reads, edits and the reads that verify them, run              against a repository asking for declines: what its repeat pool is still worth              afterwards",
            HOOK_FIXTURE,
            Protocol::heavy(),
            Isolation::FreshCopy,
            false,
            Availability::Wired,
        )
    }
}

/// Everything the plan measures that is not a hook verb, in report order.
fn plan_rows() -> [ScenarioSpec; 7] {
    [
        cold_status_row(),
        checkpoint_list_row(),
        condense_row(),
        resume_row(),
        import_row(),
        blame_why_row(),
        verify_row(),
    ]
}

/// `status` against a repository nothing has warmed.
fn cold_status_row() -> ScenarioSpec {
    other_row(
        ScenarioId::ColdStatus,
        "cold `status` (fresh copy per iteration)",
        "small",
        Protocol::latency(),
        Isolation::FreshCopy,
        true,
        Availability::Wired,
    )
}

/// Listing what has been recorded, over a repository that keeps accumulating
/// it.
fn checkpoint_list_row() -> ScenarioSpec {
    other_row(
        ScenarioId::CheckpointList,
        "`checkpoint list` over recorded checkpoints",
        "small",
        Protocol::latency(),
        Isolation::Shared,
        false,
        Availability::Wired,
    )
}

/// Condensation at commit.
///
/// One measured iteration plays a whole session — session, turns, tool pairs, a
/// subagent fan-out, a compaction — and then commits, so the post-commit
/// condense path meets a live session rather than an empty one.
fn condense_row() -> ScenarioSpec {
    ScenarioSpec {
        work_unit: Some("events"),
        budget: Some(Budget::unratified_throughput()),
        ..other_row(
            ScenarioId::Condense,
            "condensation at commit: one scripted session (3 turns, 25 hooks) then a commit",
            "medium",
            Protocol::heavy(),
            Isolation::FreshCopy,
            true,
            Availability::Wired,
        )
    }
}

/// Restoring a session on resume.
///
/// One iteration plays a whole session on a session id of its own, commits
/// with the repository's own hooks so the checkpoint shard lands inline, then
/// times the resume of that branch — checkout (a no-op on the current branch),
/// trailer walk, shard read, transcript restore. The answer probe re-runs it
/// untimed and requires the transcript to have been `restored`: a resume that
/// found nothing to restore is not a resume.
fn resume_row() -> ScenarioSpec {
    other_row(
        ScenarioId::ResumeRestore,
        "session resume: restore transcript + metadata from the branch's latest checkpoint",
        HOOK_FIXTURE,
        Protocol::latency(),
        Isolation::Shared,
        false,
        Availability::Wired,
    )
}

/// Reading a pre-Rift capture history.
///
/// The blocker recorded here was *"needs an reference/* dialect fixture"*, and
/// it was mis-stated rather than unresolved: no such artifact was ever obtained
/// and none is obtainable — the README beside the golden shards says "Nothing
/// here was fetched". What exists is a frozen, byte-compared reconstruction, so
/// the row is driven over that and **carries the caveat in the artifact** — the
/// rate is about the reader, over shards this repository declared.
fn import_row() -> ScenarioSpec {
    ScenarioSpec {
        work_unit: Some("shards"),
        budget: Some(Budget::unratified_throughput()),
        caveat: Some(IMPORT_RECONSTRUCTION_CAVEAT),
        ..other_row(
            ScenarioId::ImportThroughput,
            "read a pre-Rift capture history: 64 reconstructed shards, walked and parsed",
            "small",
            Protocol::heavy(),
            Isolation::FreshCopy,
            true,
            Availability::Wired,
        )
    }
}

/// Line → checkpoint → prompt provenance.
///
/// One iteration plays a session that writes a file inside its turn and commits
/// it through the hooks (trailer + shard inline), then times `why <file>:1`.
/// The answer probe re-runs it untimed and requires `selection.checkpoint_id`
/// to be a string: a `why` that resolved no checkpoint answered nothing.
fn blame_why_row() -> ScenarioSpec {
    other_row(
        ScenarioId::BlameWhy,
        "line → checkpoint → prompt provenance (`why <file>:1` on a line this iteration's session wrote)",
        HOOK_FIXTURE,
        Protocol::latency(),
        Isolation::Shared,
        false,
        Availability::Wired,
    )
}

/// Re-deriving the hash chain and cross-checking the projection.
///
/// The row that made the denominator's absence concrete: `rift verify` over a
/// repository holding no checkpoints exits fast and correctly, and its wall
/// time is a fine number about nothing. The work unit turns that from something
/// a reader has to know into something the report prints. The floor is declared
/// and **unratified** — no verify-throughput target exists anywhere in this
/// project either, so the verdict reads `OPEN` rather than manufacturing a
/// pass — the owner set no target.
fn verify_row() -> ScenarioSpec {
    ScenarioSpec {
        work_unit: Some("checkpoints"),
        budget: Some(Budget::unratified_throughput()),
        ..other_row(
            ScenarioId::VerifyThroughput,
            "re-derive the hash chain and cross-check (rift-only)",
            "medium",
            Protocol::heavy(),
            Isolation::Shared,
            false,
            Availability::Wired,
        )
    }
}

/// The shadow-worktree rows.
///
/// Declared by the change that wired the seventh git hook and measured by a
/// later quiet-machine sitting: each carried `measurement_owed` until that
/// sitting ran — the owed state retired itself the moment its three clean runs
/// landed in the tracked tree.
fn worktree_rows() -> [ScenarioSpec; 4] {
    [
        worktree_add_row(),
        member_switch_row(),
        known_state_row(),
        compounding_row(),
    ]
}

/// GIT latency, not agent latency: what the seventh hook adds to the checkout
/// a person typed, in the case that pays full adoption.
///
/// Held to the owner's hard git-latency bar and reported open-with-a-number if
/// it does not fit — never asserted in a test.
fn worktree_add_row() -> ScenarioSpec {
    ScenarioSpec {
        budget: Some(Budget::git_latency()),
        measurement_owed: None,
        ..other_row(
            ScenarioId::WorktreeAddAdopt,
            "`git worktree add` in a family with an agent enabled: the checkout that pays \
             full adoption",
            HOOK_FIXTURE,
            Protocol::latency(),
            Isolation::FreshCopy,
            false,
            Availability::Wired,
        )
    }
}

/// The 99% case, and NOT the same shape as the row above: a member that is
/// already provisioned pays one rift spawn plus discovery and writes nothing.
///
/// A family with NO agent enabled is a THIRD shape — it never writes the
/// ownership manifest, so every switch re-reads the projection read-only; that
/// is inside the bar and deliberate, but it is not what this row measures and
/// must never be quoted for it.
fn member_switch_row() -> ScenarioSpec {
    ScenarioSpec {
        budget: Some(Budget::git_latency()),
        measurement_owed: None,
        ..other_row(
            ScenarioId::MemberSwitch,
            "`git switch` in a member the previous checkout already provisioned: the fast path",
            HOOK_FIXTURE,
            Protocol::latency(),
            Isolation::FreshCopy,
            false,
            Availability::Wired,
        )
    }
}

/// What branch containment costs a serve.
///
/// A real axis costs a walk: one capped, memoized first-parent-inclusive
/// ancestry walk per serve. That walk is budgeted at <= 20 ms p50 ADDED, which
/// is a delta this harness cannot state as a row's own ceiling — so the row
/// publishes the whole serve and the caveat names the comparison the
/// measurement makes.
fn known_state_row() -> ScenarioSpec {
    ScenarioSpec {
        caveat: Some(ANCESTRY_WALK_CAVEAT),
        measurement_owed: None,
        ..other_row(
            ScenarioId::RecallKnownState,
            "`recall known-state` over a repository with real ancestry: the walk containment \
             added to a serve",
            HOOK_FIXTURE,
            Protocol::latency(),
            Isolation::Shared,
            false,
            Availability::Wired,
        )
    }
}

/// The owner's requirement, made a number: a member is served what its sibling
/// read, out of the one store the family shares.
///
/// The work unit is the family's own accounting figure, so the row publishes
/// what compounded rather than only how long it took.
fn compounding_row() -> ScenarioSpec {
    ScenarioSpec {
        work_unit: Some("tokens-avoided"),
        budget: Some(Budget::unratified_throughput()),
        caveat: Some(COMPOUNDING_RATIO_CAVEAT),
        measurement_owed: None,
        ..other_row(
            ScenarioId::RecallCompounding,
            "a linked member is served the reads its sibling captured: the family's shared \
             store, from a checkout that captured none of it",
            HOOK_FIXTURE,
            Protocol::heavy(),
            Isolation::FreshCopy,
            false,
            Availability::Wired,
        )
    }
}

/// The declaration for one scenario.
#[must_use]
pub fn spec(id: ScenarioId) -> ScenarioSpec {
    let all = catalogue();
    // `catalogue()` is exhaustive over `ScenarioId::all()`; the fallback keeps the
    // function total without an unwrap.
    all.into_iter()
        .find(|s| s.id == id)
        .unwrap_or(ScenarioSpec {
            id,
            title: "unknown scenario",
            fixture: "small",
            protocol: Protocol::smoke(),
            isolation: Isolation::Shared,
            headline: false,
            availability: Availability::Planned("not declared in the catalogue"),
            hook_event: None,
            session: Session::Established,
            delivery: Delivery::Distinct,
            spawn_probe: false,
            budget: None,
            work_unit: None,
            caveat: None,
            measurement_owed: None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::leg::LegId;
    use crate::scenario::tests::*;
    use crate::scenario::*;

    /// The ids are a contract, not a label: the measurement, the artifacts it
    /// wrote and this catalogue all name these rows by string, and a rename
    /// that only looked cosmetic would break several readers silently. Both
    /// spellings — serde's kebab-case and `as_str` — are checked, because the
    /// artifact carries the first and every other reader the second.
    #[test]
    fn the_shadow_worktree_rows_are_spelled_as_every_reader_reads_them() {
        let spelled: Vec<&str> = WORKTREE_ROWS.iter().map(|id| id.as_str()).collect();
        assert_eq!(
            spelled,
            [
                "worktree-add-adopt",
                "member-switch",
                "recall-known-state",
                "recall-compounding"
            ]
        );
        for id in WORKTREE_ROWS {
            assert_eq!(
                serde_json::to_string(&id).unwrap(),
                format!("\"{}\"", id.as_str()),
                "the artifact spells {} differently from every other reader",
                id.as_str()
            );
            let spec = spec(id);
            assert!(spec.is_wired(), "{} is declared as driven", id.as_str());
            assert_eq!(
                spec.measurement_owed,
                None,
                "{} has been measured; the owed state is retired \
                 and must never come back without a new unmeasured declaration",
                id.as_str()
            );
            assert!(!spec.headline, "{} joins no headline set", id.as_str());
            assert!(!spec.spawn_probe, "{} counts no spawns", id.as_str());
        }
    }

    #[test]
    fn the_catalogue_covers_every_id_exactly_once() {
        let cat = catalogue();
        let mut ids: Vec<_> = cat.iter().map(|s| s.id).collect();
        ids.sort_unstable();
        let mut all = ScenarioId::all().to_vec();
        all.sort_unstable();
        assert_eq!(ids, all);
        for id in ScenarioId::all() {
            assert_eq!(spec(id).id, id);
            assert_eq!(ScenarioId::parse(id.as_str()), Some(id));
        }
        assert_eq!(ScenarioId::parse("nope"), None);
    }

    #[test]
    fn the_headline_set_is_the_one_the_plan_names() {
        let headline: Vec<_> = catalogue()
            .into_iter()
            .filter(|s| s.headline)
            .map(|s| s.id)
            .collect();
        assert_eq!(
            headline,
            vec![
                ScenarioId::HookRoundTrip,
                ScenarioId::ColdStatus,
                ScenarioId::Condense,
                ScenarioId::ImportThroughput,
            ]
        );
    }

    /// The Planned set is EMPTY (2026-08-29): every row the plan declares is
    /// driven end to end on the rift leg. A row that regresses to `Planned`
    /// — a verb removed, a prelude that stopped working and was "parked" —
    /// must red here rather than fall out of the measured set quietly; the
    /// `Planned` state itself stays representable (and stays tested in the
    /// driver) for the next row that is declared before it can be driven.
    #[test]
    fn every_catalogue_row_is_wired() {
        let planned: Vec<&str> = catalogue()
            .iter()
            .filter(|s| !s.is_wired())
            .map(|s| s.id.as_str())
            .collect();
        assert!(planned.is_empty(), "declared but not driven: {planned:?}");
        for s in catalogue() {
            assert!(
                matches!(plan(s.id, &probe(LegId::Rift)), Plan::Run(_)),
                "{} has no rift invocation",
                s.id.as_str()
            );
        }
    }

    #[test]
    fn cold_measures_get_a_fresh_copy_per_iteration() {
        assert_eq!(spec(ScenarioId::ColdStatus).isolation, Isolation::FreshCopy);
        assert_eq!(spec(ScenarioId::Condense).isolation, Isolation::FreshCopy);
        assert_eq!(
            spec(ScenarioId::CheckpointList).isolation,
            Isolation::Shared
        );
    }

    #[test]
    fn every_hook_verb_in_the_catalogue_is_one_claude_code_publishes() {
        // The adapter's surface at `crates/rift-agent-claude-code`: a verb the
        // agent never sends is a measure of nothing.
        let known = [
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "Stop",
            "SessionEnd",
        ];
        let hooks = ScenarioId::hooks();
        assert_eq!(hooks.len(), 11, "eleven hook rows: {hooks:?}");
        for id in hooks {
            let s = spec(id);
            let event = s.hook_event.unwrap();
            assert!(known.contains(&event.verb()), "{}", event.verb());
            assert_eq!(s.budget, Some(Budget::hook()), "{}", id.as_str());
            // Every row that is supposed to write must prove it did; the one
            // row that is supposed to write nothing must not be asked to.
            assert_eq!(
                s.requires_ingest(),
                id != ScenarioId::HookReplay,
                "{}",
                id.as_str()
            );
            // p95 must be a percentile, not a synonym for the maximum.
            assert!(s.protocol.iterations >= 20, "{}", id.as_str());
        }
    }

    #[test]
    fn a_warm_hook_row_gets_its_session_established_during_warmup() {
        // Nothing else creates it: the established session's first hook is
        // iteration 0, which must be a discarded warmup or the session-creation
        // cost lands inside the published samples.
        for id in ScenarioId::hooks() {
            let s = spec(id);
            if s.session == Session::Established {
                assert!(s.protocol.warmup >= 1, "{}", id.as_str());
            }
        }
    }

    #[test]
    fn once_in_a_lifetime_verbs_get_a_session_of_their_own_each_iteration() {
        for (id, session) in [
            (ScenarioId::HookSessionStart, Session::PerIteration),
            (ScenarioId::HookSessionEnd, Session::PerIteration),
            (ScenarioId::HookStop, Session::Established),
            (ScenarioId::HookRoundTrip, Session::Established),
            // Not once in a lifetime but once per KEY: a window that has
            // declined a read is not asked about it again, so a shared
            // session would time the allow path.
            (ScenarioId::HookReadGate, Session::PerIteration),
        ] {
            assert_eq!(spec(id).session, session, "{}", id.as_str());
        }
        let mut seen = std::collections::BTreeSet::new();
        for iteration in 0..40 {
            let c = PlanCtx {
                iteration,
                ..probe(LegId::Rift)
            };
            let s = c.session_for(&spec(ScenarioId::HookSessionStart));
            assert_eq!(s.len(), BENCH_SESSION.len(), "{s}");
            assert!(seen.insert(s));
            assert_eq!(
                c.session_for(&spec(ScenarioId::HookRoundTrip)),
                BENCH_SESSION
            );
        }
        assert_eq!(seen.len(), 40);
    }

    #[test]
    fn only_the_identity_row_pays_for_a_spawn_count() {
        let identity = spec(ScenarioId::IdentityAttribution);
        let session_start = spec(ScenarioId::HookSessionStart);
        // Same verb, same session shape, same delivery: the two rows differ in
        // exactly one thing, which is what they publish.
        assert_eq!(identity.hook_event, session_start.hook_event);
        assert_eq!(identity.session, Session::PerIteration);
        assert_eq!(identity.delivery, Delivery::Distinct);
        assert!(
            identity.requires_ingest(),
            "a count off a no-op counts nothing"
        );
        assert!(identity.spawn_probe);
        assert!(
            !identity.headline,
            "the headline hook number is the round-trip"
        );

        // Nobody else takes the pass: it costs an extra invocation and its
        // shims perturb what they touch.
        for id in ScenarioId::all() {
            assert_eq!(
                spec(id).spawn_probe,
                id == ScenarioId::IdentityAttribution,
                "{}",
                id.as_str()
            );
        }

        // A session's first hook follows nothing, so this row has no prelude
        // and owes exactly one durable event per iteration.
        assert!(
            prelude(ScenarioId::IdentityAttribution, &probe(LegId::Rift))
                .invocations()
                .is_empty()
        );
        assert_eq!(
            durable_events_per_iteration(ScenarioId::IdentityAttribution),
            1
        );
    }

    #[test]
    fn the_replay_row_is_the_same_event_and_is_never_the_round_trip() {
        let replay = spec(ScenarioId::HookReplay);
        let round_trip = spec(ScenarioId::HookRoundTrip);
        // Same verb, same session, same fixture: the only difference is whether
        // the event is new — which is the whole point of the pair.
        assert_eq!(replay.hook_event, round_trip.hook_event);
        assert_eq!(replay.session, round_trip.session);
        assert_eq!(replay.delivery, Delivery::Replay);
        assert!(!replay.requires_ingest());
        assert!(round_trip.requires_ingest());
        assert!(!replay.headline, "a no-op is not the headline hook number");
        // And it is still held to the hook budget: replay is a real hook path.
        assert_eq!(replay.budget, Some(Budget::hook()));
    }

    /// The import row, wired: driven over the frozen reconstruction, counted by
    /// the product's own `importable`, and carrying the reconstruction caveat
    /// where a JSON consumer meets it.
    #[test]
    fn the_import_row_is_driven_and_carries_the_reconstruction_caveat() {
        let s = spec(ScenarioId::ImportThroughput);
        assert!(s.is_wired());
        assert_eq!(s.work_unit, Some("shards"));
        assert_eq!(
            s.isolation,
            Isolation::FreshCopy,
            "an import needs a cold repo"
        );
        assert_eq!(s.budget, Some(Budget::unratified_throughput()));
        let caveat = s.caveat.expect("the row states what its number is about");
        assert_eq!(caveat, IMPORT_RECONSTRUCTION_CAVEAT);
        assert!(caveat.contains("Nothing here was fetched"));
        assert!(caveat.contains("never a format-compatibility claim"));

        let ctx = probe(LegId::Rift);
        let Plan::Run(inv) = plan(ScenarioId::ImportThroughput, &ctx) else {
            panic!("wired")
        };
        // The flag names the FORMAT, never a tool, by owner decision.
        assert_eq!(inv.args, ["import", "--from-legacy"]);
        assert!(!inv.args.iter().any(|a| a.contains("reference")));

        // The denominator is the product's own count, not the corpus size the
        // harness planted.
        assert_eq!(
            parse_work_units(
                ScenarioId::ImportThroughput,
                "importable 64 · refused 0 · events 3"
            ),
            Some(64)
        );
        assert_eq!(
            parse_work_units(
                ScenarioId::ImportThroughput,
                "importable 0 · refused 64 · events 0"
            ),
            Some(0),
            "a refused corpus collapses the rate rather than propping it up"
        );
        let counter =
            work_probe(ScenarioId::ImportThroughput, &ctx).expect("the verb counts itself");
        assert_eq!(counter.args, ["import", "--from-legacy"]);

        // The corpus is planted untimed, in this iteration's own fresh copy.
        let pre = prelude(ScenarioId::ImportThroughput, &ctx);
        let invocations = pre.invocations();
        let plant = invocations.first().expect("the corpus is planted");
        assert_eq!(plant.args, ["fast-import", "--quiet", "--done"]);
        assert!(plant
            .stdin
            .as_deref()
            .is_some_and(|s| s.ends_with("\ndone\n")));
        assert!(pre.files().is_empty());
        assert_eq!(pre.hook_invocations(), 0, "a corpus plant is not a hook");

        let Plan::NoAnalogue(note) = plan(ScenarioId::ImportThroughput, &probe(LegId::Reference))
        else {
            panic!("the retired reference implementation read no import of its own format")
        };
        assert!(note.contains("rift-only"));
    }
}
