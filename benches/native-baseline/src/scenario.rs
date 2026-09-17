//! The scenario catalogue: what the plan measures, and how each leg is asked.
//!
//! Three outcomes are kept strictly distinct, because collapsing them is how a
//! benchmark starts flattering its author:
//!
//! * a scenario is **wired** (driven end to end today) or **planned** (declared,
//!   with the work it waits on named);
//! * a leg **has no analogue** for a verb — recorded as such, never as a loss for
//!   that leg. `rift verify` is the standing example: Reference ships no integrity
//!   check at all, so integrity-at-speed is a rift-only measure, reported as one;
//! * a leg is **absent** (see [`crate::leg`]) — the comparison is skipped, and the
//!   present leg's numbers are still collected.
//!
//! # The hook rows
//!
//! A hook verb costs what it costs *in the session state it really meets*. A
//! `Stop` fired at a session that never opened a turn closes nothing, snapshots
//! no boundary and drives no post-ingest strategy — timing that would produce a
//! flattering number for work that never happened. Every hook scenario therefore
//! declares a [`Prelude`]: untimed invocations that put the session where the
//! timed verb finds it in life, plus files (a transcript) written last, so they
//! are as fresh at the timed verb as a live agent's would be.
//!
//! Each verb gets its own row rather than one blended "hook" figure, because the
//! costs are not alike: the session's *first* hook pays identity binding, a turn
//! boundary pays a worktree snapshot, a read-shaped tool call pays worktree
//! hashing plus tokenization, and `Stop` closes a turn and *leaves the
//! transcript for the next hook* — spend is settled off the Stop, from a
//! recorded offset, by the next hook for the session (the deferred-settle
//! contract). A single average would hide all four.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    leg::LegId,
    measure::{Budget, Invocation, Protocol},
};

/// A representative Claude Code hook payload in the native shape rift's wire
/// layer accepts (`session_id` + `hook_event_name`, no envelope).
///
/// The first event of the shared session, spelled out so the shape a hook row
/// really sends is readable without running one; [`HookEvent::ToolCallBash`]
/// reproduces it, pinned by test.
pub const HOOK_PAYLOAD: &str = concat!(
    r#"{"session_id":"01JBENCH000000000000000000","hook_event_name":"PreToolUse","#,
    r#""tool_name":"Bash","tool_input":{"command":"cargo test --workspace case_0"}}"#
);

/// The established session the warm hook rows share: 26 characters, ULID-shaped,
/// byte-stable across runs.
pub const BENCH_SESSION: &str = "01JBENCH000000000000000000";

/// The capture opt-in control file (`rift_shadow::inclusion::INCLUDE_FILE`).
///
/// Spelled once here rather than at each use: the harness drives the product
/// as a black box, so a rename in the product must break this constant's one
/// definition and not three string literals in three arms.
pub const INCLUDE_FILE: &str = ".worktreeinclude";

/// The fixture every hook row runs against: hot-path latency, where repository
/// size must not dominate.
pub const HOOK_FIXTURE: &str = "small";

/// The branch every generated fixture is on (`init.defaultBranch` in the
/// hermetic git config), which is the branch `session resume` is asked for.
pub const FIXTURE_BRANCH: &str = "main";

/// Capture's opt-in for an inline archive: with it set, the post-commit hook
/// condenses in-process instead of detaching a worker, so a prelude's commit
/// has landed its checkpoint shard by the time the timed verb runs. The env
/// name is the product's (`bins/rift/src/hooks.rs`), spelled once here.
pub const CAPTURE_SYNC_ENV: &str = "RIFT_CAPTURE_SYNC";

/// A distinct session per iteration, for verbs that can only happen once in a
/// session's life — its first hook, its last.
#[must_use]
pub fn iteration_session(iteration: usize) -> String {
    // 8 + 18 = the 26 characters a ULID-shaped id has.
    format!("01JBENCH{:018}", iteration % 1_000_000_000_000_000_000)
}

/// One scripted hook event: the verb a leg is invoked with, plus the payload
/// body the agent would have sent with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    /// The session's first hook.
    SessionStart,
    /// A user prompt — opens a turn.
    PromptSubmit,
    /// `PreToolUse` for a shell command: no worktree read.
    ToolCallBash,
    /// `PreToolUse` for a file read: worktree hashing plus tokenization.
    ToolCallRead,
    /// `PostToolUse` — the tool's result.
    ToolResult,
    /// `PostToolUse` for a file read: the event the read observation lands on.
    ///
    /// The bridge records a `RecordedRead` only from `EventKind::ToolResult`
    /// (`rift-shadow/src/bridge.rs`), so a prelude that wants a serve to find
    /// a sibling's read must deliver the read's *result*, not just its
    /// `PreToolUse` — which is how Claude Code sends it, and how the landed
    /// compounding fixture (`bins/rift/tests/family_compounding.rs::agent_read`)
    /// delivers it.
    ToolReadResult,
    /// End of turn.
    Stop,
    /// End of session.
    SessionEnd,
    /// `PreToolUse` for a file read named **absolutely**, which is the only
    /// spelling the read ledger stands behind without a stated working
    /// directory — and therefore the only one the gate decides about.
    ///
    /// Delivered first, so nothing about it is a repeat.
    ToolCallReadAbs,
    /// The same read again, in a payload of its own.
    ///
    /// This is the call the gate really pays for: the window already holds
    /// these lines at this revision, so the whole decision runs — hash,
    /// ledger, predicate, the reason the model is answered with, and the
    /// accounting journalled for the detached worker. The payload differs
    /// from [`Self::ToolCallReadAbs`] in the field a live session varies, so
    /// the two are two events rather than a replay of one.
    ToolCallReadRepeat,
    /// `PostToolUse` for the absolute read: the result the projection records
    /// a read fact from.
    ToolReadAbsResult,
    /// `PostToolUse` for an edit: the file is hashed and what the window now
    /// holds is worked out from the agent's own payload.
    ToolEditRecord,
}

/// The file every edit row changes, and the body it is left holding.
///
/// One path, rewritten per iteration rather than one path per iteration: a
/// row under `Isolation::Shared` that planted a new file each time would be
/// timing a bigger work tree at iteration thirty than at iteration one.
pub const EDIT_TARGET: &str = "bench/edit_target.txt";

/// What [`EDIT_TARGET`] holds after the `nth` edit.
#[must_use]
pub fn edit_body(nth: usize) -> String {
    format!("alpha\nedit {nth}\nomega\n")
}

/// The policy file a repository writes to ask for declines, and the tables it
/// asks with — the operator's own spelling, so what the row measures is the
/// route from a file on disk to a decision on stdout.
///
/// The line floor is written at one on purpose. The `small` fixture's files
/// are forty-one lines today, comfortably over the shipped floor, so the row
/// would decline either way — but this row times the decline path and its
/// probe fails the measure on anything but a decline, and neither the fixture
/// generator's line count nor the shipped floor is this row's to depend on.
/// Pinning the floor here keeps the row timing the arm it is named for however
/// those two numbers move.
pub const ENFORCE_POLICY: &str = "[interception]\nmode = \"enforce\"\nmin_slice_lines = 1\n\n\
     [interception.agents]\nclaude-code = \"enforce\"\n";

/// Where that file lives in a work tree.
#[must_use]
pub fn serve_policy_path(repo: &Path) -> PathBuf {
    repo.join(".rift").join("policies").join("serve.toml")
}

impl HookEvent {
    /// Every scripted event, for the tests that quantify over the set.
    #[cfg(test)]
    pub(crate) const fn all_for_test() -> [Self; 12] {
        [
            Self::SessionStart,
            Self::PromptSubmit,
            Self::ToolCallBash,
            Self::ToolCallRead,
            Self::ToolResult,
            Self::ToolReadResult,
            Self::Stop,
            Self::SessionEnd,
            Self::ToolCallReadAbs,
            Self::ToolCallReadRepeat,
            Self::ToolReadAbsResult,
            Self::ToolEditRecord,
        ]
    }

    /// The hook verb passed on the command line.
    #[must_use]
    pub const fn verb(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::PromptSubmit => "UserPromptSubmit",
            Self::ToolCallBash
            | Self::ToolCallRead
            | Self::ToolCallReadAbs
            | Self::ToolCallReadRepeat => "PreToolUse",
            Self::ToolResult
            | Self::ToolReadResult
            | Self::ToolReadAbsResult
            | Self::ToolEditRecord => "PostToolUse",
            Self::Stop => "Stop",
            Self::SessionEnd => "SessionEnd",
        }
    }

    /// Whether this verb's payload names the transcript. Every verb does: the
    /// session's first hook seeds the usage-settle offset at the transcript's
    /// current length, a mid-turn hook samples context from its tail, and any
    /// later hook may settle the turn that closed before it — so a payload
    /// without the address would measure a hook that cannot do its work.
    #[must_use]
    pub const fn carries_transcript(self) -> bool {
        let _ = self;
        true
    }

    /// The JSON body an agent would put on stdin for the `nth` such event.
    ///
    /// `nth` is what makes consecutive events **distinct**, and that is not
    /// cosmetic. A canonical event's identity is
    /// `sha256(agent ‖ session ‖ verb ‖ turn ‖ ordinal ‖ payload)` — deliberately
    /// clock-free, so re-delivering a byte-identical payload in the same turn is
    /// a *replay* and ingest is a true no-op. Timing that would measure the
    /// dedup short-circuit while calling it a hook round-trip. Each verb varies
    /// in the field a live session varies: the prompt text, the command, the
    /// file being read.
    #[must_use]
    pub fn payload(
        self,
        repo: &Path,
        session: &str,
        nth: usize,
        transcript: Option<&Path>,
    ) -> String {
        let mut body = serde_json::Map::new();
        let mut put = |k: &str, v: serde_json::Value| {
            body.insert(k.to_owned(), v);
        };
        put("session_id", session.into());
        put("hook_event_name", self.verb().into());
        match self {
            Self::SessionStart => put("source", "startup".into()),
            Self::PromptSubmit => put(
                "prompt",
                format!("add test case {nth} for the parser").into(),
            ),
            Self::ToolCallBash | Self::ToolResult => {
                put("tool_name", "Bash".into());
                put(
                    "tool_input",
                    serde_json::json!({ "command": format!("cargo test --workspace case_{nth}") }),
                );
                if self == Self::ToolResult {
                    put(
                        "tool_response",
                        serde_json::json!({ "stdout": "test result: ok. 12 passed" }),
                    );
                }
            }
            Self::ToolCallRead | Self::ToolReadResult => {
                put("tool_name", "Read".into());
                // A live session reads different files, and pages within them:
                // both are real `Read` parameters, and together they keep every
                // iteration a genuinely new event.
                put(
                    "tool_input",
                    serde_json::json!({ "file_path": read_target(nth), "offset": nth }),
                );
                if self == Self::ToolReadResult {
                    put(
                        "tool_response",
                        serde_json::json!({ "file": { "filePath": read_target(nth) } }),
                    );
                }
            }
            Self::Stop => put("stop_hook_active", false.into()),
            Self::SessionEnd => put("reason", "clear".into()),
            Self::ToolCallReadAbs | Self::ToolCallReadRepeat | Self::ToolReadAbsResult => {
                let file = repo.join(read_target(nth)).display().to_string();
                put("tool_name", "Read".into());
                // Both ask for the whole file — every `limit` here is past
                // any fixture file's length, and a read is clamped to the
                // lines the file has — so the two resolve to the same run of
                // the same file, which is what makes the second one a repeat.
                // They differ in the field itself, and each varies with the
                // iteration, so no two calls in a run are the same event.
                let limit = match self {
                    Self::ToolCallReadRepeat => 200_000 + nth,
                    _ => 100_000 + nth,
                };
                put(
                    "tool_input",
                    serde_json::json!({ "file_path": file, "offset": 1, "limit": limit }),
                );
                if self == Self::ToolReadAbsResult {
                    put(
                        "tool_response",
                        serde_json::json!({ "file": { "filePath": repo.join(read_target(nth)).display().to_string() } }),
                    );
                }
            }
            Self::ToolEditRecord => {
                put("tool_name", "Edit".into());
                put(
                    "tool_input",
                    serde_json::json!({
                        "file_path": repo.join(EDIT_TARGET).display().to_string(),
                        "old_string": "alpha\n",
                        "new_string": format!("alpha\nedit {nth}\n"),
                        "replace_all": false,
                    }),
                );
                put(
                    "tool_response",
                    serde_json::json!({ "filePath": repo.join(EDIT_TARGET).display().to_string() }),
                );
            }
        }
        if let (Some(path), true) = (transcript, self.carries_transcript()) {
            put("transcript_path", path.display().to_string().into());
        }
        serde_json::Value::Object(body).to_string()
    }

    /// The invocation that delivers the `nth` such event.
    #[must_use]
    pub fn invocation(self, ctx: &PlanCtx<'_>, session: &str, nth: usize) -> Invocation {
        Invocation::new(ctx.binary, ctx.repo)
            .args(["hooks", "claude-code", self.verb()])
            .stdin(self.payload(ctx.repo, session, nth, Some(&ctx.transcript_path(session))))
    }
}

