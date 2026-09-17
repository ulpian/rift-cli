//! The `rift` command-line surface: the `clap`-derive command tree.
//!
//! Bare `rift` launches Canopy (the TUI) on a terminal, or prints a status summary
//! when piped. Every subcommand is a short-lived process that does one job and
//! exits. `rift hooks` is hidden — it is the agent/git wire entry, not a
//! human-facing command. The exit-code contract and `--json` output conventions
//! live in [`crate::main`]'s dispatch.

use clap::{Args, Parser, Subcommand, ValueEnum};

/// `rift` — provenance, recall, and cost for AI coding agents.
#[derive(Parser, Debug)]
#[command(
    name = "rift",
    version,
    about = "Rift — provenance, recall, and cost for AI coding agents",
    disable_help_subcommand = true
)]
pub(crate) struct Cli {
    /// The subcommand; `None` launches Canopy (TTY) or prints status (non-TTY).
    #[command(subcommand)]
    pub(crate) cmd: Option<Command>,

    /// Emit a single machine-readable JSON document (scriptable; CI).
    #[arg(long, global = true)]
    pub(crate) json: bool,

    /// Suppress non-essential output (the exit code is still authoritative).
    #[arg(long, global = true)]
    pub(crate) quiet: bool,

    /// Raise diagnostic verbosity to stderr (`-v`, `-vv`); never logs secrets.
    #[arg(short = 'v', long, global = true, action = clap::ArgAction::Count)]
    pub(crate) verbose: u8,

    /// When to colorize output.
    #[arg(long, global = true, value_enum)]
    pub(crate) color: Option<ColorChoice>,

    /// Force the Canopy dashboard's ASCII fallback renderer (no truecolor / 3D).
    #[arg(long, global = true)]
    pub(crate) ascii: bool,

    /// Skip the Canopy startup splash (also honored: `RIFT_SPLASH=never`).
    #[arg(long, global = true)]
    pub(crate) skip_splash: bool,
}

/// `rift savings` — the figure, and the window it is measured over.
#[derive(Args, Debug, Clone, Default)]
pub(crate) struct SavingsArgs {
    /// Move the window instead of reading it (an owner act, not an agent's).
    #[command(subcommand)]
    pub(crate) cmd: Option<SavingsCmd>,

    /// Measure from this instant instead of the family's boundary marker
    /// (UTC RFC3339 or epoch milliseconds). Reads only: nothing is written,
    /// and the stored boundary is left exactly as it was.
    #[arg(long, value_name = "TIMESTAMP")]
    pub(crate) since: Option<String>,

    /// Measure over a trailing window: `24h`, `7d` or `30d` — the dashboard's
    /// four periods, so the card and this command answer the same question.
    /// Composed with the family's boundary marker rather than replacing it:
    /// a window never reaches back past the evaluation period somebody set.
    /// Reads only, like `--since`, and mutually exclusive with it: one window
    /// per call, named one way.
    #[arg(long, value_name = "WINDOW", conflicts_with = "since")]
    pub(crate) last: Option<String>,
}

/// The one thing `rift savings` can change.
#[derive(Subcommand, Debug, Clone)]
pub(crate) enum SavingsCmd {
    /// Start a fresh evaluation period here: write this family's boundary
    /// marker, so `rift savings` counts only what is recorded from now on.
    /// Deletes nothing and rewrites no relation — the record is untouched.
    Reset(SavingsResetArgs),
}

/// `rift savings reset` — where to put the boundary.
#[derive(Args, Debug, Clone, Default)]
pub(crate) struct SavingsResetArgs {
    /// Place the boundary at this instant (UTC RFC3339 or epoch milliseconds)
    /// rather than now. A boundary in the future is refused.
    #[arg(long, value_name = "TIMESTAMP")]
    pub(crate) at: Option<String>,
}

/// Color policy for human output.
#[derive(Copy, Clone, Debug, ValueEnum)]
pub(crate) enum ColorChoice {
    /// Colorize when stdout is a TTY (default).
    Auto,
    /// Always colorize.
    Always,
    /// Never colorize.
    Never,
}