/// The repo-relative file the `nth` read-shaped tool call names: the generated
/// fixtures' own files, cycled, so the path always resolves and the read path
/// really hashes a blob.
#[must_use]
pub fn read_target(nth: usize) -> String {
    let spec = crate::fixture::FixtureSpec::preset(HOOK_FIXTURE)
        .unwrap_or_else(crate::fixture::FixtureSpec::small);
    let total = spec.total_files().max(1);
    crate::fixture::generated_path(&spec, nth % total)
        .display()
        .to_string()
}

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
    /// The shared [`BENCH_SESSION`], established before the first measured
    /// iteration — the warm path an agent spends a session in.
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
            "a read the gate DECLINES (`PreToolUse` / `Read`, the lines already held, this repository asking for declines): hash, ledger, predicate, the reason the model is answered with, and the accounting journaled for the detached worker",
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
    use Delivery::{Distinct, Replay};
    use HookEvent::{
        PromptSubmit, SessionEnd, SessionStart, Stop, ToolCallBash, ToolCallRead, ToolResult,
    };
    use Session::{Established, PerIteration};

    let [read_gate, edit_record] = gate_rows();

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
        read_gate,
        edit_record,
        // Same verb as `hook-session-start`, different claim. That row
        // publishes the latency of a session's first hook; this one publishes
        // what the latency is *made of* — the child processes first-touch
        // identity binding launches. They are separate rows because the count
        // is taken through PATH shims that cost an exec per spawn: folding the
        // probe into the latency row would either perturb the published timing
        // or hide the count behind it.
        ScenarioSpec {
            spawn_probe: true,
            ..hook(
                ScenarioId::IdentityAttribution,
                "first-touch identity binding: child processes per session's first hook (the machine id is read from the salted cache once the install has derived it)",
                SessionStart,
                PerIteration,
                Distinct,
                false,
            )
        },
    ]
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

/// Everything the plan measures that is not a hook verb.
fn plan_rows() -> [ScenarioSpec; 7] {
    use Availability::Wired;
    use Isolation::{FreshCopy, Shared};

    let other = other_row;
    [
        other(
            ScenarioId::ColdStatus,
            "cold `status` (fresh copy per iteration)",
            "small",
            Protocol::latency(),
            FreshCopy,
            true,
            Wired,
        ),
        other(
            ScenarioId::CheckpointList,
            "`checkpoint list` over recorded checkpoints",
            "small",
            Protocol::latency(),
            Shared,
            false,
            Wired,
        ),
        // Wired 2026-08-19: the blocker was the scripted-session driver, and
        // `vogon` landed on main at 54053a8. One measured iteration plays a
        // whole session — session, turns, tool pairs, a subagent fan-out, a
        // compaction — and then commits, so the post-commit condense path meets
        // a live session rather than an empty one.
        ScenarioSpec {
            work_unit: Some("events"),
            budget: Some(Budget::unratified_throughput()),
            ..other(
                ScenarioId::Condense,
                "condensation at commit: one scripted session (3 turns, 25 hooks) then a commit",
                "medium",
                Protocol::heavy(),
                FreshCopy,
                true,
                Wired,
            )
        },
        // Wired 2026-08-29, once `session resume` shipped. One
        // iteration plays a whole session on a session id of its own, commits
        // with the repository's own hooks so the checkpoint shard lands
        // inline, then times the resume of that branch — checkout (a no-op
        // on the current branch), trailer walk, shard read, transcript
        // restore. The answer probe re-runs it untimed and requires the
        // transcript to have been `restored`: a resume that found nothing to
        // restore is not a resume.
        other(
            ScenarioId::ResumeRestore,
            "session resume: restore transcript + metadata from the branch's latest checkpoint",
            HOOK_FIXTURE,
            Protocol::latency(),
            Shared,
            false,
            Wired,
        ),
        // Wired 2026-08-24. The blocker recorded here was *"needs an reference/*
        // dialect fixture"*, and it was mis-stated rather than unresolved: no
        // such artifact was ever obtained and none is obtainable — the README
        // beside the golden shards says "Nothing here was fetched". What
        // exists is a frozen, byte-compared reconstruction, so the row is
        // driven over that and **carries the caveat in the artifact** — the
        // rate is about the reader, over shards this repository declared.
        ScenarioSpec {
            work_unit: Some("shards"),
            budget: Some(Budget::unratified_throughput()),
            caveat: Some(IMPORT_RECONSTRUCTION_CAVEAT),
            ..other(
                ScenarioId::ImportThroughput,
                "read a pre-Rift capture history: 64 reconstructed shards, walked and parsed",
                "small",
                Protocol::heavy(),
                FreshCopy,
                true,
                Wired,
            )
        },
        // Wired 2026-08-29, once `why` shipped. One iteration plays a
        // session that writes a file inside its turn and commits it through
        // the hooks (trailer + shard inline), then times `why <file>:1` —
        // line → commit → checkpoint → prompt. The answer probe re-runs it
        // untimed and requires `selection.checkpoint_id` to be a string: a
        // `why` that resolved no checkpoint answered nothing.
        other(
            ScenarioId::BlameWhy,
            "line → checkpoint → prompt provenance (`why <file>:1` on a line this iteration's session wrote)",
            HOOK_FIXTURE,
            Protocol::latency(),
            Shared,
            false,
            Wired,
        ),
        // The one wired throughput row today, and the row that made the
        // denominator's absence concrete: `rift verify` over a repository
        // holding no checkpoints exits fast and correctly, and its wall time is
        // a fine number about nothing. The work unit turns that from something
        // a reader has to know into something the report prints. The floor is
        // declared and **unratified** — no verify-throughput target exists
        // anywhere in this project either, so the verdict reads `OPEN` rather
        // than manufacturing a pass — the owner set no target.
        ScenarioSpec {
            work_unit: Some("checkpoints"),
            budget: Some(Budget::unratified_throughput()),
            ..other(
                ScenarioId::VerifyThroughput,
                "re-derive the hash chain and cross-check (rift-only)",
                "medium",
                Protocol::heavy(),
                Shared,
                false,
                Wired,
            )
        },
    ]
}

/// The shadow-worktree rows.
///
/// Declared by the change that wired the seventh git hook, measured by a later
/// quiet-machine sitting (2026-08-31): each carried `measurement_owed` until
/// that sitting ran — the owed state retired itself the moment its three clean
/// runs landed in the tracked tree.
fn worktree_rows() -> [ScenarioSpec; 4] {
    use Availability::Wired;
    use Isolation::{FreshCopy, Shared};

    let other = other_row;
    [
        // The first two are GIT latency, not agent latency: what the seventh
        // hook adds to the git operation a person typed. They are held to the
        // owner's hard git-latency bar and reported open-with-a-number if they
        // do not fit it — never asserted in a test.
        ScenarioSpec {
            budget: Some(Budget::git_latency()),
            measurement_owed: None,
            ..other(
                ScenarioId::WorktreeAddAdopt,
                "`git worktree add` in a family with an agent enabled: the checkout that pays \
                 full adoption",
                HOOK_FIXTURE,
                Protocol::latency(),
                FreshCopy,
                false,
                Wired,
            )
        },
        // The 99% case, and NOT the same shape as the row above: a member that
        // is already provisioned pays one rift spawn plus discovery and writes
        // nothing. A family with NO agent enabled is a THIRD shape — it never
        // writes the ownership manifest, so every switch re-reads the
        // projection read-only; that is inside the bar and deliberate, but it
        // is not what this row measures and must never be quoted for it.
        ScenarioSpec {
            budget: Some(Budget::git_latency()),
            measurement_owed: None,
            ..other(
                ScenarioId::MemberSwitch,
                "`git switch` in a member the previous checkout already provisioned: the fast path",
                HOOK_FIXTURE,
                Protocol::latency(),
                FreshCopy,
                false,
                Wired,
            )
        },
        // Branch containment made the repo/branch axes real, and a real axis
        // costs a walk: one capped, memoized first-parent-inclusive ancestry
        // walk per serve. That walk is budgeted at <= 20 ms p50 ADDED, which is
        // a delta this harness cannot state as a row's own ceiling — so the row
        // publishes the whole serve and the caveat names the comparison the
        // measurement makes.
        ScenarioSpec {
            caveat: Some(ANCESTRY_WALK_CAVEAT),
            measurement_owed: None,
            ..other(
                ScenarioId::RecallKnownState,
                "`recall known-state` over a repository with real ancestry: the walk containment \
                 added to a serve",
                HOOK_FIXTURE,
                Protocol::latency(),
                Shared,
                false,
                Wired,
            )
        },
        // The owner's requirement, made a number: a member is served what its
        // sibling read, out of the one store the family shares. The work unit
        // is the family's own accounting figure, so the row publishes what
        // compounded rather than only how long it took.
        ScenarioSpec {
            work_unit: Some("tokens-avoided"),
            budget: Some(Budget::unratified_throughput()),
            caveat: Some(COMPOUNDING_RATIO_CAVEAT),
            measurement_owed: None,
            ..other(
                ScenarioId::RecallCompounding,
                "a linked member is served the reads its sibling captured: the family's shared \
                 store, from a checkout that captured none of it",
                HOOK_FIXTURE,
                Protocol::heavy(),
                FreshCopy,
                false,
                Wired,
            )
        },
    ]
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

/// What one iteration of one leg is measured against.
#[derive(Debug, Clone, Copy)]
pub struct PlanCtx<'a> {
    /// Which leg.
    pub leg: LegId,
    /// Its resolved binary.
    pub binary: &'a Path,
    /// The repository copy this iteration runs in.
    pub repo: &'a Path,
    /// The leg's sandbox: everything that is not repository content.
    pub sandbox: &'a Path,
    /// Iteration index, warmup included.
    pub iteration: usize,
    /// The scripted-session driver (`vogon`), when it resolved.
    ///
    /// A driver, not a leg: it is never measured and never compared. `None`
    /// means the rows that need it record why, rather than silently measuring
    /// something else.
    pub tool: Option<&'a Path>,
}

impl PlanCtx<'_> {
    /// Where this session's transcript lives. Outside the repository, because a
    /// real transcript is not repository content — putting one in the worktree
    /// would change what every worktree hash and `status` sees.
    #[must_use]
    pub fn transcript_path(&self, session: &str) -> PathBuf {
        self.sandbox
            .join("transcripts")
            .join(format!("{session}.jsonl"))
    }

    /// The session the timed verb of `spec` is delivered to.
    #[must_use]
    pub fn session_for(&self, spec: &ScenarioSpec) -> String {
        match spec.session {
            Session::Established => BENCH_SESSION.to_owned(),
            Session::PerIteration => iteration_session(self.iteration),
        }
    }
}

/// What a leg does for a scenario.
#[derive(Debug, Clone)]
pub enum Plan {
    /// Run this invocation.
    Run(Box<Invocation>),
    /// This leg has no equivalent verb. Recorded, never scored.
    NoAnalogue(&'static str),
}

/// One untimed preparation step.
#[derive(Debug, Clone)]
pub enum Step {
    /// Run a process (a hook, a `git commit`, a corpus plant).
    Invoke(Invocation),
    /// Write a file (a transcript, a fixture file the turn edits).
    Write(PathBuf, String),
}

/// Untimed preparation for one iteration: **ordered** steps, run in the
/// order declared. Order is part of the workload — a transcript must exist
/// before the session start that names it and be written last for the verb
/// that reads it, a file must be written inside the turn that edits it and
/// committed after the turn closes — so the prelude is a list of steps, not
/// two lists with a fixed relative order.
#[derive(Debug, Clone, Default)]
pub struct Prelude {
    /// The steps, in execution order.
    pub steps: Vec<Step>,
}

impl Prelude {
    /// The invocations, in order.
    #[must_use]
    pub fn invocations(&self) -> Vec<&Invocation> {
        self.steps
            .iter()
            .filter_map(|s| match s {
                Step::Invoke(inv) => Some(inv),
                Step::Write(..) => None,
            })
            .collect()
    }

    /// The files written, in order.
    #[must_use]
    pub fn files(&self) -> Vec<(&Path, &str)> {
        self.steps
            .iter()
            .filter_map(|s| match s {
                Step::Write(path, body) => Some((path.as_path(), body.as_str())),
                Step::Invoke(_) => None,
            })
            .collect()
    }

    /// How many steps deliver an agent hook (`hooks <agent> <verb>`): the
    /// invocations that each owe one durable event. A `git commit` or a
    /// corpus plant is an invocation and not a hook, and is not counted.
    #[must_use]
    pub fn hook_invocations(&self) -> u64 {
        self.invocations()
            .iter()
            .filter(|inv| inv.args.first().is_some_and(|a| a == "hooks"))
            .count() as u64
    }
}

/// Turns the condense row's scripted session plays.
///
/// `vogon --turns N` emits **6N + 7** hooks: `SessionStart`, then per turn a
/// prompt, a `Read` pair, an `Edit` pair and a `Stop`, plus the subagent beat
/// (four) and the compaction beat (one) and `SessionEnd`. Three turns is 25
/// hooks — enough that a turn boundary, a subagent fan-out and a compaction all
/// land inside one measured iteration, and few enough that a heavy-protocol row
/// stays minutes rather than hours.
pub const CONDENSE_TURNS: u32 = 3;

/// Hooks one `--turns N` script fires.
#[must_use]
pub const fn condense_hooks(turns: u32) -> u32 {
    6 * turns + 7
}

/// The flags `vogon` gets, built **once** and shared by every mode.
///
/// **This is a trap with a name.** `--repo` changes the payload (it adds `cwd`
/// and makes tool paths absolute) and a canonical event's identity hashes the
/// payload — so a `script` invocation and a `drive` invocation given different
/// flags describe different workloads while looking identical, and they
/// disagree silently. One builder, one flag vector. `--commit` is the single
/// permitted difference: it is a drive-only *action* taken after the last turn
/// and it alters no payload.
#[must_use]
pub fn vogon_spec_flags(repo: &Path) -> Vec<String> {
    vec![
        "--agent".to_owned(),
        "claude-code".to_owned(),
        "--turns".to_owned(),
        CONDENSE_TURNS.to_string(),
        "--repo".to_owned(),
        repo.display().to_string(),
    ]
}

/// A synthetic Claude Code transcript tail: the offset window reader reads the
/// last assistant records for provider-reported usage, so the enrichment the
/// `Stop` and `PreToolUse` paths perform is actually exercised.
#[must_use]
pub fn transcript_body(turns: usize) -> String {
    let mut out = String::new();
    for turn in 0..turns.max(1) {
        out.push_str(
            &serde_json::json!({
                "type": "user",
                "message": { "role": "user", "content": "add a test for the parser" },
            })
            .to_string(),
        );
        out.push('\n');
        out.push_str(
            &serde_json::json!({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "model": "claude-opus-4",
                    "usage": {
                        "input_tokens": 1_200 + turn,
                        "output_tokens": 340,
                        "cache_read_input_tokens": 18_000,
                        "cache_creation_input_tokens": 900,
                    },
                },
            })
            .to_string(),
        );
        out.push('\n');
    }
    out
}

/// The four shadow-worktree rows' timed verbs, or `None` for every other row.
///
/// Lifted out of [`plan`] so each stays readable: two of these time `git` and
/// two time the product binary, and the two pairs have nothing else in common.
fn shadow_worktree_plan(id: ScenarioId, ctx: &PlanCtx<'_>) -> Option<Plan> {
    Some(match id {
        ScenarioId::WorktreeAddAdopt => match ctx.leg {
            LegId::Rift => Plan::Run(Box::new(git_at(ctx.repo).args([
                "worktree".to_owned(),
                "add".to_owned(),
                "--quiet".to_owned(),
                member_at(ctx).display().to_string(),
                "-b".to_owned(),
                member_branch(ctx.iteration),
            ]))),
            LegId::Reference => Plan::NoAnalogue(NO_MEMBER_ANALOGUE),
        },
        ScenarioId::MemberSwitch => match ctx.leg {
            LegId::Rift => Plan::Run(Box::new(git_at(member_at(ctx)).args([
                "switch".to_owned(),
                "--quiet".to_owned(),
                switch_branch(ctx.iteration),
            ]))),
            LegId::Reference => Plan::NoAnalogue(NO_MEMBER_ANALOGUE),
        },
        // Served from the main work tree: the walk is what is being timed, and
        // it costs the same wherever the caller stands.
        ScenarioId::RecallKnownState => match ctx.leg {
            LegId::Rift => Plan::Run(Box::new(serve_at(ctx, ctx.repo))),
            LegId::Reference => Plan::NoAnalogue(NO_RECALL_ANALOGUE),
        },
        // The product's own repeat-class report, over the session the prelude
        // just played: the residue is a number only the product can state,
        // because only it knows which reads were repeats of unchanged bytes.
        ScenarioId::RecallInterceptSavings => match ctx.leg {
            LegId::Rift => Plan::Run(Box::new(reread_report_at(ctx))),
            LegId::Reference => Plan::NoAnalogue(NO_RECALL_ANALOGUE),
        },
        // Served from the MEMBER, which is the whole claim: this checkout
        // captured none of the reads it is about to be handed.
        ScenarioId::RecallCompounding => match ctx.leg {
            LegId::Rift => Plan::Run(Box::new(serve_at(ctx, member_at(ctx)))),
            LegId::Reference => Plan::NoAnalogue(NO_RECALL_ANALOGUE),
        },
        _ => return None,
    })
}

/// Build the invocation `ctx.leg` uses for `id`.
///
/// Argv for the backend leg follows its documented surface at the pinned tag
/// (`status`, `checkpoint list`, `hooks <agent> <verb>`); it is re-verified when
/// the leg is actually activated, since an absent binary cannot be probed.
#[must_use]
pub fn plan(id: ScenarioId, ctx: &PlanCtx<'_>) -> Plan {
    let spec = spec(id);
    if let Some(event) = spec.hook_event {
        let nth = spec.nth_event(ctx.iteration);
        return Plan::Run(Box::new(event.invocation(
            ctx,
            &ctx.session_for(&spec),
            nth,
        )));
    }
    if let Some(plan) = shadow_worktree_plan(id, ctx) {
        return plan;
    }
    let inv = |args: &[&str]| {
        Plan::Run(Box::new(
            Invocation::new(ctx.binary, ctx.repo).args(args.to_vec()),
        ))
    };
    match id {
        ScenarioId::ColdStatus => inv(&["status"]),
        ScenarioId::CheckpointList => inv(&["checkpoint", "list"]),
        ScenarioId::Condense => match ctx.leg {
            LegId::Rift => {
                let Some(vogon) = ctx.tool else {
                    return Plan::NoAnalogue(
                        "the scripted-session driver did not resolve: build it with \
                         `cargo build --release -p rift-shadow-hooks --bin vogon` or set \
                         $RIFT_BENCH_VOGON_BIN",
                    );
                };
                let mut args = vec![
                    "drive".to_owned(),
                    "--rift".to_owned(),
                    ctx.binary.display().to_string(),
                ];
                args.extend(vogon_spec_flags(ctx.repo));
                // The one flag `script` does not get: an action, not a payload.
                args.push("--commit".to_owned());
                Plan::Run(Box::new(Invocation::new(vogon, ctx.repo).args(args)))
            }
            LegId::Reference => Plan::NoAnalogue(
                "the scripted driver speaks rift's `hooks <agent> <verb>` surface; it cannot \
                 drive the retired reference implementation, so this row is rift-only \
                 rather than a win over a measured competitor",
            ),
        },
        ScenarioId::ImportThroughput => match ctx.leg {
            // `--from-legacy` names the FORMAT, never the tool that wrote it
            // by owner decision, and the verb is read-only: it walks, parses and
            // re-derives roots, and writes nothing to the source repository.
            // That is exactly the work the rate is about.
            LegId::Rift => inv(&["import", "--from-legacy"]),
            LegId::Reference => Plan::NoAnalogue(
                "importing a pre-Rift history is a migration INTO rift; the retired \
                 reference implementation had no verb that read its own format back as an \
                 import, so this row is rift-only rather than a win over a measured \
                 competitor",
            ),
        },
        ScenarioId::ResumeRestore => match ctx.leg {
            // `--force` says yes where resume would ask (overwrite the local
            // transcript; a checkpoint behind the tip), which in a pipe is
            // the only answer that does not hang.
            LegId::Rift => inv(&["--json", "session", "resume", FIXTURE_BRANCH, "--force"]),
            LegId::Reference => Plan::NoAnalogue(
                "the retired reference implementation resumed by re-launching the agent \
                 against its shadow branch; it had no verb that restored a transcript from \
                 a checkpoint into place, so this row is rift-only rather than a win over a \
                 measured competitor",
            ),
        },
        ScenarioId::BlameWhy => match ctx.leg {
            LegId::Rift => inv(&["--json", "why", &why_target(spec.nth_event(ctx.iteration))]),
            LegId::Reference => Plan::NoAnalogue(
                "the retired reference implementation answered no line → checkpoint → \
                 prompt question; its checkpoints were shadow-branch snapshots with no \
                 per-line resolver, so this row is rift-only rather than a win over a \
                 measured competitor",
            ),
        },
        ScenarioId::VerifyThroughput => match ctx.leg {
            LegId::Rift => inv(&["verify"]),
            LegId::Reference => Plan::NoAnalogue(
                "the retired reference implementation shipped no integrity check (no hash \
                 chain, no Merkle roots) — integrity-at-speed is a rift-only measure, not a \
                 win over a measured competitor",
            ),
        },
        _ => Plan::NoAnalogue("scenario not wired yet"),
    }
}

/// Why the backend leg has no worktree row: it keys its state by checkout, so
/// "the same repository from a second path" is not a thing it can be asked.
const NO_MEMBER_ANALOGUE: &str =
    "the pinned backend has no repository-family notion: a second checkout is a second \
     repository to it, so a member's adoption and its fast path have no counterpart to compare \
     against. Rift-only, and reported as such rather than as a win.";

/// Why the backend leg has no recall row.
const NO_RECALL_ANALOGUE: &str =
    "the pinned backend serves no memory-delta plane, so there is no verb to time against this \
     one. Rift-only, and reported as such rather than as a win.";

/// A hermetic `git` in `cwd` — the one on `PATH`, as every other git step in
/// this crate uses.
fn git_at(cwd: impl Into<PathBuf>) -> Invocation {
    Invocation::new("git", cwd).env(CAPTURE_SYNC_ENV, "1")
}

/// The linked worktree this iteration's member rows create.
///
/// Under the **sandbox**, never inside the fixture: a work tree nested in
/// another work tree is a directory the outer repository's own `status` walks,
/// and every later iteration would be timing a bigger tree than the last.
fn member_at(ctx: &PlanCtx<'_>) -> PathBuf {
    ctx.sandbox
        .join("members")
        .join(format!("m{}", ctx.iteration))
}

/// The branch a member is created on.
fn member_branch(nth: usize) -> String {
    format!("bench-member-{nth}")
}