/// The `rift` subcommands.
#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// Scaffold `.rift/`, install git hooks, init the projection, wire agents.
    #[command(
        alias = "setup",
        long_about = "Scaffold `.rift/`, install git hooks, and \
        initialize the projection.\n\nCapture is native Rust and runs in this binary: \
        there is nothing to download and no other program to install. Everything this \
        command does is local, and every step it takes is recorded in the append-only \
        audit ledger."
    )]
    Init(InitArgs),
    /// One-shot snapshot of capture / projection / verify health.
    Status,
    /// Open the read-only Canopy checkpoint, shadow, transcript, and Recall viewer.
    Viewer(ViewerArgs),
    /// Diagnose capture, the memory chain, sessions, the queue and the soak (no network).
    Doctor(DoctorArgs),
    /// GC transient state: shadow refs past TTL, stale queue entries, run-state
    /// caches. Never touches canonical refs or the chain.
    Clean(CleanArgs),
    /// Wire an agent's hooks to `rift hooks <agent> <verb>`.
    Enable(AgentArgs),
    /// Remove an agent's `rift` hook wiring.
    Disable(AgentArgs),
    /// Re-derive the hash chain and cross-check the projection (its own exit codes).
    Verify,
    /// Rebuild the projection from the redacted checkpoint branch (the artifact
    /// bridge). Idempotent: over a live projection it inserts nothing new.
    Rebuild,
    /// Put back the projection facts a rebuild cannot derive: the per-commit
    /// diff facts (from git history) and, with `--from`, the session/cost rows
    /// that only a set-aside projection still holds.
    Backfill(BackfillArgs),
    /// Read a pre-Rift capture tool's checkpoints out of this repository.
    Import(ImportArgs),
    /// Inspect or record the canonical checkpoint order (`refs/rift/chain/v1`).
    #[command(subcommand)]
    Chain(ChainCmd),
    /// Inspect checkpoints.
    #[command(subcommand)]
    Checkpoint(CheckpointCmd),
    /// Inspect and drive sessions (list, info, resume, attach, stop).
    #[command(subcommand, visible_alias = "sessions")]
    Session(SessionCmd),
    /// Ship queued checkpoint refs to the configured checkpoint remote.
    #[command(subcommand)]
    Sync(SyncCmd),
    /// Write or apply an offline transfer bundle (checkpoints + chain segment).
    #[command(subcommand)]
    Bundle(BundleCmd),
    /// What happened in a window: checkpoints, sessions, attribution, cost,
    /// savings, risk — facts only, for your agent to reason over.
    Dispatch(DispatchArgs),
    /// Offline usage analytics: sessions, tokens, tool mix, serves, savings.
    Recap(RecapArgs),
    /// Compile the review evidence pack for the commits in scope. Rift gathers
    /// the facts; the verdict is your agent's.
    Review(ReviewArgs),
    /// Scaffold an investigation: findings.md plus a budgeted evidence pack.
    Investigate(InvestigateArgs),
    /// Explain a checkpoint: roots, chain position, attribution, cost.
    Explain(ExplainArgs),
    /// Blame a file: which checkpoint, session and agent explain each line.
    Blame(BlameArgs),
    /// One line's full story: who wrote it, under which prompt, at what cost.
    Why(WhyArgs),
    /// The savings share over this family's window: avoided ÷ (avoided + read),
    /// overall and per family, with the honesty figure and the window beside it.
    Savings(SavingsArgs),
    /// Ask the recall plane: what changed, is a file stale, where tokens go.
    #[command(subcommand)]
    Memory(MemoryCmd),
    /// Initialize, diagnose, or disable the Total Recall integration.
    #[command(subcommand)]
    Recall(RecallCmd),
    /// Read a document as Markdown (`doc read <path>`): the alias for
    /// `recall doc-read`, spelled the way an agent reaches for it.
    #[command(subcommand, visible_alias = "docs")]
    Doc(DocCmd),
    /// Find files by name, and lines inside them: the alias for `recall
    /// search`, spelled the way an agent reaches for it.
    Search(RecallSearchArgs),
    /// Print the agent-facing map of Rift's serving plane (for agents; rendered
    /// live from this binary's own command tree, so it cannot go stale).
    AgentHelp,
    /// Prune transient shadow refs / orphaned objects (never the canonical chain).
    Gc(GcArgs),
    /// Inspect and maintain the machine-global project registry (the dashboard's
    /// project list).
    #[command(subcommand)]
    Projects(ProjectsCmd),
    /// Inspect and repair this repository family's recorded identity.
    #[command(subcommand)]
    Family(FamilyCmd),
    /// Wire a linked worktree into its family (`adopt`).
    #[command(subcommand, visible_alias = "worktrees")]
    Worktree(WorktreeCmd),
    /// Configure policy and the checkpoint remote.
    Configure(ConfigureArgs),
    /// Agent + git hook entry (JSON on stdin). Hidden: for agents, not humans.
    #[command(hide = true, alias = "hook", disable_help_flag = true)]
    Hooks(HooksArgs),
}

/// `rift search <QUERY>` — the search surface, and it is an ALIAS in the same
/// sense `rift doc read` is: it parses into exactly the arguments `rift recall
/// search` parses into and routes to the same function, so there is one verb
/// with two spellings rather than two verbs that have to be kept in step.
///
/// `rift doc …` — the document surface. One verb today, and it is an ALIAS:
/// `rift doc read` parses into exactly the arguments `rift recall doc-read`
/// parses into and routes to the same function, so there is one verb with two
/// spellings rather than two verbs that have to be kept in step.
#[derive(Subcommand, Debug)]
pub(crate) enum DocCmd {
    /// A document as Markdown, from the sidecar Rift holds for it.
    Read(RecallDocArgs),
}

/// `rift viewer [TARGET] [--repo PATH]`.
#[derive(Args, Debug)]
pub(crate) struct ViewerArgs {
    /// Checkpoint id/prefix or local Git commit-ish/ref. Checkpoint identity wins.
    #[arg(value_name = "TARGET")]
    pub(crate) target: Option<String>,
    /// Repository to inspect transiently without registering it.
    #[arg(long, value_name = "PATH")]
    pub(crate) repo: Option<std::path::PathBuf>,
}

/// `rift init` / `rift setup`.
#[derive(Args, Debug)]
pub(crate) struct InitArgs {
    /// Skip automatic Total Recall initialization and native agent wiring.
    #[arg(long)]
    pub(crate) no_recall: bool,
}