/// The branch [`ScenarioId::MemberSwitch`]'s timed `git switch` moves to.
fn switch_branch(nth: usize) -> String {
    format!("bench-switch-{nth}")
}

/// The repeat-class report the intercept row times and probes — the same
/// invocation for both, so the probe cannot be reading a cheaper verb than the
/// one the timing is about.
fn reread_report_at(ctx: &PlanCtx<'_>) -> Invocation {
    Invocation::new(ctx.binary, ctx.repo).args([
        "--json".to_owned(),
        "memory".to_owned(),
        "reread-report".to_owned(),
    ])
}

/// The serve both recall rows time, from `cwd`.
///
/// `--json` on the timed verb as well as on the work probe, so the two are the
/// same invocation and the probe cannot be measuring a cheaper one.
fn serve_at(ctx: &PlanCtx<'_>, cwd: impl Into<PathBuf>) -> Invocation {
    let nth = ctx.iteration;
    Invocation::new(ctx.binary, cwd).args([
        "recall".to_owned(),
        "known-state".to_owned(),
        "--json".to_owned(),
        "--task".to_owned(),
        format!("finish the parser change {nth}"),
        "--file".to_owned(),
        read_target(nth),
    ])
}

/// The untimed per-iteration preparation for `id`.
#[must_use]
pub fn prelude(id: ScenarioId, ctx: &PlanCtx<'_>) -> Prelude {
    use HookEvent::{PromptSubmit, SessionStart, Stop, ToolCallBash};
    let spec = spec(id);
    // The import row's untimed preparation is the corpus itself: one
    // `fast-import` child plants 64 reconstructed shards on the legacy
    // checkpoint branch of this iteration's own fresh copy. Outside the timed
    // region, like every other prelude — the rate is about reading them, not
    // about writing them. A missing frozen fixture yields no invocation here
    // and the row then measures a repository with nothing in it, which the work
    // probe reports as zero rather than as a fast import.
    if id == ScenarioId::ImportThroughput && ctx.leg == LegId::Rift {
        let plant = crate::legacy::plant(
            &crate::leg::default_repo_root(),
            ctx.repo,
            crate::legacy::SHARD_COUNT,
        );
        return Prelude {
            steps: plant.into_iter().map(Step::Invoke).collect(),
        };
    }
    if id == ScenarioId::ResumeRestore && ctx.leg == LegId::Rift {
        return resume_prelude(ctx, &spec);
    }
    if id == ScenarioId::BlameWhy && ctx.leg == LegId::Rift {
        return why_prelude(ctx, &spec);
    }
    if id == ScenarioId::WorktreeAddAdopt && ctx.leg == LegId::Rift {
        // An enabled agent is what makes the timed `git worktree add` the FULL
        // adoption shape: without one the member never gets an ownership
        // manifest, and the row would publish a third, cheaper shape under
        // this row's name.
        return Prelude {
            steps: vec![Step::Invoke(enable_agent(ctx))],
        };
    }
    if id == ScenarioId::MemberSwitch && ctx.leg == LegId::Rift {
        let nth = ctx.iteration;
        let member = member_at(ctx);
        // The member is created and adopted HERE, untimed: the timed verb is
        // the switch that follows an adoption, which is every switch after the
        // first and so the case a person actually pays.
        return Prelude {
            steps: vec![
                Step::Invoke(enable_agent(ctx)),
                Step::Invoke(git_at(ctx.repo).args([
                    "worktree".to_owned(),
                    "add".to_owned(),
                    "--quiet".to_owned(),
                    member.display().to_string(),
                    "-b".to_owned(),
                    member_branch(nth),
                ])),
                Step::Invoke(git_at(ctx.repo).args(["branch".to_owned(), switch_branch(nth)])),
            ],
        };
    }
    if id == ScenarioId::RecallKnownState && ctx.leg == LegId::Rift {
        return recall_prelude(ctx, &spec, false);
    }
    if id == ScenarioId::RecallCompounding && ctx.leg == LegId::Rift {
        return recall_prelude(ctx, &spec, true);
    }
    if id == ScenarioId::RecallInterceptSavings && ctx.leg == LegId::Rift {
        return intercept_prelude(ctx, &spec);
    }
    let Some(event) = spec.hook_event else {
        return Prelude::default();
    };
    let session = ctx.session_for(&spec);
    let events: &[HookEvent] = match event {
        // A session's first hook follows nothing by definition; a prompt or a
        // tool call happens many times inside an established session, so
        // iteration i is simply the next one.
        HookEvent::SessionStart
        | HookEvent::PromptSubmit
        | HookEvent::ToolCallBash
        | HookEvent::ToolCallRead => &[],
        // A result needs the call it is a result of.
        HookEvent::ToolResult => &[ToolCallBash],
        HookEvent::ToolReadResult => &[HookEvent::ToolCallRead],
        // A turn end needs a turn: prompt, tool call, result, then Stop.
        HookEvent::Stop => &[PromptSubmit, ToolCallBash, HookEvent::ToolResult],
        // A session end needs a whole session, and a new one each iteration.
        HookEvent::SessionEnd => &[SessionStart, PromptSubmit, ToolCallBash, Stop],
        // A first read of the very lines the timed repeat asks for again:
        // without it the timed call is a first read and pays none of the
        // decision this row exists to time.
        HookEvent::ToolCallReadRepeat => &[HookEvent::ToolCallReadAbs],
        // An absolute first read, a result of one, and an edit each stand
        // alone: nothing has to have happened before them for the step they
        // time to do its whole work.
        HookEvent::ToolCallReadAbs | HookEvent::ToolReadAbsResult | HookEvent::ToolEditRecord => {
            &[]
        }
    };
    let nth = spec.nth_event(ctx.iteration);
    let mut steps: Vec<Step> = Vec::new();
    // What the two gate rows need on disk before their verb runs: the policy
    // that asks for declines, and the file an edit is about to be recorded
    // against. Written rather than invoked, because neither is work the timed
    // verb should be charged for.
    if id == ScenarioId::HookReadGate {
        steps.push(Step::Write(
            serve_policy_path(ctx.repo),
            ENFORCE_POLICY.to_owned(),
        ));
    }
    if id == ScenarioId::HookEditRecord {
        steps.push(Step::Write(ctx.repo.join(EDIT_TARGET), edit_body(nth)));
    }
    steps.extend(
        events
            .iter()
            .map(|e| Step::Invoke(e.invocation(ctx, &session, nth))),
    );
    // Written last so its mtime is the timed verb's own present: a Stop
    // must not be measured reading a transcript that has been quiet for
    // seconds, and a prompt that settles the previous turn reads bytes
    // that were written moments ago.
    if event.carries_transcript() {
        steps.push(Step::Write(
            ctx.transcript_path(&session),
            transcript_body(8),
        ));
    }
    Prelude { steps }
}

/// One whole session on this iteration's own session id — transcript first
/// (the session start names it), then start / prompt / tool call / Stop.
fn session_steps(ctx: &PlanCtx<'_>, session: &str, nth: usize) -> Vec<Step> {
    use HookEvent::{PromptSubmit, SessionStart, Stop, ToolCallBash};
    let mut steps = vec![Step::Write(
        ctx.transcript_path(session),
        transcript_body(2),
    )];
    steps.extend(
        [SessionStart, PromptSubmit, ToolCallBash, Stop]
            .iter()
            .map(|e| Step::Invoke(e.invocation(ctx, session, nth))),
    );
    steps
}

/// Write `rel_path` in the repository and commit it **through the
/// repository's own hooks** with an inline archive, so the commit carries its
/// `Rift-Checkpoint` trailer and the checkpoint shard has landed when the
/// step returns. The prelude runs with hooks enabled (see
/// [`crate::hermetic::Hooks`]); `git` is the one on `PATH`, as the fixture
/// generator's is.
fn commit_steps(ctx: &PlanCtx<'_>, rel_path: &str, body: &str, message: &str) -> Vec<Step> {
    let git = |args: &[&str]| {
        Step::Invoke(
            Invocation::new("git", ctx.repo)
                .args(args.to_vec())
                .env(CAPTURE_SYNC_ENV, "1"),
        )
    };
    vec![
        Step::Write(ctx.repo.join(rel_path), body.to_owned()),
        git(&["add", "--", rel_path]),
        git(&["commit", "--quiet", "--message", message]),
    ]
}

/// The resume row's prelude: a session, a commit that condenses it (so the
/// branch's latest checkpoint is this iteration's), nothing else. The timed
/// verb then restores exactly that session.
fn resume_prelude(ctx: &PlanCtx<'_>, spec: &ScenarioSpec) -> Prelude {
    let session = ctx.session_for(spec);
    let nth = spec.nth_event(ctx.iteration);
    let mut steps = session_steps(ctx, &session, nth);
    steps.extend(commit_steps(
        ctx,
        &format!("bench/resume_{nth}.txt"),
        &format!("resume fixture {nth}\n"),
        &format!("bench: resume fixture {nth}"),
    ));
    Prelude { steps }
}

/// Wire this agent on, so the family has an integration to fan out to.
/// Idempotent: the verb rewrites what it owns and leaves everything else.
fn enable_agent(ctx: &PlanCtx<'_>) -> Invocation {
    Invocation::new(ctx.binary, ctx.repo).args(["enable", "--agent", "claude-code"])
}

/// Run these steps' hooks **inline**.
///
/// A non-hook row names no detached journal (`detached_journal`), so the
/// harness does not wait for one — which is correct for what these rows time
/// and wrong for what their preludes produce. Capturing inline is what keeps
/// the two consistent: the reads a serve is about to be measured over have
/// landed before the timer starts, rather than racing it from a worker.
fn inline(steps: Vec<Step>) -> Vec<Step> {
    steps
        .into_iter()
        .map(|step| match step {
            Step::Invoke(inv) => Step::Invoke(inv.env(CAPTURE_SYNC_ENV, "1")),
            write @ Step::Write(..) => write,
        })
        .collect()
}

/// The preparation both recall rows share: recall on, one whole session whose
/// turn READS a file, and a commit that archives the checkpoint the reads hang
/// from — all in the **main work tree**.
///
/// With `member`, one linked worktree is cut from the tip afterwards, so its
/// `HEAD` is the checkpoint's own anchor commit: the member is on the family's
/// line and the serve it is about to be given compounds a sibling's reads
/// as branch containment requires. Cut last, deliberately — a member created before the
/// commit would sit behind the anchor and containment would rightly hold the
/// whole thing back, which is a different row's claim.
fn recall_prelude(ctx: &PlanCtx<'_>, spec: &ScenarioSpec, member: bool) -> Prelude {
    use HookEvent::{PromptSubmit, SessionStart, Stop, ToolCallRead, ToolReadResult};
    let session = ctx.session_for(spec);
    let nth = spec.nth_event(ctx.iteration);
    // Recall is off by default and `setup_plan` keeps it off (`--no-recall`),
    // so a row that serves has to turn it on. Idempotent.
    let mut steps = vec![Step::Invoke(
        Invocation::new(ctx.binary, ctx.repo).args(["recall", "init"]),
    )];
    // Stage the policy file `recall init` wrote, so the fixture commit below
    // carries it and a member cut from the tip inherits recall ON by checking
    // it out — the tracked-policy convention the product reads per worktree
    // (`EffectiveConfig::load(repo/.rift)`) and the compounding fixture
    // states outright ("the policy every member inherits by checking it
    // out"). Without this the member serve exits `recall_disabled` and the
    // compounding row measures a refusal. Idempotent under Shared isolation.
    steps.push(Step::Invoke(
        Invocation::new("git", ctx.repo)
            .args(["add", "--", ".rift/config.toml"])
            .env(CAPTURE_SYNC_ENV, "1"),
    ));
    steps.push(Step::Write(
        ctx.transcript_path(&session),
        transcript_body(2),
    ));
    steps.extend(inline(
        [
            SessionStart,
            PromptSubmit,
            ToolCallRead,
            ToolReadResult,
            Stop,
        ]
        .iter()
        .map(|e| Step::Invoke(e.invocation(ctx, &session, nth)))
        .collect(),
    ));
    steps.extend(commit_steps(
        ctx,
        &format!("bench/recall_{nth}.txt"),
        &format!("recall fixture {nth}\n"),
        &format!("bench: recall fixture {nth}"),
    ));
    if member {
        steps.push(Step::Invoke(git_at(ctx.repo).args([
            "worktree".to_owned(),
            "add".to_owned(),
            "--quiet".to_owned(),
            member_at(ctx).display().to_string(),
            "-b".to_owned(),
            member_branch(nth),
        ])));
    }
    Prelude { steps }
}

/// How many files the intercept row's scripted session reads and then re-reads.
///
/// Four rather than one, so the residue is a sum over a session rather than a
/// statement about a single file, and few enough that a heavy-protocol row
/// stays minutes.
pub const INTERCEPT_FILES: usize = 4;

/// The intercept row's preparation: recall on, this repository asking for
/// declines, and one whole session that reads four files, is handed each of
/// them, then asks for every one of them again.
///
/// The repeats are delivered as the calls they are (`PreToolUse`) and their
/// results are deliberately NOT delivered, because a declined call has no
/// result — that is the shape of the saving, and a script that reported one
/// anyway would put the pool back into the projection by hand.
fn intercept_prelude(ctx: &PlanCtx<'_>, spec: &ScenarioSpec) -> Prelude {
    use HookEvent::{
        PromptSubmit, SessionStart, Stop, ToolCallReadAbs, ToolCallReadRepeat, ToolReadAbsResult,
    };
    let session = ctx.session_for(spec);
    let nth = spec.nth_event(ctx.iteration);
    let mut steps = vec![
        Step::Invoke(Invocation::new(ctx.binary, ctx.repo).args(["recall", "init"])),
        Step::Write(serve_policy_path(ctx.repo), ENFORCE_POLICY.to_owned()),
        Step::Write(ctx.transcript_path(&session), transcript_body(2)),
    ];
    let mut beats: Vec<Invocation> = vec![
        SessionStart.invocation(ctx, &session, nth),
        PromptSubmit.invocation(ctx, &session, nth),
    ];
    // The reads, each with the result that makes it a fact the projection
    // classes; then the repeats, which is the pool.
    for k in 0..INTERCEPT_FILES {
        let file = nth * INTERCEPT_FILES + k;
        beats.push(ToolCallReadAbs.invocation(ctx, &session, file));
        beats.push(ToolReadAbsResult.invocation(ctx, &session, file));
    }
    for k in 0..INTERCEPT_FILES {
        beats.push(ToolCallReadRepeat.invocation(ctx, &session, nth * INTERCEPT_FILES + k));
    }
    beats.push(Stop.invocation(ctx, &session, nth));
    steps.extend(inline(beats.into_iter().map(Step::Invoke).collect()));
    steps.extend(commit_steps(
        ctx,
        &format!("bench/intercept_{nth}.txt"),
        &format!("intercept fixture {nth}\n"),
        &format!("bench: intercept fixture {nth}"),
    ));
    Prelude { steps }
}

/// The `why` row's file for the `nth` iteration, with the line the verb asks
/// about: `bench/why_<n>.txt:1`.
fn why_target(nth: usize) -> String {
    format!("bench/why_{nth}.txt:1")
}

/// The `why` row's prelude: a session whose turn WRITES the file (the write
/// lands between the tool call and the Stop, inside the turn's boundaries),
/// then a commit through the hooks so the line has a commit, the commit a
/// checkpoint, and the checkpoint a prompt.
fn why_prelude(ctx: &PlanCtx<'_>, spec: &ScenarioSpec) -> Prelude {
    let session = ctx.session_for(spec);
    let nth = spec.nth_event(ctx.iteration);
    let mut steps = session_steps(ctx, &session, nth);
    let rel_path = format!("bench/why_{nth}.txt");
    let stop = steps.pop();
    steps.push(Step::Write(
        ctx.repo.join(&rel_path),
        format!("why fixture {nth}: written inside the turn\n"),
    ));
    steps.extend(stop);
    let git = |args: &[&str]| {
        Step::Invoke(
            Invocation::new("git", ctx.repo)
                .args(args.to_vec())
                .env(CAPTURE_SYNC_ENV, "1"),
        )
    };
    steps.push(git(&["add", "--", &rel_path]));
    steps.push(git(&[
        "commit",
        "--quiet",
        "--message",
        &format!("bench: why fixture {nth}"),
    ]));
    Prelude { steps }
}

/// Where a row's timed verb leaves work for the detached agent-hook worker
/// — `<repo>/.git/rift-agent-queue`, the journal a `SessionEnd` condense and
/// a read's BPE count are handed to (an owner decision of 2026-08-30) — or `None`
/// for a row that detaches nothing.
///
/// The harness WAITS for that journal to drain, and only ever outside the
/// timed region: before the next iteration's untimed prelude, and once more
/// after the window before the probes. Two reasons, both about honesty. A
/// worker still condensing the previous session while the next iteration's
/// verb is timed would be measured as contention the budget never meant;
/// and a row whose deferred work never lands has published the latency of
/// writing a note, so the wait is bounded and a journal that will not drain
/// is a failed measure, not a fast one.
#[must_use]
pub fn detached_journal(id: ScenarioId, ctx: &PlanCtx<'_>) -> Option<PathBuf> {
    match (ctx.leg, spec(id).hook_event) {
        (LegId::Rift, Some(_)) => Some(ctx.repo.join(".git").join("rift-agent-queue")),
        _ => None,
    }
}

/// The untimed check that a row's verb really ANSWERED — ran once more
/// outside the timed region, over the same repository state — for verbs
/// whose wall time is meaningless without their answer: a `session resume`
/// that restored nothing and a `why` that resolved no checkpoint both exit
/// 0 quickly, and publishing either as a latency would be timing a refusal.
#[must_use]
pub fn answer_probe(id: ScenarioId, ctx: &PlanCtx<'_>) -> Option<Invocation> {
    match (id, ctx.leg) {
        (
            ScenarioId::ResumeRestore
            | ScenarioId::BlameWhy
            | ScenarioId::RecallInterceptSavings
            | ScenarioId::HookReadGate,
            LegId::Rift,
        ) => match plan(id, ctx) {
            Plan::Run(inv) => Some(*inv),
            Plan::NoAnalogue(_) => None,
        },
        _ => None,
    }
}

/// Whether this row's answer probe has to be asked a FRESH question — its own
/// untimed prelude replayed, one iteration past the window — rather than the
/// last measured iteration's question asked again.
///
/// The read gate answers a key once: the window it just declined would allow
/// the same call on a re-run, and a probe reading that Allow would report the
/// opposite of what the samples did. Replaying the prelude gives the probe a
/// window of its own, a first read of its own, and therefore the same decision
/// the timed samples were paid for.
#[must_use]
pub const fn probe_replays_the_prelude(id: ScenarioId) -> bool {
    matches!(id, ScenarioId::HookReadGate)
}