/// `rift recall …` — the **agent-facing serving plane**: the closed
/// skill-verb surface an agent shells out to, plus Recall's lifecycle and
/// diagnostics. Distinct from `rift memory …`, which answers the same questions
/// for a *human* (it supplies the known state for you; here the agent states
/// what it already holds).
#[derive(Subcommand, Debug)]
pub(crate) enum RecallCmd {
    /// Idempotently enable Recall and configure every first-class client.
    Init,
    /// Show per-client configuration, capability, and native wiring state.
    Status,
    /// Remove only Rift-owned native entries; Recall data remains intact.
    Disable,
    /// Manifest first: what changed, what you already hold, what to retrieve.
    KnownState(RecallRankedArgs),
    /// Changed vs unchanged files, given the content hashes you already hold.
    DiffSince(RecallDeltaArgs),
    /// Is a file still what you last saw? (`unchanged` / `changed` / `invalid`.)
    FileIfChanged(RecallTargetArgs),
    /// The changed lines of a file since the revision you hold, with content.
    ChangedHunks(RecallHunksArgs),
    /// A hash-bound summary — reported `stale`, never served, once its source moved.
    ReadSummary(RecallTargetArgs),
    /// Exact bytes by handle. Policy-gated; `--reason` is mandatory and audited.
    RetrieveExact(RecallExactArgs),
    /// Where the context tokens go (descriptive awareness; never a directive).
    CostHotspots(RecallHotspotArgs),
    /// One ordered, budgeted, position-aware evidence pack.
    ContextPack(RecallRankedArgs),
    /// A document as Markdown: the sidecar Rift holds for a `.docx`, `.pdf`,
    /// spreadsheet or slide deck, instead of the file's own bytes.
    DocRead(RecallDocArgs),
    /// Which paths in this checkout a query is about, ranked by match and by
    /// what this family works on — instead of listing and reading your way to
    /// them.
    Search(RecallSearchArgs),
}

/// The query-aware arguments the ranking-sensitive verbs require: ranking
/// without a stated task and focus set is guesswork, so the surface refuses it.
#[derive(Args, Debug)]
pub(crate) struct RecallRankedArgs {
    /// What you are trying to do right now (required).
    #[arg(long, value_name = "TEXT")]
    pub(crate) task: String,
    /// A file currently in focus; repeat for several (at least one required).
    #[arg(long = "file", value_name = "PATH", required = true)]
    pub(crate) files: Vec<String>,
    /// An error signature currently in play; repeatable.
    #[arg(long = "error", value_name = "SIGNATURE")]
    pub(crate) errors: Vec<String>,
    /// A content hash you already hold; repeatable (drives `known_unchanged`).
    #[arg(long = "known-hash", value_name = "SHA256")]
    pub(crate) known_hashes: Vec<String>,
    /// Token ceiling for the response (clamped to the per-turn budget).
    #[arg(long, value_name = "TOKENS")]
    pub(crate) token_budget: Option<u32>,
    // The reason is audited two ways: a fixed neutral tag in
    // `memd_audit_event.reason`, and this text in `serve_grant_event` —
    // stored, never re-served. Kept out of the doc comment because `rift
    // agent-help` renders doc comments verbatim into an agent's context, and a
    // table name is not something the agent can act on.
    /// Widen containment past this checkout's branch, with an audited reason.
    #[arg(long = "cross-branch", value_name = "REASON")]
    pub(crate) cross_branch: Option<String>,
}

/// `rift recall changed-hunks <PATH> --known-hash <SHA256>` — the pull the
/// per-turn manifest's own hint teaches, and the older ranked form.
///
/// The two shapes are one verb because they answer one question with different
/// amounts of the caller's own state. Given a path and the revision the caller
/// holds, the answer is that file's changed lines. Given neither, the answer is
/// the ranked one, which needs the task and focus set every ranking-sensitive
/// verb needs — so both are optional here and the surface refuses the ranked
/// call without them rather than the parser refusing the pull.
#[derive(Args, Debug)]
pub(crate) struct RecallHunksArgs {
    /// The repo-relative path to ask about (with `--known-hash`).
    #[arg(value_name = "PATH")]
    pub(crate) target: Option<String>,
    /// The revision you hold: its content hash, or the leading 16+ hex of one.
    #[arg(long = "known-hash", value_name = "SHA256")]
    pub(crate) known_hashes: Vec<String>,
    /// What you are trying to do right now (required unless you name a path and `--known-hash`).
    #[arg(long, value_name = "TEXT")]
    pub(crate) task: Option<String>,
    /// A file currently in focus; repeat for several (required unless you name a path and `--known-hash`).
    #[arg(long = "file", value_name = "PATH")]
    pub(crate) files: Vec<String>,
    /// An error signature currently in play; repeatable.
    #[arg(long = "error", value_name = "SIGNATURE")]
    pub(crate) errors: Vec<String>,
    /// Token ceiling for the response (clamped to the per-turn budget).
    #[arg(long, value_name = "TOKENS")]
    pub(crate) token_budget: Option<u32>,
    // The reason is audited two ways: a fixed neutral tag in
    // `memd_audit_event.reason`, and this text in `serve_grant_event` —
    // stored, never re-served. Kept out of the doc comment because `rift
    // agent-help` renders doc comments verbatim into an agent's context, and a
    // table name is not something the agent can act on.
    /// Widen containment past this checkout's branch, with an audited reason.
    #[arg(long = "cross-branch", value_name = "REASON")]
    pub(crate) cross_branch: Option<String>,
}

/// `rift recall diff-since` — the delta against the hashes you already hold.
#[derive(Args, Debug)]
pub(crate) struct RecallDeltaArgs {
    /// A content hash you already hold; repeatable. With none, everything reads
    /// as changed — which is the honest answer for an agent holding nothing.
    #[arg(long = "known-hash", value_name = "SHA256")]
    pub(crate) known_hashes: Vec<String>,
    /// Token ceiling for the response (clamped to the per-turn budget).
    #[arg(long, value_name = "TOKENS")]
    pub(crate) token_budget: Option<u32>,
    // The reason is audited two ways: a fixed neutral tag in
    // `memd_audit_event.reason`, and this text in `serve_grant_event` —
    // stored, never re-served. Kept out of the doc comment because `rift
    // agent-help` renders doc comments verbatim into an agent's context, and a
    // table name is not something the agent can act on.
    /// Widen containment past this checkout's branch, with an audited reason.
    #[arg(long = "cross-branch", value_name = "REASON")]
    pub(crate) cross_branch: Option<String>,
}

/// `rift recall file-if-changed <path>` / `read-summary <path>`.
#[derive(Args, Debug)]
pub(crate) struct RecallTargetArgs {
    /// The repo-relative path to ask about.
    #[arg(value_name = "PATH")]
    pub(crate) target: String,
    /// A content hash you already hold for it; repeatable.
    #[arg(long = "known-hash", value_name = "SHA256")]
    pub(crate) known_hashes: Vec<String>,
    // The reason is audited two ways: a fixed neutral tag in
    // `memd_audit_event.reason`, and this text in `serve_grant_event` —
    // stored, never re-served. Kept out of the doc comment because `rift
    // agent-help` renders doc comments verbatim into an agent's context, and a
    // table name is not something the agent can act on.
    /// Widen containment past this checkout's branch, with an audited reason.
    #[arg(long = "cross-branch", value_name = "REASON")]
    pub(crate) cross_branch: Option<String>,
}

/// `rift recall doc-read <path>` (alias `rift doc read <path>`).
///
/// One positional and nothing else. A sheet or page range would mean cutting
/// the Markdown up, which needs a parser this program does not have and will
/// not grow; the whole converted document is the unit, and the answer names the
/// file when it is dearer than the document it stands in for.
#[derive(Args, Debug, Clone)]
pub(crate) struct RecallDocArgs {
    /// The document to read as Markdown (repo-relative or absolute).
    #[arg(value_name = "PATH")]
    pub(crate) target: String,
}

/// `rift recall search <QUERY> [--content]` (alias `rift search <QUERY>`).
///
/// One positional and one flag. The query is the finder's grammar — words to
/// match against paths, with `*.rs` for an extension, `/src/` for a path
/// segment and `!tests/` for an exclusion — and there is no `--cross-branch`,
/// because the answer is this checkout's own file list and no branch axis
/// contains it. A flag with nothing behind it is worse than no flag.
#[derive(Args, Debug, Clone)]
pub(crate) struct RecallSearchArgs {
    /// What to look for: words matched against paths, with `*.rs` (extension),
    /// `/src/` (path segment) and `!tests/` (exclusion) constraints
    #[arg(value_name = "QUERY")]
    pub(crate) query: String,
    /// Also return the matching lines from the tracked files that ranked
    /// (a literal, matched without regard to case)
    #[arg(long)]
    pub(crate) content: bool,
}

/// `rift recall retrieve-exact <handle> --reason <why>`.
#[derive(Args, Debug)]
pub(crate) struct RecallExactArgs {
    /// The handle to retrieve (e.g. `file:src/lib.rs@sha256:<hash>`).
    #[arg(value_name = "HANDLE")]
    pub(crate) handle: String,
    /// Why the exact bytes are needed. Mandatory, recorded in the serve audit.
    #[arg(long, value_name = "TEXT")]
    pub(crate) reason: String,
    // The reason is audited two ways: a fixed neutral tag in
    // `memd_audit_event.reason`, and this text in `serve_grant_event` —
    // stored, never re-served. Kept out of the doc comment because `rift
    // agent-help` renders doc comments verbatim into an agent's context, and a
    // table name is not something the agent can act on.
    /// Widen containment past this checkout's branch, with an audited reason.
    #[arg(long = "cross-branch", value_name = "REASON")]
    pub(crate) cross_branch: Option<String>,
}

/// `rift recall cost-hotspots`.
#[derive(Args, Debug)]
pub(crate) struct RecallHotspotArgs {
    /// How many hotspots to report.
    #[arg(long, value_name = "N", default_value_t = 5)]
    pub(crate) top_n: u32,
    // The reason is audited two ways: a fixed neutral tag in
    // `memd_audit_event.reason`, and this text in `serve_grant_event` —
    // stored, never re-served. Kept out of the doc comment because `rift
    // agent-help` renders doc comments verbatim into an agent's context, and a
    // table name is not something the agent can act on.
    /// Widen containment past this checkout's branch, with an audited reason.
    #[arg(long = "cross-branch", value_name = "REASON")]
    pub(crate) cross_branch: Option<String>,
}

/// `rift enable/disable --agent <x>`.
#[derive(Args, Debug)]
pub(crate) struct AgentArgs {
    /// The agent to wire (e.g. `claude-code`).
    #[arg(long)]
    pub(crate) agent: String,
}

/// `rift backfill [--from <projection>] [--dry-run]`.
///
/// Its own verb rather than a flag on `rift rebuild`: rebuild walks the
/// checkpoint branch and takes no arguments, while this walks git history and
/// may read a foreign file by path. One verb, one meaning.
#[derive(Args, Debug)]
pub(crate) struct BackfillArgs {
    /// A set-aside projection to re-import the non-derivable rows from
    /// (sessions, turns, model calls, token usage, costs). Opened READ-ONLY:
    /// it is never created, never migrated and never written.
    #[arg(long = "from", value_name = "PROJECTION")]
    pub(crate) from: Option<String>,
    /// Report what would be written and write nothing.
    #[arg(long = "dry-run")]
    pub(crate) dry_run: bool,
}

/// `rift import --from-legacy`.
///
/// The flag names the *format*, never the tool that wrote it.
/// There is no `--from-<toolname>` alias and there never was one to retire.
#[derive(Args, Debug)]
pub(crate) struct ImportArgs {
    /// Bulk-read this agent's local transcript store for the repository into
    /// read-only checkpoints (searchable/explainable, never resumable).
    /// Idempotent; the chain positions one run appends are its import epoch.
    #[arg(value_name = "AGENT", conflicts_with = "from_legacy")]
    pub(crate) agent: Option<String>,
    /// Report what would be imported without writing anything.
    #[arg(long, requires = "agent")]
    pub(crate) dry_run: bool,
    /// Import only this session id.
    #[arg(long, value_name = "ID", requires = "agent")]
    pub(crate) session: Option<String>,
    /// Read transcripts from this directory instead of the agent's default
    /// per-repo store.
    #[arg(long, value_name = "DIR", requires = "agent")]
    pub(crate) path: Option<std::path::PathBuf>,
    /// Read checkpoints written in the pre-Rift on-disk format. Read-only:
    /// nothing in the source repository is written, moved or deleted.
    #[arg(long = "from-legacy")]
    pub(crate) from_legacy: bool,
}