/// Read an [`answer_probe`]'s stdout: `Ok` when the verb answered, `Err`
/// with the reason when it did not. `None` for a row with no probe.
#[must_use]
pub fn parse_answer(id: ScenarioId, stdout: &str) -> Option<Result<(), String>> {
    let json = || {
        serde_json::from_str::<serde_json::Value>(stdout)
            .map_err(|e| format!("the answer is not JSON: {e}"))
    };
    match id {
        ScenarioId::ResumeRestore => Some(json().and_then(|v| {
            let restored = v
                .get("sessions")
                .and_then(|sessions| sessions.get(0))
                .and_then(|session| session.get("restored"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("absent");
            if restored == "restored" {
                Ok(())
            } else {
                Err(format!(
                    "session resume restored no transcript (sessions[0].restored = {restored:?}): \
                     a resume that restores nothing is a refusal, not a latency"
                ))
            }
        })),
        ScenarioId::BlameWhy => Some(json().and_then(|v| {
            let resolved = v
                .get("selection")
                .and_then(|sel| sel.get("checkpoint_id"))
                .and_then(serde_json::Value::as_str);
            if resolved.is_some() {
                Ok(())
            } else {
                Err(
                    "why resolved no checkpoint for the line (selection.checkpoint_id is not a \
                     string): an unresolved why is a refusal, not a latency"
                        .to_owned(),
                )
            }
        })),
        // A report that classified no reads at all saw no session, and its
        // residue of zero would read as a perfect saving rather than as an
        // empty fixture — which is the one way this row could flatter itself.
        ScenarioId::RecallInterceptSavings => Some(json().and_then(|v| {
            let reads = v
                .get("repeat_classes")
                .and_then(|c| c.get("first_read"))
                .and_then(|c| c.get("reads"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default();
            if reads > 0 {
                Ok(())
            } else {
                Err(format!(
                    "the scripted session left {reads} first reads in the projection: a residue \
                     measured over an empty pool is an empty fixture, not a saving"
                ))
            }
        })),
        // A budget row is a claim about the arm it names. This one names the
        // decline — the arm that hashes, reads the ledger, decides, composes a
        // reason and journals the accounting — and the allow path is cheaper
        // than all of that. A row that timed the cheap arm would make its own
        // bar easier to clear, so the probe reads the decision out of the same
        // envelope an agent reads it from and fails the measure on anything
        // but a decline.
        ScenarioId::HookReadGate => Some(json().and_then(|v| {
            let decision = v
                .get("hookSpecificOutput")
                .and_then(|out| out.get("permissionDecision"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("absent");
            if decision == "deny" {
                Ok(())
            } else {
                Err(format!(
                    "the gate answered this call {decision:?}, not a decline: the row is \
                     named for the arm that decides, and timing the arm that waves the \
                     call through would publish the cheap half of the verb"
                ))
            }
        })),
        _ => None,
    }
}

/// How many durable events one measured iteration of `id` owes: the timed
/// verb's own, plus one for every ingesting invocation in its untimed prelude.
///
/// The looser "at least one per iteration" floor a hook row started with is
/// satisfied by a row whose prelude ingests and whose *timed verb* does not —
/// the exact failure the proof exists to catch. Every prelude invocation is a
/// distinct hook event (they carry the same `nth` the timed verb does, and a
/// per-iteration row gives each iteration its own session), so each is worth
/// exactly one durable event and the floor is tight.
#[must_use]
pub fn durable_events_per_iteration(id: ScenarioId) -> u64 {
    let spec = spec(id);
    if !spec.requires_ingest() {
        return 0;
    }
    // Neither the paths nor the iteration change how many invocations a
    // prelude has — only the verb does.
    let ctx = PlanCtx {
        leg: LegId::Rift,
        binary: Path::new(""),
        repo: Path::new(""),
        sandbox: Path::new(""),
        iteration: 0,
        tool: None,
    };
    1 + prelude(id, &ctx).hook_invocations()
}

/// The untimed probe that counts how much work one measured iteration did.
///
/// A wall time is a claim about a workload, and the harness has to be able to
/// say what the workload was — otherwise "3 seconds" is unfalsifiable. The
/// probe runs **outside** every timed region, exactly as the ingest proof does,
/// and its count is published beside the timing rather than folded into it.
///
/// `None` where the leg or the row has nothing to count. That is reported as
/// absent, never as zero (see [`crate::measure::Stats::units_per_iteration`]).
#[must_use]
pub fn work_probe(id: ScenarioId, ctx: &PlanCtx<'_>) -> Option<Invocation> {
    match (id, ctx.leg) {
        // The verb under test is its own counter: it reports how many
        // checkpoints it re-derived. Run once more, untimed, so the count
        // covers the same repository state the published samples ran against.
        (ScenarioId::VerifyThroughput, LegId::Rift) => {
            Some(Invocation::new(ctx.binary, ctx.repo).args(["verify"]))
        }
        // The denominator is **measured, not scripted**. `vogon script` would
        // report the hooks the session *would* fire; the projection reports the
        // canonical events the session actually produced, so a drive that
        // silently stopped ingesting collapses the rate instead of holding it
        // up with a constant. Every condense iteration gets its own repository
        // copy, so this counts exactly one session's work.
        (ScenarioId::Condense, LegId::Rift) => {
            Some(Invocation::new(ctx.binary, ctx.repo).args(["status"]))
        }
        // The verb under test is its own counter again, and it can be: reading
        // a legacy history writes nothing to the source repository, so running
        // it once more untimed observes exactly the corpus the samples ran
        // against and disturbs nothing.
        (ScenarioId::ImportThroughput, LegId::Rift) => {
            Some(Invocation::new(ctx.binary, ctx.repo).args(["import", "--from-legacy"]))
        }
        // The serve is its own counter: it reports what it avoided, in the
        // accounting seam's own figure and the tokenizer that produced it. Run
        // once more untimed from the same member, over the same store — a
        // serve writes an audit row and no fact the next serve would read
        // differently, so the count covers exactly the state the samples ran
        // against.
        // The report is its own counter: it states how many tokens of this
        // session's reads it still classes as repeats of unchanged bytes. Run
        // once more untimed over the same repository state — the report
        // appends a metric snapshot and changes no fact a later run would read
        // differently.
        (ScenarioId::RecallInterceptSavings, LegId::Rift) => Some(reread_report_at(ctx)),
        (ScenarioId::RecallCompounding, LegId::Rift) => {
            // The caller's ctx sits one past the window, which is right for
            // repository-state probes but wrong here: the past-the-window
            // iteration's prelude never ran, so ITS member does not exist.
            // The store state every sample ran against is the family store,
            // and the last measured member serves over exactly that.
            let last = PlanCtx {
                iteration: ctx.iteration.saturating_sub(1),
                ..*ctx
            };
            Some(serve_at(&last, member_at(&last)))
        }
        _ => None,
    }
}

/// Read the work count out of a [`work_probe`]'s stdout.
#[must_use]
pub fn parse_work_units(id: ScenarioId, stdout: &str) -> Option<u64> {
    match id {
        // `verify: OK — re-derived 12 checkpoint(s) from …`
        ScenarioId::VerifyThroughput => stdout
            .split_once("re-derived ")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .and_then(|n| n.parse().ok()),
        ScenarioId::Condense => captured_events(stdout),
        // `importable N · refused M · events K` — the product's own count of
        // what it could read, never the count the harness planted.
        ScenarioId::ImportThroughput => crate::legacy::parse_importable(stdout),
        // `{"verb":…,"estimated_tokens_avoided":N,…}` — the product's own
        // figure, never one the harness re-derives.
        // `{"repeat_classes":{"reread_unchanged":{"tokens":N,…},…},…}` — the
        // product's own classification, never one the harness re-derives.
        ScenarioId::RecallInterceptSavings => serde_json::from_str::<serde_json::Value>(stdout)
            .ok()
            .and_then(|doc| {
                doc.get("repeat_classes")?
                    .get("reread_unchanged")?
                    .get("tokens")?
                    .as_u64()
            }),
        ScenarioId::RecallCompounding => serde_json::from_str::<serde_json::Value>(stdout)
            .ok()
            .and_then(|doc| {
                doc.get("estimated_tokens_avoided")
                    .and_then(serde_json::Value::as_u64)
            }),
        _ => None,
    }
}

/// The untimed check that a hook scenario really wrote something durable.
///
/// Only the rift leg can answer it: `rift status` publishes the projection's own
/// counters. The backend leg has no equivalent counter verb, so its hook numbers
/// are published without this proof and the report says which is which.
#[must_use]
pub fn ingest_probe(ctx: &PlanCtx<'_>) -> Option<Invocation> {
    match ctx.leg {
        LegId::Rift => Some(Invocation::new(ctx.binary, ctx.repo).args(["status"])),
        LegId::Reference => None,
    }
}

/// Read the durable event count out of `rift status`.
#[must_use]
pub fn captured_events(stdout: &str) -> Option<u64> {
    stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("captured events:"))
        .and_then(|rest| rest.trim().parse().ok())
}

/// The untimed preparation a leg needs before its verbs mean anything: capture
/// has to be enabled in the copy under test. It runs outside every timed region.
#[must_use]
pub fn setup_plan(leg: LegId, binary: &Path, repo: &Path) -> Invocation {
    match leg {
        // `--no-recall` only. **`--no-provision` was removed by `5028107`
        // ("collapse the archive to the one route that exists") and this line
        // went on passing it**, so every wired row's setup exited 2 and the
        // harness has measured nothing since — discovered by running it, in a
        // sitting the machine was held quiet for. Provisioning is gone fully:
        // capture is native and there is nothing to download, so the flag is
        // not merely renamed, its whole reason is deleted.
        LegId::Rift => Invocation::new(binary, repo).args(["init", "--no-recall"]),
        // Flags read off the backend's own `enable --help`. `--agent` is not
        // cosmetic: without it `enable` runs an interactive configuration flow, and
        // a benchmark that waits on a prompt never finishes. The rest keep the run
        // local — no telemetry, no session pushes, no repository creation.
        LegId::Reference => Invocation::new(binary, repo).args([
            "enable",
            "--agent",
            "claude-code",
            "--local",
            "--telemetry=false",
            "--skip-push-sessions",
            "--no-init-repo",
        ]),
    }
}

/// Environment that keeps a leg's state inside `sandbox` instead of the
/// developer's home directory — an unisolated run measures whatever happened to
/// be in `~` and cannot be reproduced.
#[must_use]
pub fn leg_env(leg: LegId, sandbox: &Path) -> Vec<(String, String)> {
    let at = |sub: &str| sandbox.join(sub).display().to_string();
    match leg {
        LegId::Rift => vec![
            ("RIFT_CONFIG_DIR".into(), at("rift-config")),
            ("RIFT_CACHE_DIR".into(), at("rift-cache")),
            // Never prompt for a keychain unlock, never fetch a backend: a
            // benchmark that blocks on a dialog is not a benchmark.
            ("RIFT_NO_KEYCHAIN".into(), "1".into()),
            ("RIFT_NO_PROVISION".into(), "1".into()),
            ("RIFT_HEADLESS".into(), "1".into()),
            ("HOME".into(), at("home")),
        ],
        LegId::Reference => vec![
            ("HOME".into(), at("home")),
            ("XDG_CONFIG_HOME".into(), at("xdg-config")),
            ("XDG_DATA_HOME".into(), at("xdg-data")),
            // Telemetry and login state are network-shaped; the pinned binary runs
            // as a local benchmark fixture only.
            ("REFERENCE_TELEMETRY".into(), "0".into()),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `vogon --turns N` fires 6N + 7 hooks. Pinned here because the number
    /// appears in the row's published title, and a title that states a workload
    /// is a claim.
    #[test]
    fn the_scripted_session_is_six_per_turn_plus_seven() {
        assert_eq!(condense_hooks(1), 13);
        assert_eq!(condense_hooks(2), 19);
        assert_eq!(condense_hooks(CONDENSE_TURNS), 25);
        assert!(
            spec(ScenarioId::Condense).title.contains("25 hooks"),
            "the row's title states the workload it plays"
        );
    }

    /// Without the driver the row is **refused with a reason**, never quietly
    /// replaced by something measurable. A benchmark that substitutes a
    /// workload it can run for the one it was asked for is the failure this
    /// whole catalogue's wired/planned distinction exists to prevent.
    #[test]
    fn condense_without_the_driver_is_refused_and_says_how_to_get_it() {
        let with_tool = ctx(
            LegId::Rift,
            Path::new("/bin/rift"),
            Path::new("/tmp/repo"),
            Path::new("/tmp/sandbox"),
        );
        let Plan::Run(inv) = plan(ScenarioId::Condense, &with_tool) else {
            panic!("with a driver the row must run")
        };
        let line = inv.args.join(" ");
        assert!(line.starts_with("drive --rift /bin/rift"), "{line}");
        assert!(line.contains("--repo /tmp/repo"), "{line}");
        assert!(line.contains("--turns 3"), "{line}");
        assert!(line.ends_with("--commit"), "{line}");

        let without = PlanCtx {
            tool: None,
            ..with_tool
        };
        let Plan::NoAnalogue(reason) = plan(ScenarioId::Condense, &without) else {
            panic!("without a driver the row must not run")
        };
        assert!(reason.contains("RIFT_BENCH_VOGON_BIN"), "{reason}");
    }

    /// The trap named in `vogon_spec_flags`: `--repo` is part of the payload
    /// and the payload is part of a canonical event's identity, so every mode
    /// must be handed the same vector. `--commit` is the one permitted
    /// difference and it is an action, not a payload.
    #[test]
    fn every_vogon_mode_is_handed_the_same_payload_flags() {
        let flags = vogon_spec_flags(Path::new("/tmp/repo"));
        assert!(flags.contains(&"--repo".to_owned()));
        assert!(!flags.iter().any(|f| f == "--commit"));
        assert_eq!(flags, vogon_spec_flags(Path::new("/tmp/repo")));
        assert_ne!(flags, vogon_spec_flags(Path::new("/tmp/other")));
    }

    fn ctx<'a>(leg: LegId, binary: &'a Path, repo: &'a Path, sandbox: &'a Path) -> PlanCtx<'a> {
        PlanCtx {
            leg,
            binary,
            repo,
            sandbox,
            iteration: 0,
            tool: Some(Path::new("/bin/vogon")),
        }
    }

    fn probe(leg: LegId) -> PlanCtx<'static> {
        ctx(
            leg,
            Path::new("/bin/rift"),
            Path::new("/tmp/repo"),
            Path::new("/tmp/sandbox"),
        )
    }

    /// The four shadow-worktree rows, in the spelling everything else reads
    /// them by.
    const WORKTREE_ROWS: [ScenarioId; 4] = [
        ScenarioId::WorktreeAddAdopt,
        ScenarioId::MemberSwitch,
        ScenarioId::RecallKnownState,
        ScenarioId::RecallCompounding,
    ];

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

    /// What each of the four rows actually times, read off the plan.
    ///
    /// The two member rows time **git**, not Rift: the claim is what the
    /// seventh hook adds to an operation a person typed, and a row that timed
    /// `rift` would be measuring the hook without the git it must not delay.
    /// The two recall rows time the product binary, and the compounding row
    /// times it **in the member** — a serve from the main work tree would be a
    /// checkout being handed its own reads back, which is not compounding.
    #[test]
    fn the_member_rows_time_git_and_the_compounding_row_serves_the_member() {
        let ctx = probe(LegId::Rift);
        let member = member_at(&ctx);
        let timed = |id| match plan(id, &ctx) {
            Plan::Run(inv) => *inv,
            Plan::NoAnalogue(why) => panic!("{}: {why}", ScenarioId::as_str(id)),
        };

        let add = timed(ScenarioId::WorktreeAddAdopt);
        assert_eq!(add.program, Path::new("git"));
        assert_eq!(add.cwd, Path::new("/tmp/repo"));
        assert!(add.args.contains(&"worktree".to_owned()) && add.args.contains(&"add".to_owned()));
        assert!(
            add.args.contains(&member.display().to_string()),
            "the timed add creates the member the row is named for: {:?}",
            add.args
        );

        let switch = timed(ScenarioId::MemberSwitch);
        assert_eq!(switch.program, Path::new("git"));
        assert_eq!(
            switch.cwd, member,
            "the switch is measured INSIDE the member, not in the main work tree"
        );
        assert!(switch.args.contains(&switch_branch(0)));

        let known = timed(ScenarioId::RecallKnownState);
        assert_eq!(known.program, Path::new("/bin/rift"));
        assert_eq!(known.cwd, Path::new("/tmp/repo"));
        let compounding = timed(ScenarioId::RecallCompounding);
        assert_eq!(compounding.program, Path::new("/bin/rift"));
        assert_eq!(compounding.cwd, member);
        for inv in [&known, &compounding] {
            assert_eq!(inv.args.first().map(String::as_str), Some("recall"));
            assert_eq!(inv.args.get(1).map(String::as_str), Some("known-state"));
            assert!(inv.args.contains(&"--json".to_owned()));
        }
        // The work probe re-runs the SAME invocation: a probe that measured a
        // cheaper serve would publish a figure the samples never produced.
        assert_eq!(
            work_probe(ScenarioId::RecallCompounding, &ctx).map(|i| i.args),
            Some(compounding.args.clone())
        );
        assert_eq!(
            parse_work_units(
                ScenarioId::RecallCompounding,
                r#"{"verb":"known-state","estimated_tokens_avoided":41}"#
            ),
            Some(41)
        );
        assert_eq!(
            parse_work_units(ScenarioId::RecallCompounding, "not json"),
            None,
            "an unreadable answer is absent, never zero"
        );
        // The backend leg has no counterpart for any of them, and says so.
        for id in WORKTREE_ROWS {
            assert!(
                matches!(plan(id, &probe(LegId::Reference)), Plan::NoAnalogue(_)),
                "{} claims a backend analogue it does not have",
                id.as_str()
            );
        }
    }

    /// Each row's untimed preparation leaves exactly the state its timed verb
    /// needs — and, for the rows that capture, leaves it INLINE, because a
    /// non-hook row names no detached journal for the harness to wait on.
    #[test]
    fn the_shadow_worktree_preludes_leave_what_the_timed_verb_needs() {
        let ctx = probe(LegId::Rift);
        let member = member_at(&ctx).display().to_string();

        // The add row must not create the member itself — the timed verb does.
        let add = prelude(ScenarioId::WorktreeAddAdopt, &ctx);
        assert!(add
            .invocations()
            .iter()
            .all(|inv| !inv.args.contains(&member)));
        assert!(
            add.invocations()
                .iter()
                .any(|inv| inv.args.first().is_some_and(|a| a == "enable")),
            "without an enabled agent this row measures a cheaper shape"
        );

        // The switch row must: the timed switch happens in a member that is
        // already there and already adopted.
        let switch = prelude(ScenarioId::MemberSwitch, &ctx);
        assert!(switch
            .invocations()
            .iter()
            .any(|inv| inv.args.contains(&member)));
        assert!(switch.invocations().iter().any(|inv| inv
            .args
            .first()
            .is_some_and(|a| a == "branch")
            && inv.args.contains(&switch_branch(0))));

        for id in [ScenarioId::RecallKnownState, ScenarioId::RecallCompounding] {
            let p = prelude(id, &ctx);
            assert!(
                p.invocations()
                    .iter()
                    .any(|inv| inv.args == ["recall", "init"]),
                "{}: recall is off by default and setup keeps it off",
                id.as_str()
            );
            assert!(
                p.hook_invocations() > 0,
                "{}: a serve over a session nobody played serves nothing",
                id.as_str()
            );
            assert!(
                p.invocations()
                    .iter()
                    .filter(|inv| inv.program == Path::new("git")
                        || inv.args.first().is_some_and(|a| a == "hooks"))
                    .all(|inv| inv
                        .env
                        .iter()
                        .any(|(k, v)| k == CAPTURE_SYNC_ENV && v == "1")),
                "{}: every step that can capture does it inline, or the timer races the worker",
                id.as_str()
            );
            assert_eq!(
                detached_journal(id, &ctx),
                None,
                "{}: a row that waits on no journal must leave none owed",
                id.as_str()
            );
            let creates_member = p.invocations().iter().any(|inv| inv.args.contains(&member));
            assert_eq!(
                creates_member,
                id == ScenarioId::RecallCompounding,
                "{}: only the compounding row is served from a member",
                id.as_str()
            );
        }
    }

    /// Whether `id`'s prelude opens a turn, which advances the ordinal that is
    /// part of a canonical event's identity.
    fn opens_a_turn(id: ScenarioId) -> bool {
        prelude(id, &probe(LegId::Rift))
            .invocations()
            .iter()
            .any(|inv| inv.args.get(2).is_some_and(|v| v == "UserPromptSubmit"))
    }

    fn payload_of(inv: &Invocation) -> serde_json::Value {
        serde_json::from_str(inv.stdin.as_deref().expect("hook invocations carry stdin")).unwrap()
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
    fn wired_scenarios_use_the_same_argv_on_both_legs() {
        let (bin, repo, sb) = (
            Path::new("/bin/echo"),
            Path::new("/tmp/repo"),
            Path::new("/tmp/sandbox"),
        );
        let mut ids = vec![ScenarioId::ColdStatus, ScenarioId::CheckpointList];
        ids.extend(ScenarioId::hooks());
        for id in ids {
            let (Plan::Run(rift), Plan::Run(reference)) = (
                plan(id, &ctx(LegId::Rift, bin, repo, sb)),
                plan(id, &ctx(LegId::Reference, bin, repo, sb)),
            ) else {
                panic!("{} must be runnable on both legs", id.as_str())
            };
            assert_eq!(rift.args, reference.args, "identical scripted invocation");
            assert_eq!(rift.stdin, reference.stdin, "identical scripted payload");
        }
    }

    #[test]
    fn the_hook_payload_is_the_native_claude_code_shape() {
        let value: serde_json::Value = serde_json::from_str(HOOK_PAYLOAD).unwrap();
        assert!(value["session_id"].is_string());
        assert_eq!(value["hook_event_name"], "PreToolUse");
        let Plan::Run(inv) = plan(ScenarioId::HookRoundTrip, &probe(LegId::Rift)) else {
            panic!("hook round-trip is wired")
        };
        assert_eq!(inv.args, ["hooks", "claude-code", "PreToolUse"]);
        // The baseline payload, key for key: `hook-round-trip` numbers stay
        // comparable with the first recorded run. Only `transcript_path` is
        // added, and it is inert until a transcript exists on disk.
        let mut built = payload_of(&inv);
        assert!(built
            .as_object_mut()
            .unwrap()
            .remove("transcript_path")
            .is_some());
        assert_eq!(built, value);
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
    fn a_stop_closes_a_turn_it_opened_and_leaves_the_transcript_for_the_next_hook() {
        let c = probe(LegId::Rift);
        let pre = prelude(ScenarioId::HookStop, &c);
        let verbs: Vec<_> = pre
            .invocations()
            .iter()
            .map(|i| i.args[2].clone())
            .collect();
        assert_eq!(verbs, ["UserPromptSubmit", "PreToolUse", "PostToolUse"]);
        assert!(
            matches!(pre.steps.last(), Some(Step::Write(..))),
            "a hook prelude ends with the transcript write"
        );
        // One transcript, written after the prelude ran, where the payload
        // says — and named on EVERY hook of the session, because the settle
        // that records this turn's spend runs on the next hook, not on the
        // Stop (the deferred-settle contract the row's title states).
        assert!(spec(ScenarioId::HookStop)
            .title
            .contains("settled by the next hook"));
        for verb in HookEvent::all_for_test() {
            assert!(verb.carries_transcript(), "{}", verb.verb());
            assert!(
                payload_of(&verb.invocation(&c, BENCH_SESSION, 0))["transcript_path"].is_string(),
                "{} names the transcript",
                verb.verb()
            );
        }
        let (path, body) = pre
            .files()
            .first()
            .map(|(p, b)| (p.to_path_buf(), (*b).to_owned()))
            .expect("a stop leaves a transcript for the next hook");
        let Plan::Run(inv) = plan(ScenarioId::HookStop, &c) else {
            panic!("wired")
        };
        assert_eq!(
            payload_of(&inv)["transcript_path"],
            path.display().to_string()
        );
        assert!(
            !path.starts_with(c.repo),
            "a transcript is not repo content"
        );
        // It parses as the JSONL the usage enrichment reads.
        let lines: Vec<_> = body.lines().collect();
        assert_eq!(lines.len(), 16);
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(v["type"].is_string());
        }
        assert!(body.ends_with('\n'), "every record is newline-terminated");
        let last: serde_json::Value = serde_json::from_str(lines[15]).unwrap();
        assert_eq!(last["type"], "assistant");
        assert!(last["message"]["usage"]["input_tokens"].is_u64());
    }

    /// Every hook row on the rift leg names the worker's journal to wait on;
    /// no other row, and no reference-leg row, waits on anything.
    #[test]
    fn hook_rows_name_the_detached_journal_and_nothing_else_does() {
        let c = probe(LegId::Rift);
        for id in ScenarioId::all() {
            let named = detached_journal(id, &c);
            if spec(id).hook_event.is_some() {
                assert_eq!(
                    named.as_deref(),
                    Some(c.repo.join(".git").join("rift-agent-queue").as_path()),
                    "{}",
                    id.as_str()
                );
            } else {
                assert_eq!(named, None, "{}", id.as_str());
            }
            assert_eq!(
                detached_journal(id, &probe(LegId::Reference)),
                None,
                "{}: the reference leg detaches nothing the harness knows of",
                id.as_str()
            );
        }
    }

    #[test]
    fn a_session_end_replays_a_whole_session_first() {
        let verbs: Vec<_> = prelude(ScenarioId::HookSessionEnd, &probe(LegId::Rift))
            .invocations()
            .iter()
            .map(|i| i.args[2].clone())
            .collect();
        assert_eq!(
            verbs,
            ["SessionStart", "UserPromptSubmit", "PreToolUse", "Stop"]
        );
        // A session start follows nothing at all.
        assert!(prelude(ScenarioId::HookSessionStart, &probe(LegId::Rift))
            .invocations()
            .is_empty());
        // And a read-only verb has no prelude at all.
        assert!(prelude(ScenarioId::ColdStatus, &probe(LegId::Rift))
            .steps
            .is_empty());
    }

    #[test]
    fn a_read_shaped_call_names_a_file_the_fixtures_actually_contain() {
        // If this drifts, the read path resolves to no fact and the measure stops
        // covering worktree hashing without failing.
        let spec = crate::fixture::FixtureSpec::preset(HOOK_FIXTURE).unwrap();
        for nth in [0, 1, 11, 12, 97] {
            let target = read_target(nth);
            let expected = crate::fixture::generated_path(&spec, nth % spec.total_files())
                .display()
                .to_string();
            assert_eq!(target, expected, "nth={nth}");
        }
        assert_eq!(read_target(0), "src/mod_0000/file_0000.rs");
        let Plan::Run(inv) = plan(ScenarioId::HookReadTool, &probe(LegId::Rift)) else {
            panic!("wired")
        };
        assert_eq!(payload_of(&inv)["tool_input"]["file_path"], read_target(0));
        assert_eq!(payload_of(&inv)["tool_name"], "Read");
    }

    #[test]
    fn consecutive_iterations_deliver_genuinely_new_events() {
        // The load-bearing property of the whole hook set. A canonical event's
        // identity is clock-free — same session, same turn, same payload bytes
        // means *replay*, and ingest is a designed no-op. A row that repeats its
        // payload therefore times the dedup short-circuit while claiming to time
        // a round-trip. (This is exactly what the first recorded
        // `hook-round-trip` baseline did: 33 identical payloads, one ingest.)
        for id in ScenarioId::hooks() {
            let s = spec(id);
            let event = s.hook_event.unwrap();
            let mut seen = std::collections::BTreeSet::new();
            for iteration in 0..64 {
                let ctx = PlanCtx {
                    iteration,
                    ..probe(LegId::Rift)
                };
                let session = ctx.session_for(&s);
                let nth = s.nth_event(iteration);
                seen.insert(event.payload(ctx.repo, &session, nth, None));
            }
            match s.delivery {
                // An event is new when its payload differs — or when its prelude
                // has opened a new turn, since the turn ordinal is part of the
                // identity too. `hook-stop` is the second kind: every Stop is
                // byte-identical and every one closes a different turn.
                Delivery::Distinct => assert!(
                    seen.len() == 64 || opens_a_turn(id),
                    "{} delivers the same event twice",
                    id.as_str()
                ),
                Delivery::Replay => assert_eq!(seen.len(), 1, "{} varies", id.as_str()),
            }
            assert_eq!(s.requires_ingest(), s.delivery == Delivery::Distinct);
        }
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
        let probe = work_probe(ScenarioId::ImportThroughput, &ctx).expect("the verb counts itself");
        assert_eq!(probe.args, ["import", "--from-legacy"]);

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

        let Plan::NoAnalogue(note) =
            plan(ScenarioId::ImportThroughput, &probe_leg(LegId::Reference))
        else {
            panic!("the retired reference implementation read no import of its own format")
        };
        assert!(note.contains("rift-only"));
    }

    fn probe_leg(leg: LegId) -> PlanCtx<'static> {
        probe(leg)
    }

    /// The why row: wired, rift-only, its prelude writes the file INSIDE the
    /// turn (between the tool call and the Stop) and commits it through the
    /// hooks, the timed verb asks about line 1 of that file, and the answer
    /// probe refuses a `why` that resolved no checkpoint.
    #[test]
    fn blame_why_is_wired_and_its_prelude_writes_the_file_inside_the_turn() {
        let s = spec(ScenarioId::BlameWhy);
        assert!(s.is_wired());
        assert_eq!(s.session, Session::PerIteration);
        assert_eq!(s.isolation, Isolation::Shared);
        assert_eq!(s.budget, None, "no owner has ratified a why budget");
        assert_eq!(s.fixture, HOOK_FIXTURE);

        let c = probe(LegId::Rift);
        let Plan::Run(inv) = plan(ScenarioId::BlameWhy, &c) else {
            panic!("wired")
        };
        assert_eq!(inv.args, ["--json", "why", "bench/why_0.txt:1"]);
        let later = PlanCtx {
            iteration: 7,
            ..probe(LegId::Rift)
        };
        let Plan::Run(inv7) = plan(ScenarioId::BlameWhy, &later) else {
            panic!("wired")
        };
        assert_eq!(
            inv7.args[2], "bench/why_7.txt:1",
            "each iteration asks about its own line"
        );
        assert!(matches!(
            plan(ScenarioId::BlameWhy, &probe_leg(LegId::Reference)),
            Plan::NoAnalogue(note) if note.contains("rift-only")
        ));

        let pre = prelude(ScenarioId::BlameWhy, &c);
        let shape: Vec<String> = pre
            .steps
            .iter()
            .map(|st| match st {
                Step::Write(p, _) => format!("write {}", p.file_name().unwrap().to_string_lossy()),
                Step::Invoke(i) => format!(
                    "{} {}",
                    i.program.file_name().unwrap().to_string_lossy(),
                    i.args.iter().take(3).cloned().collect::<Vec<_>>().join(" ")
                ),
            })
            .collect();
        let session = c.session_for(&s);
        assert_eq!(
            shape,
            [
                format!("write {session}.jsonl"),
                "rift hooks claude-code SessionStart".to_owned(),
                "rift hooks claude-code UserPromptSubmit".to_owned(),
                "rift hooks claude-code PreToolUse".to_owned(),
                "write why_0.txt".to_owned(),
                "rift hooks claude-code Stop".to_owned(),
                "git add -- bench/why_0.txt".to_owned(),
                "git commit --quiet --message".to_owned(),
            ]
        );
        let commit = pre.invocations().last().copied().unwrap();
        assert!(commit
            .env
            .iter()
            .any(|(k, v)| k == CAPTURE_SYNC_ENV && v == "1"));
        assert_eq!(pre.hook_invocations(), 4);
        assert_eq!(durable_events_per_iteration(ScenarioId::BlameWhy), 0);

        let probe_inv = answer_probe(ScenarioId::BlameWhy, &c).expect("the row answers");
        assert_eq!(probe_inv.args, inv.args);
        // The probe asks the iteration it is handed about ITS file: the
        // driver hands it the last measured one, never the one past the
        // measured region (whose file no prelude wrote — the first proof run failed
        // its own probe on exactly that).
        assert_eq!(
            answer_probe(ScenarioId::BlameWhy, &later).unwrap().args[2],
            "bench/why_7.txt:1"
        );
        assert_eq!(
            parse_answer(
                ScenarioId::BlameWhy,
                r#"{"selection":{"checkpoint_id":"abc123","resolved":true}}"#
            ),
            Some(Ok(()))
        );
        let refused = parse_answer(ScenarioId::BlameWhy, r#"{"selection":null}"#)
            .unwrap()
            .unwrap_err();
        assert!(refused.contains("resolved no checkpoint"), "{refused}");
        assert!(parse_answer(ScenarioId::BlameWhy, "nope").unwrap().is_err());
    }

    /// The resume row: wired, rift-only, its prelude plays a session on a
    /// session id of its own and commits it THROUGH the hooks with the
    /// archive inline, the timed verb resumes the fixture branch with
    /// `--force`, and the answer probe refuses a resume that restored nothing.
    #[test]
    fn resume_restore_is_wired_and_its_prelude_lands_a_checkpoint_inline() {
        let s = spec(ScenarioId::ResumeRestore);
        assert!(s.is_wired());
        assert_eq!(s.session, Session::PerIteration);
        assert_eq!(s.isolation, Isolation::Shared);
        assert_eq!(s.budget, None, "no owner has ratified a resume budget");
        assert_eq!(s.work_unit, None);
        assert_eq!(s.fixture, HOOK_FIXTURE);

        let c = probe(LegId::Rift);
        let Plan::Run(inv) = plan(ScenarioId::ResumeRestore, &c) else {
            panic!("wired")
        };
        assert_eq!(
            inv.args,
            ["--json", "session", "resume", FIXTURE_BRANCH, "--force"]
        );
        assert!(matches!(
            plan(ScenarioId::ResumeRestore, &probe_leg(LegId::Reference)),
            Plan::NoAnalogue(note) if note.contains("rift-only")
        ));

        let pre = prelude(ScenarioId::ResumeRestore, &c);
        let shape: Vec<String> = pre
            .steps
            .iter()
            .map(|st| match st {
                Step::Write(p, _) => format!("write {}", p.file_name().unwrap().to_string_lossy()),
                Step::Invoke(i) => format!(
                    "{} {}",
                    i.program.file_name().unwrap().to_string_lossy(),
                    i.args.iter().take(3).cloned().collect::<Vec<_>>().join(" ")
                ),
            })
            .collect();
        let session = c.session_for(&s);
        assert_eq!(
            shape,
            [
                format!("write {session}.jsonl"),
                "rift hooks claude-code SessionStart".to_owned(),
                "rift hooks claude-code UserPromptSubmit".to_owned(),
                "rift hooks claude-code PreToolUse".to_owned(),
                "rift hooks claude-code Stop".to_owned(),
                "write resume_0.txt".to_owned(),
                "git add -- bench/resume_0.txt".to_owned(),
                "git commit --quiet --message".to_owned(),
            ]
        );
        // The commit condenses inline, through the repository's own hooks.
        let commit = pre.invocations().last().copied().unwrap();
        assert_eq!(commit.hooks, crate::hermetic::Hooks::Enabled);
        assert!(commit
            .env
            .iter()
            .any(|(k, v)| k == CAPTURE_SYNC_ENV && v == "1"));
        assert_eq!(pre.hook_invocations(), 4);
        assert_eq!(
            durable_events_per_iteration(ScenarioId::ResumeRestore),
            0,
            "not a hook row: it owes no ingest proof, it owes an ANSWER"
        );

        let probe_inv = answer_probe(ScenarioId::ResumeRestore, &c).expect("the row answers");
        assert_eq!(probe_inv.args, inv.args);
        assert!(answer_probe(ScenarioId::ResumeRestore, &probe_leg(LegId::Reference)).is_none());
        assert_eq!(
            parse_answer(
                ScenarioId::ResumeRestore,
                r#"{"sessions":[{"restored":"restored"}]}"#
            ),
            Some(Ok(()))
        );
        let refused = parse_answer(
            ScenarioId::ResumeRestore,
            r#"{"sessions":[{"restored":"no-archived-transcript"}]}"#,
        )
        .unwrap()
        .unwrap_err();
        assert!(refused.contains("no-archived-transcript"), "{refused}");
        assert!(parse_answer(ScenarioId::ResumeRestore, "not json")
            .unwrap()
            .is_err());
        assert!(
            parse_answer(ScenarioId::ResumeRestore, r#"{"sessions":[]}"#)
                .unwrap()
                .is_err()
        );
        assert!(parse_answer(ScenarioId::ColdStatus, "{}").is_none());
    }

    #[test]
    fn verify_is_rift_only_and_says_why() {
        let Plan::NoAnalogue(note) = plan(ScenarioId::VerifyThroughput, &probe(LegId::Reference))
        else {
            panic!("the retired reference implementation had no integrity check to measure")
        };
        assert!(note.contains("rift-only measure"));
        assert!(matches!(
            plan(ScenarioId::VerifyThroughput, &probe(LegId::Rift)),
            Plan::Run(_)
        ));
    }

    #[test]
    fn the_ingest_floor_counts_the_prelude_as_well_as_the_timed_verb() {
        // One per row, spelled out: these are the numbers the recorded baseline
        // observed (+30 / +60 / +120 / +150 over 30 iterations), so the floor is
        // an equality in practice and a row that quietly stopped ingesting
        // cannot hide behind its prelude's growth.
        for (id, expected) in [
            (ScenarioId::HookRoundTrip, 1),
            (ScenarioId::HookSessionStart, 1),
            (ScenarioId::HookPromptSubmit, 1),
            (ScenarioId::HookReadTool, 1),
            (ScenarioId::HookToolResult, 2),
            (ScenarioId::HookStop, 4),
            (ScenarioId::HookSessionEnd, 5),
            // A replay writes nothing by design and is asked for no proof.
            (ScenarioId::HookReplay, 0),
            // Not a hook row at all.
            (ScenarioId::ColdStatus, 0),
        ] {
            assert_eq!(
                durable_events_per_iteration(id),
                expected,
                "{}",
                id.as_str()
            );
        }
        // The floor is exactly "the timed verb plus its prelude", for every row.
        for id in ScenarioId::hooks() {
            let s = spec(id);
            let pre = prelude(id, &probe(LegId::Rift)).hook_invocations();
            assert_eq!(
                durable_events_per_iteration(id),
                if s.requires_ingest() { 1 + pre } else { 0 },
                "{}",
                id.as_str()
            );
        }
    }

    #[test]
    fn ingest_is_proved_from_the_projections_own_counter() {
        let real = "rift status\n  checkpoints:   0\n  sessions:      1\n  \
                    captured events: 42\n  hook receipts: 42\n  transitions:   3\n";
        assert_eq!(captured_events(real), Some(42));
        assert_eq!(captured_events("rift status\n  sessions: 1\n"), None);
        assert_eq!(captured_events(""), None);
        assert_eq!(captured_events("captured events: not-a-number"), None);

        let inv = ingest_probe(&probe(LegId::Rift)).expect("rift publishes counters");
        assert_eq!(inv.args, ["status"]);
        assert!(
            ingest_probe(&probe(LegId::Reference)).is_none(),
            "the backend has no counter verb; its hook numbers carry no proof"
        );
    }

    #[test]
    fn setup_never_provisions_or_phones_home() {
        let rift = setup_plan(LegId::Rift, Path::new("rift"), Path::new("/tmp/repo"));
        assert_eq!(rift.args, vec!["init".to_owned(), "--no-recall".to_owned()]);
        let reference = setup_plan(
            LegId::Reference,
            Path::new("reference"),
            Path::new("/tmp/repo"),
        );
        assert!(reference.args.contains(&"--telemetry=false".to_owned()));
        assert!(reference.args.contains(&"--skip-push-sessions".to_owned()));
        // Non-interactive or the benchmark hangs on a configuration prompt.
        assert!(reference.args.contains(&"--agent".to_owned()));
    }

    #[test]
    fn leg_env_keeps_every_leg_inside_the_sandbox() {
        let sandbox = Path::new("/tmp/sandbox-42");
        for leg in LegId::all() {
            let env = leg_env(leg, sandbox);
            let home = env
                .iter()
                .find(|(k, _)| k == "HOME")
                .map(|(_, v)| v.clone())
                .unwrap();
            assert!(home.starts_with("/tmp/sandbox-42"), "{home}");
            assert!(env.iter().all(|(_, v)| !v.is_empty()));
        }
        let rift = leg_env(LegId::Rift, sandbox);
        assert!(rift
            .iter()
            .any(|(k, v)| k == "RIFT_NO_KEYCHAIN" && v == "1"));
        assert!(rift
            .iter()
            .any(|(k, v)| k == "RIFT_NO_PROVISION" && v == "1"));
    }
}