/// `rift chain …` — the checkpoint order recorded in `refs/rift/chain/v1`.
///
/// `init` is its own typed verb rather than a step inside `init`/`rebuild`
/// because it CREATES an append-only artifact from a one-time reading of
/// history: it can only ever run once per repository, and a creation that can
/// only happen once should have an author. `status` is the read-only half.
#[derive(Subcommand, Debug)]
pub(crate) enum ChainCmd {
    /// Record the canonical checkpoint order from the checkpoint branch's own
    /// history. Refuses if the chain ref already exists (append-only), and
    /// refuses rather than linearizing a branch that contains a merge.
    Init,
    /// Show how many positions the order records, and its head (read-only).
    Status,
}

/// `rift explain <ref>`.
#[derive(Args, Debug)]
pub(crate) struct ExplainArgs {
    /// A checkpoint id (or a prefix).
    pub(crate) reference: String,
}

/// `rift why <path>[:LINE] [--line N]`.
#[derive(Args, Debug)]
pub(crate) struct WhyArgs {
    /// A file tracked in this worktree, optionally with a line (`src/a.rs:42`).
    #[arg(value_name = "PATH[:LINE]")]
    pub(crate) spec: String,
    /// The line to explain (alternative to the `:LINE` suffix). One line only —
    /// `rift blame --line N-M` answers ranges.
    #[arg(long, value_name = "N")]
    pub(crate) line: Option<String>,
}

/// `rift blame <path> [--line N|N-M]`.
#[derive(Args, Debug)]
pub(crate) struct BlameArgs {
    /// A file tracked in this worktree.
    #[arg(value_name = "PATH")]
    pub(crate) path: String,
    /// Answer for one line (`--line 12`) or an inclusive range (`--line 12-20`)
    /// instead of the whole file.
    #[arg(long, value_name = "N|N-M")]
    pub(crate) line: Option<String>,
}

/// `rift doctor [--fix]`.
#[derive(Args, Debug)]
pub(crate) struct DoctorArgs {
    /// Apply each finding's safe fix: finalize stuck sessions, drain the
    /// queue, heal a chain TAIL gap, level the projection watermark. Interior
    /// chain gaps and divergence are never auto-fixed.
    #[arg(long)]
    pub(crate) fix: bool,
}

/// `rift clean [--dry-run] [--ttl-days N]`.
#[derive(Args, Debug)]
pub(crate) struct CleanArgs {
    /// Report what would be removed without deleting anything.
    #[arg(long)]
    pub(crate) dry_run: bool,
    /// Transients older than this many days are eligible.
    #[arg(long, value_name = "N", default_value_t = 14)]
    pub(crate) ttl_days: u32,
}

/// `rift gc`.
#[derive(Args, Debug)]
pub(crate) struct GcArgs {
    /// Report what would be pruned without deleting anything.
    #[arg(long)]
    pub(crate) dry_run: bool,
}

/// `rift configure`.
#[derive(Args, Debug)]
pub(crate) struct ConfigureArgs {
    /// Set the checkpoint remote URL.
    #[arg(long)]
    pub(crate) checkpoint_remote: Option<String>,

    /// The configure subcommand (global preference setters).
    #[command(subcommand)]
    pub(crate) cmd: Option<ConfigureCmd>,
}

/// `rift configure <setter>` — global (per-user) preference setters, distinct
/// from the per-repo `.rift/config.toml` flags above.
#[derive(Subcommand, Debug)]
pub(crate) enum ConfigureCmd {
    /// Set when the Canopy startup splash plays. Writes only the splash key
    /// of the global canopy preferences.
    SplashMode(SplashModeArgs),
}

/// `rift configure splash-mode <always|first-run|never>`.
#[derive(Args, Debug)]
pub(crate) struct SplashModeArgs {
    /// When the splash plays.
    #[arg(value_enum)]
    pub(crate) mode: SplashModeArg,
}

/// The splash-mode choices (the labels are the canopy-prefs contract).
#[derive(Copy, Clone, Debug, ValueEnum)]
pub(crate) enum SplashModeArg {
    /// Play on every launch.
    Always,
    /// Play only on the first run (the default preference).
    FirstRun,
    /// Never play.
    Never,
}

impl SplashModeArg {
    /// The canopy-prefs label this choice persists as.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::FirstRun => "first-run",
            Self::Never => "never",
        }
    }
}

/// `rift hooks …` — the trailing args are handed verbatim to the hook router.
#[derive(Args, Debug)]
pub(crate) struct HooksArgs {
    /// `<agent> <verb>` | `git <name> …` | `install …` | `--schema`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub(crate) rest: Vec<String>,
}

/// `rift bundle <cmd>` — the offline transfer unit.
#[derive(Subcommand, Debug)]
pub(crate) enum BundleCmd {
    /// Write a self-contained bundle of this repository's capture refs.
    Export(BundleExportArgs),
    /// Verify and apply a bundle (all-or-nothing; set-union).
    Import(BundleImportArgs),
}

/// `rift dispatch` — the deterministic window report.
///
/// There is no relative window (`--since 7d`): a report whose bounds depend on
/// when it ran cannot be compared with itself, and byte-identical `--json` for
/// one window is this verb's exit criterion.
#[derive(Args, Debug)]
pub(crate) struct DispatchArgs {
    /// Window start, inclusive: a UTC day (`YYYY-MM-DD`) or epoch milliseconds.
    #[arg(long, value_name = "WHEN")]
    pub(crate) since: Option<String>,

    /// Window end, inclusive: a UTC day (`YYYY-MM-DD`) or epoch milliseconds.
    #[arg(long, value_name = "WHEN")]
    pub(crate) until: Option<String>,

    /// Scope to the checkpoints one branch's commits reach, resolved from the
    /// `Rift-Checkpoint` trailers and the shards' own anchor commits.
    #[arg(long, value_name = "BRANCH")]
    pub(crate) branch: Option<String>,

    /// Render the report as Markdown instead of plain text.
    #[arg(long)]
    pub(crate) md: bool,

    /// Emit a budgeted evidence pack (pack-spine shaped) instead of prose.
    #[arg(long = "for-agent")]
    pub(crate) for_agent: bool,
}

/// `rift review` — the ReviewPack for the commits in scope.
#[derive(Args, Debug)]
pub(crate) struct ReviewArgs {
    /// Window start, inclusive: a UTC day (`YYYY-MM-DD`) or epoch milliseconds.
    #[arg(long, value_name = "WHEN")]
    pub(crate) since: Option<String>,

    /// Window end, inclusive: a UTC day (`YYYY-MM-DD`) or epoch milliseconds.
    #[arg(long, value_name = "WHEN")]
    pub(crate) until: Option<String>,

    /// Scope to the checkpoints one branch's commits reach.
    #[arg(long, value_name = "BRANCH")]
    pub(crate) branch: Option<String>,

    /// Which sections and budget: `full` (default), `signals`, or `risk`.
    #[arg(long, value_name = "PROFILE")]
    pub(crate) profile: Option<String>,
}

/// `rift investigate` — seed an investigation.
///
/// A SEED is an identifier: a repo-relative path the capture projection has
/// recorded a `file_revision` for, a 64-hex checkpoint id, or `sha256:<hex>`. Anything else is free text, which reaches
/// a pack only as the trust chokepoint's neutral digest — which is exactly what
/// `--allow-untrusted-seed` admits.
#[derive(Args, Debug)]
pub(crate) struct InvestigateArgs {
    /// The seed: a path, a checkpoint id, or `sha256:<hex>`.
    #[arg(value_name = "SEED")]
    pub(crate) seed: Option<String>,

    /// Admit a non-identifier seed as a neutral digest (never verbatim).
    #[arg(long = "allow-untrusted-seed")]
    pub(crate) allow_untrusted_seed: bool,

    /// A finding from the `investigate` skill: `<kind>:<handle>::<text>`.
    /// Repeatable. Accepted findings are UNVERIFIED notes.
    #[arg(long = "finding", value_name = "FINDING")]
    pub(crate) finding: Vec<String>,
}

/// `rift recap` — offline usage analytics.
///
/// `--agent` and `--model` choose the split axis and are mutually exclusive:
/// one report, one axis, so a reader never has to ask which column they are
/// looking at.
#[derive(Args, Debug)]
pub(crate) struct RecapArgs {
    /// Window start, inclusive: a UTC day (`YYYY-MM-DD`) or epoch milliseconds.
    #[arg(long, value_name = "WHEN")]
    pub(crate) since: Option<String>,

    /// Window end, inclusive: a UTC day (`YYYY-MM-DD`) or epoch milliseconds.
    #[arg(long, value_name = "WHEN")]
    pub(crate) until: Option<String>,

    /// Split by agent (the default).
    #[arg(long, conflicts_with = "model")]
    pub(crate) agent: bool,

    /// Split by model instead of by agent.
    #[arg(long)]
    pub(crate) model: bool,
}

/// `rift bundle export`.
#[derive(Args, Debug)]
pub(crate) struct BundleExportArgs {
    /// Resume point: the chain segment covers everything AFTER this cursor.
    /// Defaults to the origin cursor (`rift-cursor/1:0:`) — the whole chain.
    #[arg(long, value_name = "CURSOR")]
    pub(crate) since: Option<String>,

    /// Where to write the bundle (default `rift-bundle.riftbundle` in the repo).
    #[arg(long, value_name = "FILE")]
    pub(crate) out: Option<std::path::PathBuf>,
}

/// `rift bundle import`.
#[derive(Args, Debug)]
pub(crate) struct BundleImportArgs {
    /// The bundle file to verify and apply.
    #[arg(value_name = "FILE")]
    pub(crate) file: std::path::PathBuf,
}

/// `rift sync <cmd>` — the push queue's on-demand surface.
///
/// There is no `--remote` flag on purpose: the destination is repository
/// policy (`.rift/config.toml` `[checkpoint] remote`), not a per-invocation
/// argument, so a mistyped command line can never aim a capture ledger
/// somewhere new.
#[derive(Subcommand, Debug)]
pub(crate) enum SyncCmd {
    /// What is queued, and where it would be shipped.
    Status,
    /// Ship everything queued to the configured checkpoint remote.
    Flush,
}

#[derive(Subcommand, Debug)]
pub(crate) enum CheckpointCmd {
    /// List recorded checkpoints (newest first).
    List,
    /// Show one checkpoint's roots and chain position.
    Show(IdArgs),
    /// Explain one checkpoint natively from its shard: metadata, digests, the
    /// redacted prompt, files, tokens/cost, attribution. No target = HEAD.
    Explain(CheckpointExplainArgs),
}

/// `rift checkpoint explain [ID|SHA] [--full|--short]`.
#[derive(Args, Debug)]
pub(crate) struct CheckpointExplainArgs {
    /// A checkpoint id (or unique prefix), a commit sha/ref, or nothing for
    /// HEAD's claimed checkpoint (a squash resolves to the latest by
    /// created-at).
    #[arg(value_name = "ID|SHA")]
    pub(crate) target: Option<String>,
    /// Also render the compact transcript.
    #[arg(long)]
    pub(crate) full: bool,
    /// Header, sessions and digest summary only.
    #[arg(long)]
    pub(crate) short: bool,
}

/// `rift session …`.
#[derive(Subcommand, Debug)]
pub(crate) enum SessionCmd {
    /// The session-state surface: ages, agents, models, token totals —
    /// journal (live + ended) and projection (archived) sessions together.
    List,
    /// One session in full: state, age, identity, token totals, checkpoints.
    Info(IdArgs),
    /// Show one session's phase and captured events.
    Show(IdArgs),
    /// Finalize stale sessions through the state machine (IdleTimeout →
    /// condense → CondenseDone). With an ID, stop exactly that session.
    Stop(StopArgs),
    /// Resume a stopped session: check out its branch, restore the native
    /// transcript from the checkpoint shard, and print the agent's resume
    /// command (plus a recall known-state cursor). No branch = list resumable
    /// sessions across worktrees.
    Resume(ResumeArgs),
    /// Adopt an out-of-band session: link its existing native transcript to
    /// the last commit as a checkpoint. Amending the commit to add the
    /// trailer requires an explicit yes (a TTY confirm, or --force in a
    /// pipe). Never runs from hooks.
    Attach(AttachArgs),
}

/// `rift session stop [ID]`.
#[derive(Args, Debug)]
pub(crate) struct StopArgs {
    /// The session to stop (omit to finalize every stale session).
    pub(crate) id: Option<String>,
}

/// `rift session attach SESSION_ID --agent A [--force]`.
#[derive(Args, Debug)]
pub(crate) struct AttachArgs {
    /// The agent's native session id.
    pub(crate) session_id: String,
    /// The agent that produced the session.
    #[arg(long)]
    pub(crate) agent: String,
    /// Acknowledge the amend without a terminal confirm (pipes/scripts).
    #[arg(long, short = 'f')]
    pub(crate) force: bool,
}

/// `rift session resume [BRANCH] [--force]`.
#[derive(Args, Debug)]
pub(crate) struct ResumeArgs {
    /// The branch to resume (omit to list resumable sessions).
    pub(crate) branch: Option<String>,
    /// Say yes where resume would ask: overwrite an existing local
    /// transcript, resume from a checkpoint behind the branch tip (nothing is
    /// ever reset), or check out a remote-only branch.
    #[arg(long, short = 'f')]
    pub(crate) force: bool,
}

/// A positional id (checkpoint / session), possibly a prefix.
#[derive(Args, Debug)]
pub(crate) struct IdArgs {
    /// The id (or a unique prefix).
    pub(crate) id: String,
}

/// `rift projects …` — the registry's CLI: the dashboard's project
/// list, inspectable and sweepable without a TUI keypress.
#[derive(Subcommand, Debug)]
pub(crate) enum ProjectsCmd {
    /// List every registered project: path, label, presence.
    List(ProjectsListArgs),
    /// Sweep registered rows whose path no longer exists as a directory,
    /// appending one superseding removed event per victim (append-only —
    /// history stays). A present-but-not-a-repo row is never pruned.
    ///
    /// Caveat: a temporarily unmounted network/external volume also reads as
    /// missing — run `--dry-run` first when in doubt.
    Prune(PruneArgs),
}

/// `rift family …` — the repository family's minted identity.
///
/// A family's ids are minted once, into `<common_dir>/rift-family/family.json`,
/// and read thereafter. Everything else in Rift reads them; this is the one
/// surface that writes them, and only when asked in so many words.
#[derive(Subcommand, Debug)]
pub(crate) enum FamilyCmd {
    /// Replace the recorded identity, appending the reason to the family's
    /// history. Nothing else rewrites the manifest — `rift doctor --fix`
    /// prints this command rather than running it — so reach for it only when
    /// the recorded ids are wrong and you know which ids are right.
    Remint(RemintArgs),
}

/// `rift worktree …` — the family's members.
#[derive(Subcommand, Debug)]
pub(crate) enum WorktreeCmd {
    /// Every member of this repository's family: name, path, branch,
    /// main/linked, whether it is provisioned, its live sessions, its last
    /// capture, and whether git would prune it.
    List,
    /// One member in full: its row, the client wiring Rift can verify without
    /// writing anything, and the worktree observations recorded in it. No
    /// PATH = the worktree you are standing in.
    Status(WorktreeStatusArgs),
    /// Wire this worktree for every agent the family has enabled, using the
    /// family's binary of record. Idempotent; never touches git hooks, tracked
    /// files, or global client configuration.
    Adopt(AdoptArgs),
    /// Report members whose checkout is gone while their administrative entry
    /// survives. Reports only; `--yes` runs `git worktree prune`.
    Prune(WorktreePruneArgs),
}

/// `rift worktree status [PATH]`.
#[derive(Args, Debug)]
pub(crate) struct WorktreeStatusArgs {
    /// The member to report on. Defaults to the worktree you are standing in.
    #[arg(value_name = "PATH")]
    pub(crate) path: Option<std::path::PathBuf>,
}

/// `rift worktree prune [--yes]`.
#[derive(Args, Debug)]
pub(crate) struct WorktreePruneArgs {
    /// Required to run anything. Without it this verb reports and executes
    /// nothing: removing an administrative entry is the operator's decision,
    /// and a verb asked what is prunable must not answer with an act.
    #[arg(long)]
    pub(crate) yes: bool,
}

/// `rift worktree adopt [PATH]`.
#[derive(Args, Debug)]
pub(crate) struct AdoptArgs {
    /// The member to adopt. Defaults to the worktree you are standing in.
    #[arg(value_name = "PATH")]
    pub(crate) path: Option<std::path::PathBuf>,
}

/// `rift family remint (--adopt-main | --fresh) --yes`.
#[derive(Args, Debug)]
pub(crate) struct RemintArgs {
    /// Re-adopt the main work tree's own ids — the rule a first mint uses.
    #[arg(long, conflicts_with = "fresh")]
    pub(crate) adopt_main: bool,
    /// Derive ids from the common dir instead of adopting a work tree's.
    #[arg(long)]
    pub(crate) fresh: bool,
    /// Required. The previous ids keep whatever data is filed under them; this
    /// command does not move it, and there is no undo but another remint.
    #[arg(long)]
    pub(crate) yes: bool,
    /// Why, recorded verbatim in `family.history.jsonl`.
    #[arg(long, value_name = "TEXT")]
    pub(crate) reason: Option<String>,
}

/// `rift projects list`.
#[derive(Args, Debug)]
pub(crate) struct ProjectsListArgs {
    /// Also list each repository's linked worktrees, indented under it.
    ///
    /// A linked worktree is a second path to one repository and is never a
    /// registry row of its own, so this is the only way the CLI
    /// names the members of a family. Off, the output is the registered rows
    /// and nothing else.
    #[arg(long)]
    pub(crate) worktrees: bool,
}

/// `rift projects prune`.
#[derive(Args, Debug)]
pub(crate) struct PruneArgs {
    /// Print the rows that would be removed without writing anything.
    #[arg(long)]
    pub(crate) dry_run: bool,
    /// Sweep the per-path projection directories orphaned by the family mint
    /// instead of registry rows. Reports path, id and event
    /// count; removes nothing without `--yes`.
    #[arg(long)]
    pub(crate) orphan_projections: bool,
    /// Required to DELETE an orphan projection. The facts inside one — per-turn
    /// usage, the serve audit — are not recoverable into the family store, so
    /// the removal is irreversible and asks for the word.
    #[arg(long, requires = "orphan_projections")]
    pub(crate) yes: bool,
}

/// `rift memory …` — the human-facing recall surface. Reuses the
/// transport-independent Recall dispatch: the same envelopes, budgets, and
/// policy gates as the agent-facing `rift recall` skill verbs.
#[derive(Subcommand, Debug)]
pub(crate) enum MemoryCmd {
    /// What changed since a checkpoint (id or unique prefix)?
    DiffSince(PointArgs),
    /// Does a file differ from what was last captured?
    FileIfChanged(PathArgs),
    /// The files consuming the most context tokens.
    CostHotspots(TopArgs),
    /// Measured repeated/avoidable context rates over the captured sessions
    /// (appends an immutable metric snapshot, so the history is tamper-evident).
    RereadReport,
}

/// `rift memory diff-since <point>`.
#[derive(Args, Debug)]
pub(crate) struct PointArgs {
    /// A checkpoint id (or a unique prefix).
    pub(crate) point: String,
}

/// `rift memory file-if-changed <path>`.
#[derive(Args, Debug)]
pub(crate) struct PathArgs {
    /// A repo-relative file path.
    pub(crate) path: String,
}

/// `rift memory cost-hotspots`.
#[derive(Args, Debug)]
pub(crate) struct TopArgs {
    /// How many hotspots to report.
    #[arg(long, default_value_t = 5)]
    pub(crate) top: u32,
}

/// The stable exit-code contract. Scripts and CI depend on these.
/// `rift verify` overrides them with its own granular contract.
pub(crate) mod exit {
    /// Success.
    pub(crate) const OK: i32 = 0;
    /// Generic, command-specific failure.
    pub(crate) const FAILURE: i32 = 1;
    /// Usage error (bad args/flags) — clap's convention.
    pub(crate) const USAGE: i32 = 2;
    /// Not initialized / not a Rift repo (run `rift init`).
    pub(crate) const NOT_INIT: i32 = 3;
    /// A precondition failed (e.g. an unsupported platform, or no adapter for
    /// the named agent — the environment is not in the state the command needs).
    pub(crate) const PRECONDITION: i32 = 4;
    /// Refused on an integrity check (chain rewrite, migration-ledger tamper).
    /// Not retryable: the bytes on offer are not the bytes we recorded.
    /// (sysexits `EX_DATAERR`.)
    pub(crate) const INTEGRITY: i32 = 65;
    /// Unexpected internal error (a bug) — never silently 0.
    pub(crate) const INTERNAL: i32 = 70;
    // `RETRYABLE` (75, `EX_TEMPFAIL`) was here. Its only producer was the
    // capture-backend download, which was deleted with the wrapper on
    // 2026-08-11; no command can emit it now. A documented exit code nothing
    // returns is a promise no test can keep, so it goes rather than lingering
    // behind an `allow(dead_code)`.
}

/// A command result: an exit code plus dual-rendered output (human + JSON), so the
/// dispatcher can honor `--json` without each command knowing the output mode.
#[derive(Debug, Clone)]
pub(crate) struct Outcome {
    /// The process exit code.
    pub(crate) code: i32,
    /// Human-readable text (stdout).
    pub(crate) human: String,
    /// The `--json` document.
    pub(crate) json: serde_json::Value,
}

impl Outcome {
    /// A success outcome (exit 0).
    #[must_use]
    pub(crate) fn ok(human: impl Into<String>, json: serde_json::Value) -> Self {
        Self {
            code: exit::OK,
            human: human.into(),
            json,
        }
    }

    /// An outcome with an explicit exit code.
    #[must_use]
    pub(crate) fn coded(code: i32, human: impl Into<String>, json: serde_json::Value) -> Self {
        Self {
            code,
            human: human.into(),
            json,
        }
    }
}
