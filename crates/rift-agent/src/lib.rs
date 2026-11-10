//! The Rift **plugin SDK**: the [`AgentAdapter`] trait every agent
//! integration implements, typed event [`builders`](event), an
//! [`idempotency_key`] helper, [`TokenUsage`] extraction types, and a
//! [`conformance`] kit so an adapter can prove its mapping is correct.
//!
//! An adapter is a **stateless, no-authority** mapping: it translates one
//! agent's native hook lifecycle (`verb` + the agent's own JSON payload) into
//! zero-or-more [`CanonicalEvent`]s. It has **no** redaction, hashing, or
//! chain-write authority — the ingest layer (`rift hooks`) owns those. An adapter
//! fills the *structural* fields of an event (`kind`, `trust`, and the schema
//! version); the ingest layer stamps identity/time/content-hash from the hook
//! envelope. This keeps the trust boundary at `rift hooks`, even for third-party
//! adapters that run in their own process.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use rift_core::{AgentId, CanonicalEvent, EventKind, Sha256Id, Trust};

pub mod conformance;
pub mod fold;
pub mod respond;
pub mod settle;
pub mod shell_read;
pub mod window_journal;

/// The canonical-event schema version this SDK builds (bumped additively).
pub const SCHEMA_VERSION: u16 = 1;

/// The largest slice, in bytes, that may be treated as text an agent's context
/// actually holds.
///
/// A record saying "this range was delivered" has to be true of the *context*,
/// not merely of the tool, and the two thresholds that decide it are different
/// per channel — which is why this number is measured rather than reasoned
/// from the first one that came to hand.
///
/// * A **command's** result stops arriving inline at roughly 30,000
///   characters: past that the client saves it to a file and hands the model a
///   path and a preview. That is the bound on a shell read, and a shell read
///   records a whole-file or unbounded range anyway, so no decision rests on
///   it.
/// * A **read's** result was measured never to be persisted at all. A read
///   without a limit hands over the whole file and truncates in place on a
///   25,000-token cap, with a partial-view hint naming the page it stopped at
///   and a field on the result object saying how far it reached. So a read
///   that arrived at all arrived as bytes, and 25,000 tokens — about 96 KiB at
///   the density the tokenizer measures here — is the real ceiling on what a
///   context can be holding.
///
/// 96 KiB is that measured bound, with the line numbering a read adds already
/// inside it. Anything larger is remembered without ever being treated as
/// held.
///
/// It is a **default**, not a limit: the value in force is a policy setting,
/// and it is raised only against a measured threshold, never on a hunch.
pub const DEFAULT_MAX_SLICE_BYTES: usize = 96 * 1024;

/// The shortest slice a decision may act on, in lines.
///
/// A decline is not free: the model reads a reason frame instead of the range,
/// and below some length that frame costs more than the read it replaces.
/// A plain frame measures **61 tokens** against about 4 tokens a line, so a
/// decline starts paying for itself at 16 lines and the floor sits exactly
/// there; a slice under it is still decided and recorded, so the pool the floor
/// forgoes is a figure rather than a guess.
///
/// It is a **default**, not a limit: the value in force is a policy setting,
/// sized by the window that watched the lever record without acting.
///
/// It moved from 24 to 28 when the frame grew the provenance handle, and from
/// 28 to 16 when the plain frame dropped the pull and provenance hints, which
/// pointed at a diff that is empty by construction. The floor is the frame's break-even, so it follows the
/// frame's measured cost;
/// `the_reason_frame_pays_for_itself_at_the_floor` in `rift-recall-serve` is
/// where the arithmetic runs, because that crate has both the sealed renderer
/// and the real tokenizer and this one has neither.
pub const DEFAULT_MIN_SLICE_LINES: u32 = 16;

/// Where a read request's last line comes from — the difference between a
/// range a tool's own contract bounds and one nothing bounds.
///
/// The distinction is the whole point. A recorded range must never claim more
/// than the tool can have delivered. A range whose end no artifact pins is
/// represented by [`ReadSpan::Unknown`] instead of appearing here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HiKind {
    /// The caller bounded the range itself: the tool can have delivered
    /// through this line and no further, before the file's own end clamps it.
    Explicit(u32),
    /// The caller bounded nothing, and the family's default window **is**
    /// pinned: through this line.
    ///
    /// A family whose limit-less delivery runs to the end of the file names
    /// [`HiKind::END_OF_FILE`] here rather than a line number, because an
    /// adapter never opens a file and so never knows how many lines one has.
    /// The clamp in `ResolvedRead::new` — the one place the bytes are already
    /// in hand — turns it into the file's own last line.
    DefaultWindow(u32),
}

impl HiKind {
    /// The end of a delivery that runs to the file's last line, whichever line
    /// that turns out to be.
    ///
    /// A sentinel rather than a real line number, and deliberately the largest
    /// one there is: every path that reads a range clamps it against the
    /// file's length, so a window stated this way can only ever shrink to the
    /// truth and never grow past it.
    pub const END_OF_FILE: u32 = u32::MAX;
}

/// Which of a client's two result channels a read's bytes came back on.
///
/// The distinction exists because the channels have different ceilings, and
/// the tighter one is not the one a read tool has. A read tool's result is
/// handed to the model in place, cut short at the client's own cap; terminal
/// output over a measured size is written to a file instead, and what the
/// model receives is a pointer and a short preview. The bytes ran either way,
/// so the same command line is the same read — but what the window was handed
/// is not the same thing, and only the channel says which ceiling applies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReadChannel {
    /// A read tool, whose result reaches the model inline.
    #[default]
    Tool,
    /// A terminal command, whose output the client may put in a file.
    Shell,
}

/// Which part of a file a native read call asks for.
///
/// This is closed on purpose: a line range, a byte range whose request and
/// result semantics have both been verified, or no trustworthy range at all.
/// `Unknown` is not a small conservative range. It is a statement that no
/// coverage claim can be made, so resolution must fail open without recording
/// a read, observation, or denial.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadSpan {
    /// A 1-based line range with a pinned source for its inclusive end.
    Lines {
        /// First line, 1-based.
        lo: u32,
        /// Inclusive end and the pinned authority it came from.
        hi_kind: HiKind,
    },
    /// The file's **last** `count` lines, whichever lines those turn out to
    /// be: `tail -n N`, and a read narrowed by a trailing `| tail -n N`.
    ///
    /// The mirror image of [`HiKind::END_OF_FILE`], and resolved in the same
    /// one place for the same reason. A window counted from the bottom is a
    /// range a tool's own contract bounds exactly — `N` lines, ending at the
    /// last one — but *which* line numbers those are is a function of the
    /// file's length, and an adapter never opens a file. So the count travels
    /// and `ResolvedRead::new`, where the bytes are already in hand for
    /// hashing, turns it into `(total - N + 1) ..= total`.
    ///
    /// It is deliberately not a [`ReadSpan::Lines`] with a sentinel: a
    /// sentinel start would have to be smaller than every real line number,
    /// and the clamp that made it honest would be a clamp in the wrong
    /// direction.
    LastLines {
        /// How many lines from the end, at least one.
        count: u32,
    },
    /// A half-open byte range with pinned request and delivered-result
    /// semantics. No current alpha adapter emits this variant yet.
    VerifiedBytes {
        /// Zero-based byte offset under the pinned native contract.
        offset: u64,
        /// Delivered byte count under the pinned result contract.
        length: u64,
    },
    /// Scattered runs of lines, sorted and disjoint: what a filtered output
    /// such as a search's delivers. Learned from the delivered bytes, never
    /// from a request, so no command line resolves to it and it names no
    /// single range a decision could act on.
    Sparse(Vec<crate::window_journal::Interval>),
    /// Pagination or delivery semantics are not pinned on both sides.
    Unknown,
}

impl ReadSpan {
    /// A pinned line span.
    #[must_use]
    pub const fn lines(lo: u32, hi_kind: HiKind) -> Self {
        Self::Lines { lo, hi_kind }
    }

    /// The conservative inclusive line bounds before the file clamps them.
    ///
    /// [`ReadSpan::LastLines`] answers `None` here, and that is not a claim
    /// that it has no range: it is the statement that its range is not
    /// derivable from the request alone. The one place that has the file's
    /// length resolves it; every caller that only has the request is right to
    /// treat it as a range it cannot name.
    #[must_use]
    pub const fn conservative_lines(&self) -> Option<(u32, u32)> {
        match *self {
            Self::Lines { lo, hi_kind } => {
                let hi = match hi_kind {
                    HiKind::Explicit(hi) | HiKind::DefaultWindow(hi) if hi > lo => hi,
                    HiKind::Explicit(_) | HiKind::DefaultWindow(_) => lo,
                };
                Some((lo, hi))
            }
            Self::LastLines { .. }
            | Self::VerifiedBytes { .. }
            | Self::Sparse(_)
            | Self::Unknown => None,
        }
    }
}

/// A read-shaped tool call, reduced to the facts a coverage record needs:
/// which file, which verified span it asked for, and which channel carried the
/// answer back.
///
/// Built by [`AgentAdapter::read_request`] from field names a committed pin
/// carries. A call whose arguments step outside that pinned set — pages of a
/// PDF, cells of a notebook, an argument added after the pin was taken — is
/// not a read request at all, because its range is not lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadRequest {
    /// The path exactly as the payload spelled it. Not canonicalized here: an
    /// adapter has no filesystem authority.
    pub path: String,
    /// The requested range and the authority available for it.
    pub span: ReadSpan,
    /// How the result travelled back to the model, which is what decides the
    /// size a delivery stops being vouchable at.
    pub channel: ReadChannel,
}

impl ReadRequest {
    /// The conservative inclusive line bounds before the file clamps them.
    /// Byte and unknown spans return `None`: the line journal must not invent a
    /// conversion or a delivery result.
    #[must_use]
    pub const fn conservative_lines(&self) -> Option<(u32, u32)> {
        self.span.conservative_lines()
    }
}

/// The `sha256` of bytes a payload carried — the hash a file *should* have
/// once a whole-file write lands, so that "the agent holds this file" becomes
/// an equality something later checks rather than an assumption that nothing
/// reformatted it on the way in.
///
/// It lives here, beside the type that holds the result, so no adapter needs a
/// hashing dependency of its own to answer [`AgentAdapter::edit_request`].
#[must_use]
pub fn content_sha256(bytes: &[u8]) -> Sha256Id {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(&hasher.finalize());
    Sha256Id::from_bytes(out)
}

/// What an edit-shaped tool call did, in terms a later verification can check
/// rather than assume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditKind {
    /// The whole file was replaced by bytes the payload carries, so their hash
    /// is the hash the file should have afterwards. Anything else means
    /// something rewrote it in between.
    Write {
        /// [`content_sha256`] of the content the caller supplied.
        content_sha256: Sha256Id,
    },
    /// One stretch of text was replaced by another. The replacement is carried
    /// so the site can be *located in the new bytes*; the count of lines
    /// removed is carried so the shift is a subtraction rather than a guess.
    Edit {
        /// The text the caller wrote in.
        new_string: String,
        /// How many lines the replaced text spanned.
        old_string_lines: u32,
        /// Whether the caller asked for every occurrence.
        replace_all: bool,
    },
    /// Several stretches replaced in one call, applied in order, all or none:
    /// a multi-edit, or a patch whose hunks each carry their new side whole.
    /// Each is carried so it can be located in the new bytes on its own, under
    /// the same rule one replacement is held to.
    Regions {
        /// The replacements, in the order the call applied them.
        regions: Vec<Replacement>,
    },
    /// The file changed and no before-and-after pair follows from the payload
    /// — a notebook cell, a multi-edit whose fields no pin states, a patch
    /// whose chunks Rift cannot read. The
    /// old bytes' coverage must stop counting; no new coverage is derived.
    Other,
}

/// One stretch of a [`EditKind::Regions`] call: what was written in, and how
/// many lines it took out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replacement {
    /// The text the caller wrote in.
    pub new_string: String,
    /// How many lines the replaced text spanned, counted the way
    /// [`EditKind::Edit`] counts them.
    pub old_string_lines: u32,
    /// Whether the caller asked for every occurrence.
    pub replace_all: bool,
}

/// An edit-shaped tool call, reduced to the file it changed and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditRequest {
    /// The path exactly as the payload spelled it.
    pub path: String,
    /// What the call did to it.
    pub kind: EditKind,
}

/// What a search-shaped tool call asked for.
///
/// The three are separated because what may be done about them differs: a name
/// search has an answer that fits in a sentence, a content search never does,
/// and a listing is a name search wearing another name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchKind {
    /// A file-name pattern over the tree.
    Glob {
        /// The pattern as written.
        pattern: String,
    },
    /// A search **inside** file contents. Never answered from an index and
    /// never substituted; the only additive help is a result cap, and only
    /// where the family has an argument for one.
    Grep {
        /// Whether the caller already declared a result cap. A caller that
        /// declared one — including an explicit "no cap" — chose it, and the
        /// choice is left alone.
        has_head_limit: bool,
    },
    /// The contents of one directory, by name, optionally filtered.
    Listing {
        /// The directory as written.
        dir: String,
        /// The name pattern, where the tool takes one.
        pattern: Option<String>,
    },
}

/// A search-shaped tool call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchRequest {
    /// Which of the three shapes it is.
    pub kind: SearchKind,
}

/// **The per-family tool-argument table, as data** — the input side of the
/// same discipline [`respond::CAPABILITIES`] applies to the output side.
///
/// A row says: this family calls this tool, Rift reads it as this shape, and
/// these are the argument names it reads it through. A row exists only where
/// that family's own published surface — or, where the surface is silent,
/// recorded runs marked as observations — is pinned into this tree as a
/// committed artifact under
/// `crates/rift-agent-*/fixtures/*-tool-field-surface.md`. Each adapter's
/// `pinned_tool_field_surface` test asserts its family's rows here and the
/// rows in its fixture are the same set, in both directions, so neither can
/// grow alone.
///
/// The asymmetry that earns the ceremony mirrors the response side's. There,
/// an invented field is a broken agent. Here, an argument read as something it
/// is not — a page number taken for a line number, an end line taken for a
/// count — records that a context holds text it never received, and every
/// decision downstream inherits the error in silence.
pub mod tool_fields {
    use std::fmt;

    use rift_core::AgentId;

    /// What Rift reads a tool call as.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ToolShape {
        /// A call that delivers lines of a file.
        Read,
        /// A call that changes a file.
        Edit,
        /// A call that returns names or matches.
        Search,
        /// A call whose argument is a shell command line, read as a file
        /// read or a listing only when it matches the grammar the family's
        /// own artifact pins, and left alone otherwise.
        Shell,
        /// A tool this adapter deliberately reads nothing from, named so that
        /// silence can be told from an oversight.
        None,
    }

    impl ToolShape {
        /// The token this shape carries in a pinned row.
        #[must_use]
        pub const fn as_str(self) -> &'static str {
            match self {
                Self::Read => "read",
                Self::Edit => "edit",
                Self::Search => "search",
                Self::Shell => "shell",
                Self::None => "none",
            }
        }
    }

    /// One tool's row: the name on the wire, the shape, and the pinned
    /// `key=value` facts about it.
    ///
    /// The facts are one string, spelled exactly as the fixture row spells its
    /// tail, rather than a struct with a field per fact. The three families
    /// disagree about what there is to say — one nests its arguments, one has
    /// no result cap to name, one has no read tool at all — so a struct wide
    /// enough for all of them would carry an empty slot for every family it
    /// does not describe, and an empty slot reads as an absence of evidence
    /// where the truth is evidence of an absence.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ToolFieldRow {
        /// The `tool_name` this family puts on the wire.
        pub tool: &'static str,
        /// What Rift reads it as.
        pub shape: ToolShape,
        /// The pinned facts: space-separated `key=value` pairs.
        pub facts: &'static str,
    }

    impl ToolFieldRow {
        /// This row's value for one key, or `None` when it states none.
        #[must_use]
        pub fn fact(&self, key: &str) -> Option<&'static str> {
            self.facts
                .split(' ')
                .filter_map(|pair| pair.split_once('='))
                .find(|(k, _)| *k == key)
                .map(|(_, value)| value)
        }
    }

    impl fmt::Display for ToolFieldRow {
        /// Renders the row exactly as a fixture spells it, so the artifact and
        /// the table can be compared as text.
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                f,
                "tool={} shape={} {}",
                self.tool,
                self.shape.as_str(),
                self.facts
            )
        }
    }

    /// A row, spelled the way a fixture spells one.
    const fn row(tool: &'static str, shape: ToolShape, facts: &'static str) -> ToolFieldRow {
        ToolFieldRow { tool, shape, facts }
    }

    /// The table itself, one entry per `(family, tool)`.
    pub const TOOL_FIELDS: &[(AgentId, ToolFieldRow)] = &[
        (
            AgentId::CLAUDE_CODE,
            row(
                "Read",
                ToolShape::Read,
                "path=file_path lo=offset lo_base=1 hi=limit hi_kind=count default_window=to-end-of-file",
            ),
        ),
        (
            AgentId::CLAUDE_CODE,
            row("Glob", ToolShape::Search, "kind=glob pattern=pattern"),
        ),
        (
            AgentId::CLAUDE_CODE,
            row("Grep", ToolShape::Search, "kind=grep head_limit=head_limit"),
        ),
        (
            AgentId::CLAUDE_CODE,
            row(
                "Edit",
                ToolShape::Edit,
                "kind=edit path=file_path new=new_string old=old_string replace_all=replace_all",
            ),
        ),
        (
            AgentId::CLAUDE_CODE,
            row(
                "Write",
                ToolShape::Edit,
                "kind=write path=file_path content=content",
            ),
        ),
        (
            AgentId::CLAUDE_CODE,
            row("MultiEdit", ToolShape::Edit, "kind=other path=file_path"),
        ),
        (
            AgentId::CLAUDE_CODE,
            row("NotebookEdit", ToolShape::Edit, "kind=other path=file_path"),
        ),
        (
            AgentId::CLAUDE_CODE,
            row(
                "mcp__acp__Write",
                ToolShape::Edit,
                "kind=other path=file_path",
            ),
        ),
        (
            AgentId::CLAUDE_CODE,
            row(
                "mcp__acp__Edit",
                ToolShape::Edit,
                "kind=other path=file_path",
            ),
        ),
        (
            AgentId::CLAUDE_CODE,
            row(
                "Bash",
                ToolShape::Shell,
                "command=command grammar=composed-read-and-listing",
            ),
        ),
        (
            AgentId::CODEX,
            row(
                "apply_patch",
                ToolShape::Edit,
                "kind=regions command=command path=from-a-single-file-header regions=update-chunks",
            ),
        ),
        (
            AgentId::CODEX,
            row(
                "Bash",
                ToolShape::Shell,
                "command=command grammar=composed-read-and-listing",
            ),
        ),
        (
            AgentId::ANTIGRAVITY,
            row(
                "view_file",
                ToolShape::Read,
                "args=toolCall.args path=AbsolutePath lo=StartLine lo_base=undocumented hi=EndLine hi_kind=line flag=IsSkillFile envelope=toolAction,toolSummary default_window=unpinned",
            ),
        ),
        (
            AgentId::ANTIGRAVITY,
            row(
                "write_to_file",
                ToolShape::Edit,
                "kind=write args=toolCall.args path=TargetFile content=CodeContent",
            ),
        ),
        (
            AgentId::ANTIGRAVITY,
            row(
                "replace_file_content",
                ToolShape::Edit,
                "kind=edit args=toolCall.args path=TargetFile new=ReplacementContent old=TargetContent replace_all=AllowMultiple",
            ),
        ),
        (
            AgentId::ANTIGRAVITY,
            row(
                "multi_replace_file_content",
                ToolShape::Edit,
                "kind=other args=toolCall.args path=TargetFile",
            ),
        ),
        (
            AgentId::ANTIGRAVITY,
            row(
                "find_by_name",
                ToolShape::Search,
                "kind=listing args=toolCall.args dir=SearchDirectory pattern=Pattern",
            ),
        ),
        (
            AgentId::ANTIGRAVITY,
            row(
                "list_dir",
                ToolShape::Search,
                "kind=listing args=toolCall.args dir=DirectoryPath",
            ),
        ),
        (
            AgentId::ANTIGRAVITY,
            row("grep_search", ToolShape::Search, "kind=grep head_limit=none"),
        ),
        (
            AgentId::ANTIGRAVITY,
            row(
                "run_command",
                ToolShape::Shell,
                "args=toolCall.args command=CommandLine cwd=Cwd grammar=composed-read-and-listing",
            ),
        ),
    ];

    /// The fence a fixture's tool-field rows live in, so prose that
    /// necessarily discusses rows while explaining them cannot smuggle one
    /// into the set.
    const TOOL_FIELD_FENCE: &str = "```rift-tool-field-rows";

    /// This family's rows, rendered in the one-line form a fixture spells them
    /// in. The rendering *is* the comparison: an adapter proves its pin and
    /// this table agree by comparing two sorted lists of strings, with no
    /// per-field destructuring to fall out of date.
    #[must_use]
    pub fn tool_field_rows(agent: &AgentId) -> Vec<String> {
        let mut rows: Vec<String> = TOOL_FIELDS
            .iter()
            .filter(|(a, _)| a == agent)
            .map(|(_, row)| row.to_string())
            .collect();
        rows.sort();
        rows
    }

    /// One row of the table, by family and tool.
    #[must_use]
    pub fn tool_field_row(agent: &AgentId, tool: &str) -> Option<ToolFieldRow> {
        TOOL_FIELDS
            .iter()
            .find(|(a, row)| a == agent && row.tool == tool)
            .map(|(_, row)| *row)
    }

    /// One row's value for a key — the accessor an adapter's tests use to
    /// build a payload out of the *pinned* argument names, rather than out of
    /// names retyped beside the code they are meant to check.
    #[must_use]
    pub fn pinned_fact(agent: &AgentId, tool: &str, key: &str) -> Option<&'static str> {
        tool_field_row(agent, tool).and_then(|row| row.fact(key))
    }

    /// The tool-field rows a pinned fixture states, read only from inside its
    /// `rift-tool-field-rows` fence.
    ///
    /// The parse is deliberately dumb — whole lines, taken as written —
    /// because a clever parse is one that can be talked into accepting a row
    /// the artifact does not actually contain.
    #[must_use]
    pub fn pinned_tool_field_rows(fixture: &str) -> Vec<String> {
        let mut rows = Vec::new();
        let mut inside = false;
        for line in fixture.lines() {
            if inside {
                if line.trim_end() == "```" {
                    inside = false;
                } else {
                    rows.push(line.trim().to_owned());
                }
            } else if line.trim_end() == TOOL_FIELD_FENCE {
                inside = true;
            }
        }
        rows.sort();
        rows
    }
}

/// An error from mapping a native hook to canonical events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdapterError {
    /// A verb the adapter does not understand. Never silently dropped — the
    /// ingest layer records it as a skipped hook, with the verb named.
    UnknownVerb(String),
    /// The native payload could not be interpreted (shape the adapter requires
    /// is absent/ill-typed). Carries a reason only — never payload bytes.
    MalformedPayload(String),
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownVerb(v) => write!(f, "unknown verb: {v}"),
            Self::MalformedPayload(r) => write!(f, "malformed payload: {r}"),
        }
    }
}

impl std::error::Error for AdapterError {}

/// Provider-reported token usage extracted from a native payload — the most
/// trustworthy token counts there are, so they are carried verbatim. Absent
/// fields stay `None`: adapters must **never** fabricate a count, because an
/// invented number is worse than an honest gap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenUsage {
    /// Input/prompt tokens.
    pub input: Option<u64>,
    /// Output/completion tokens.
    pub output: Option<u64>,
    /// Cache-read tokens (where the provider reports a cache split).
    pub cache_read: Option<u64>,
    /// Cache-write/creation tokens.
    pub cache_write: Option<u64>,
}

/// Provenance for a model identifier normalized from an agent hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelSource {
    /// The running client/provider included the model in the hook payload.
    RuntimeHook,
    /// A concrete client setting supplied the model; runtime identity was absent.
    ConfiguredClient,
}

impl ModelSource {
    /// Stable projection/UI token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RuntimeHook => "runtime_hook",
            Self::ConfiguredClient => "configured_client",
        }
    }
}

/// A normalized model identifier with explicit provenance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelFact {
    /// Provider/client model identifier.
    pub model: String,
    /// How Rift learned the identifier.
    pub source: ModelSource,
}

/// Where a hook payload's working directory came from — the `basis` token
/// `session_worktree` records.
///
/// Two agents, two spellings of the same fact: Claude Code and Codex put the
/// agent's working directory on every payload as `cwd`; Antigravity has no
/// `cwd` at all and carries `workspacePaths[]` instead. The projection stores
/// which one answered, because "the agent said it was here" and "the workspace
/// the IDE had open" are different claims and an auditor is entitled to know
/// which one a row rests on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CwdSource {
    /// The payload's own `cwd` field.
    PayloadCwd,
    /// The first entry of Antigravity's `workspacePaths[]`.
    WorkspacePath,
}

impl CwdSource {
    /// The stable projection token — exactly the `session_worktree.basis`
    /// vocabulary (`rift_store::schema::SESSION_WORKTREE_BASES`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PayloadCwd => "payload_cwd",
            Self::WorkspacePath => "workspace_path",
        }
    }
}

/// A working directory an agent's payload named, with its provenance.
///
/// Paired with its source for the same reason [`ModelFact`] is: the ingest
/// layer records the basis beside the value, and a path whose basis had to be
/// guessed from the agent token is a path whose basis is a guess.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CwdFact {
    /// The directory, exactly as the payload spelled it. Not canonicalized
    /// here — an adapter has no filesystem authority (that is the ingest
    /// layer's, which resolves the path against a repository it discovers).
    pub path: String,
    /// Which payload field answered.
    pub source: CwdSource,
}

/// The payload's own `cwd`, when it names a non-empty one — the convention
/// every adapter but Antigravity's shares, lifted out so an adapter that builds
/// its facts by hand cannot forget it (this is a `HookFacts` literal in five
/// crates; four of them build it field by field).
#[must_use]
pub fn payload_cwd(native: &serde_json::Value) -> Option<CwdFact> {
    native
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .filter(|p| !p.is_empty())
        .map(|path| CwdFact {
            path: path.to_owned(),
            source: CwdSource::PayloadCwd,
        })
}

/// Safe scalar facts extracted from one native hook payload.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HookFacts {
    /// Model, if the payload states one.
    pub model: Option<ModelFact>,
    /// Native tool name.
    pub tool_name: Option<String>,
    /// Child/subagent native session id.
    pub child_session: Option<String>,
    /// Read-shaped tool path.
    pub read_path: Option<String>,
    /// The working directory the payload named, with its provenance — the
    /// worktree an agent hook was fired in. `None` when the
    /// payload names none, and the ingest layer falls back to the hook
    /// process's own cwd (`basis: process_cwd`) rather than inventing one.
    pub cwd: Option<CwdFact>,
}

impl TokenUsage {
    /// Whether any field is present (i.e. the provider surfaced usage at all).
    #[must_use]
    pub fn is_present(&self) -> bool {
        self.input.is_some()
            || self.output.is_some()
            || self.cache_read.is_some()
            || self.cache_write.is_some()
    }
}

/// Per-agent compatibility notes: which lifecycle points the agent exposes,
/// whether it surfaces token usage, and whether capture is best-effort. Honestly
/// recording gaps here propagates to attribution confidence, which is lowered
/// when an agent's hooks are sparse.
#[derive(Clone, Debug)]
pub struct CompatNotes {
    /// The agent these notes describe.
    pub agent: AgentId,
    /// Lifecycle points the agent does **not** expose (e.g. "no compaction signal").
    pub lifecycle_gaps: &'static [&'static str],
    /// Whether the agent surfaces provider-reported token usage.
    pub surfaces_token_usage: bool,
    /// Whether capture is best-effort (the agent has no stable hook API).
    pub best_effort: bool,
    /// A one-line human note.
    pub note: &'static str,
}

/// The root-field fact convention every adapter starts from — the body of
/// [`AgentAdapter::facts`]'s default, lifted out so an override can refine it.
///
/// Split out because the alternative is what actually happened: an adapter that
/// needed one extra field re-derived the whole shape by hand, and the copy
/// dropped `read_path`. An override should call this and add, never restate.
#[must_use]
pub fn default_facts(_verb: &str, native: &serde_json::Value) -> HookFacts {
    let string = |key: &str| {
        native
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let tool_name = string("tool_name");
    let read_path = tool_name
        .as_deref()
        .filter(|name| name.eq_ignore_ascii_case("read"))
        .and_then(|_| {
            native
                .get("tool_input")
                .and_then(|v| v.get("file_path"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    HookFacts {
        model: string("model").map(|model| ModelFact {
            model,
            source: ModelSource::RuntimeHook,
        }),
        tool_name,
        child_session: string("subagent_session_id")
            .or_else(|| string("child_session_id"))
            .or_else(|| string("agent_session_id")),
        read_path,
        cwd: payload_cwd(native),
    }
}

/// Build a *structural* canonical event: `kind` + `trust` + the SDK schema
/// version. Identity (`session`, ordinals), time, and content hash are stamped by
/// the ingest layer from the hook envelope — an adapter has no authority to set
/// them. The returned event's `session`/`payload_sha256` are placeholders.
#[must_use]
pub fn event(kind: EventKind, trust: Trust) -> CanonicalEvent {
    CanonicalEvent {
        schema_version: SCHEMA_VERSION,
        session: String::new(),
        turn_ordinal: 0,
        event_ordinal: 0,
        kind,
        trust,
        occurred_at_ms: 0,
        payload_sha256: Sha256Id::from_bytes([0u8; 32]),
    }
}

/// Which turn-boundary signals an adapter's verb table can emit (2026-08-29).
///
/// `start` is true when some native verb maps to `PromptSubmit` or
/// `TurnStart`; `end` when some verb maps to `TurnEnd`. The bridge reads this
/// to synthesize the Start an End-only adapter (Antigravity) never sends, so
/// its turn ordinal advances and every Stop lands as its own boundary; a
/// Start-capable adapter never gets a synthesized Start. Declared per adapter
/// and checked at the door: [`conformance::check`] refuses a declaration that
/// disagrees with the adapter's own golden cases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnSignals {
    /// The adapter emits a turn-opening signal (`PromptSubmit` / `TurnStart`).
    pub start: bool,
    /// The adapter emits a turn-closing signal (`TurnEnd`).
    pub end: bool,
}

impl TurnSignals {
    /// Both signals — the default declaration.
    pub const BOTH: Self = Self {
        start: true,
        end: true,
    };

    /// The signals a set of emitted kinds implies — what the golden cases say.
    #[must_use]
    pub fn from_kinds<'a>(kinds: impl IntoIterator<Item = &'a EventKind>) -> Self {
        let mut out = Self {
            start: false,
            end: false,
        };
        for k in kinds {
            match k {
                EventKind::PromptSubmit | EventKind::TurnStart => out.start = true,
                EventKind::TurnEnd => out.end = true,
                _ => {}
            }
        }
        out
    }
}

/// An adapter mapping one agent's native verbs onto canonical events.
///
/// Implementors are small and declarative: they know the agent's verb names and
/// payload shape and nothing else. They produce events only — redaction and
/// chaining are Rift's job.
pub trait AgentAdapter: Send + Sync {
    /// The agent this adapter handles.
    fn agent_id(&self) -> AgentId;

    /// Map a native hook (`verb` + the agent's own payload) to zero-or-more
    /// canonical events.
    ///
    /// # Errors
    /// [`AdapterError::UnknownVerb`] for a verb outside [`verbs`](Self::verbs)
    /// (never silently dropped); [`AdapterError::MalformedPayload`] if a required
    /// shape is absent.
    fn map(
        &self,
        verb: &str,
        native: &serde_json::Value,
    ) -> Result<Vec<CanonicalEvent>, AdapterError>;

    /// The native verbs this adapter understands (for `rift enable` validation).
    fn verbs(&self) -> &'static [&'static str];

    /// Per-agent compatibility notes.
    fn compat_notes(&self) -> CompatNotes;

    /// The turn-boundary signals this adapter's verb table can emit. Default:
    /// both. An adapter whose verbs never open or never close a turn overrides
    /// this honestly — [`conformance::check`] asserts the declaration equals
    /// what its golden cases emit.
    fn turn_signals(&self) -> TurnSignals {
        TurnSignals::BOTH
    }

    /// Normalize safe scalar identity facts from the native payload. The
    /// default preserves the original root-field convention for third-party
    /// and older first-party adapters; providers with nested schemas override.
    ///
    /// The body is [`default_facts`], so an adapter that needs the convention
    /// **plus** one agent-specific field can call it and refine the result
    /// rather than re-deriving the whole shape — which is how a per-agent
    /// override silently loses `read_path`.
    fn facts(&self, verb: &str, native: &serde_json::Value) -> HookFacts {
        default_facts(verb, native)
    }

    /// The `additionalContext` ceiling this family's hook configuration must
    /// declare for `verb`, in tokens — for the family that bounds that channel
    /// by a key on the hook's own configuration entry rather than by anything
    /// in a payload.
    ///
    /// Default: `None`, "this family declares no limit", which is the answer
    /// for every family whose pinned surface names no such key. The installer
    /// projects this the way it projects [`verbs`](Self::verbs), so the number
    /// written into a hook file and the number the hook fits its text to are
    /// one number and not two.
    fn declared_context_limit(&self, _verb: &str) -> Option<u32> {
        None
    }

    /// Extract provider-reported token usage from a native payload, if present.
    /// Default: none (the agent doesn't surface usage). Never fabricates.
    fn usage(&self, _verb: &str, _native: &serde_json::Value) -> Option<TokenUsage> {
        None
    }

    /// Read this payload as a request for lines of a file, if that is what it
    /// is — the file, the first line, and how far the tool can have reached.
    ///
    /// Default: `None`, which means "not read-shaped" and costs nothing but a
    /// saving. Only an adapter whose family's argument names are pinned into
    /// this tree overrides it, and only through the names
    /// [`tool_fields::TOOL_FIELDS`] carries.
    ///
    /// Answering `None` is always safe; answering with a range wider than the
    /// tool can have delivered is not, so an argument outside the pinned set
    /// takes the whole call out of the shape rather than being ignored.
    fn read_request(&self, _verb: &str, _native: &serde_json::Value) -> Option<ReadRequest> {
        None
    }

    /// **Every** read this payload performs, in the order it performs them.
    ///
    /// Default: the one [`read_request`](Self::read_request) answers with, and
    /// for every family whose read tool reads one file that is the whole
    /// truth. A family whose reads arrive as a *command line* overrides it,
    /// because one such line routinely reads several files — `cd <dir> && cat
    /// a && cat b` is one call and three facts — and a caller that saw only
    /// the first would record a fraction of what reached the window.
    ///
    /// A member whose delivered lines are not derivable is present here with
    /// [`ReadSpan::Unknown`] rather than absent. It records nothing and can
    /// satisfy no decline either way; what its presence buys is that a caller
    /// deciding about the whole call can see there was a part of it nobody
    /// can answer for.
    fn read_requests(&self, verb: &str, native: &serde_json::Value) -> Vec<ReadRequest> {
        self.read_request(verb, native).into_iter().collect()
    }

    /// Read this payload as a change to a file, if that is what it is.
    ///
    /// Default: `None`. An edit whose site cannot be derived is still an edit
    /// worth reporting — [`EditKind::Other`] — because the point of recording
    /// one is as much to stop trusting the old bytes as to learn the new.
    fn edit_request(&self, _verb: &str, _native: &serde_json::Value) -> Option<EditRequest> {
        None
    }

    /// Read this payload as a search, if that is what it is.
    ///
    /// Default: `None`.
    fn search_request(&self, _verb: &str, _native: &serde_json::Value) -> Option<SearchRequest> {
        None
    }

    /// The one file a **content search** in this payload named, when this
    /// family's pinned surface names one.
    ///
    /// Default: `None`, and that is the answer for every tool argument no
    /// artifact in this tree claims. Today the only pinned spelling is the
    /// shell grammar's own `grep <pattern> <path>`
    /// ([`shell_read::ShellRead::searched_file`]), where the path is part of
    /// the recognition rather than a field read off a call.
    ///
    /// A searched file is not a read file: the window is handed the matching
    /// lines, never the file. It is the accounting of reads that did **not**
    /// happen that asks this question, and answering `None` costs a saving
    /// and nothing else.
    fn searched_file(&self, _verb: &str, _native: &serde_json::Value) -> Option<String> {
        None
    }

    /// The **structured** test that recognizes *this agent's own stop-hook
    /// marker record* in its transcript — the authoritative "the turn's writes
    /// are flushed" signal for the [`settle`] protocol. `None` (the default)
    /// means the agent publishes no such marker and settling falls back to the
    /// quiet window.
    ///
    /// It is a predicate over a parsed record, not a substring, because a
    /// transcript also *quotes* hook command lines back (tool results, error
    /// attachments, queued prompts) and accepting one of those would end the
    /// wait on a half-written transcript. Only the adapter knows its own
    /// record schema, so only the adapter can tell the two apart. See
    /// [`settle::StopSentinel`].
    fn stop_sentinel(&self) -> Option<settle::StopSentinel> {
        None
    }

    /// Extract a provider-reported snapshot of the primary model context at a
    /// boundary whose call-grain semantics this adapter understands. This is
    /// deliberately separate from [`usage`](Self::usage), which may aggregate
    /// a whole turn and include sidechains.
    fn main_context_usage(&self, _verb: &str, _native: &serde_json::Value) -> Option<TokenUsage> {
        None
    }
}

/// The content/identity-derived idempotency key for a canonical event:
/// `sha256(agent ‖ session ‖ verb ‖ turn_ordinal ‖ event_ordinal ‖ payload_sha256)`.
///
/// Deterministic over identity-defining fields (not wall-clock, not receipt
/// order), so the *same* logical event always yields the *same* key regardless of
/// when/how often it's delivered — making re-ingest a true no-op. The
/// `turn_ordinal` separates the same hook fired in different turns; the
/// `event_ordinal` separates the fan-out within one hook (e.g. `ToolCall` then
/// `FileEdit`); the `payload_sha256` separates distinct payloads in the same slot.
#[must_use]
pub fn idempotency_key(
    agent: &AgentId,
    session: &str,
    verb: &str,
    turn_ordinal: u32,
    event_ordinal: u32,
    payload_sha256: &Sha256Id,
) -> Sha256Id {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(agent.as_str().as_bytes());
    h.update([0]);
    h.update(session.as_bytes());
    h.update([0]);
    h.update(verb.as_bytes());
    h.update([0]);
    h.update(turn_ordinal.to_le_bytes());
    h.update([0]);
    h.update(event_ordinal.to_le_bytes());
    h.update([0]);
    h.update(payload_sha256.as_bytes());
    let mut b = [0u8; 32];
    b.copy_from_slice(&h.finalize());
    Sha256Id::from_bytes(b)
}

/// Read a `u64` token-count field from a native payload by JSON path of keys,
/// honestly returning `None` when absent (never fabricates).
#[must_use]
pub fn read_count(native: &serde_json::Value, path: &[&str]) -> Option<u64> {
    let mut cur = native;
    for key in path {
        cur = cur.get(key)?;
    }
    cur.as_u64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_builder_sets_schema_and_leaves_envelope_unset() {
        let e = event(EventKind::ToolCall, Trust::CodeFact);
        assert_eq!(e.schema_version, SCHEMA_VERSION);
        assert!(e.session.is_empty());
        assert_eq!(e.kind, EventKind::ToolCall);
        assert_eq!(e.trust, Trust::CodeFact);
    }

    #[test]
    fn idempotency_key_is_stable_and_ordinal_sensitive() {
        let h = Sha256Id::from_bytes([7u8; 32]);
        let a = idempotency_key(&AgentId::CLAUDE_CODE, "s", "PreToolUse", 1, 0, &h);
        let b = idempotency_key(&AgentId::CLAUDE_CODE, "s", "PreToolUse", 1, 0, &h);
        let event_diff = idempotency_key(&AgentId::CLAUDE_CODE, "s", "PreToolUse", 1, 1, &h);
        let turn_diff = idempotency_key(&AgentId::CLAUDE_CODE, "s", "PreToolUse", 2, 0, &h);
        assert_eq!(a, b, "same logical event → same key");
        assert_ne!(a, event_diff, "a distinct event_ordinal → a distinct key");
        assert_ne!(a, turn_diff, "a distinct turn_ordinal → a distinct key");
    }

    /// **An unpinned range records no lines.**
    ///
    /// This is the rule the whole surface is pinned for. Where a family's own
    /// artifact bounds what a read delivers, the record is that bound; where
    /// nothing bounds it — a view whose range was left out, a command whose
    /// span the line names nowhere — the request stays unknown and produces no
    /// record. Under-recording costs a saving nobody sees. Over-recording says
    /// a context holds text that never reached it.
    ///
    /// Every read-shaped row in the table is swept, so a family that arrives
    /// later cannot be given a window by a reader who assumed one: a row may
    /// say `unpinned`, or it may name a window an artifact settles, and the
    /// vocabulary of the second is closed here rather than open to whatever a
    /// row cares to claim. The type that carries the verdict is checked
    /// against the same rule.
    #[test]
    fn an_unpinned_range_records_no_lines() {
        use tool_fields::{ToolShape, TOOL_FIELDS};

        // `to-end-of-file` is the one window any artifact settles today
        // (Claude Code `Read`, observation O7 of the tool-field pin: below the
        // client's token cap a limit-less read hands over every line, and
        // above it the result says so in a field). A row naming anything else
        // is a window someone assumed.
        let pinned_windows = ["unpinned", "to-end-of-file"];
        let mut read_shaped = 0;
        for (agent, row) in TOOL_FIELDS {
            if row.shape != ToolShape::Read {
                continue;
            }
            read_shaped += 1;
            let window = row.fact("default_window");
            assert!(
                window.is_some_and(|window| pinned_windows.contains(&window)),
                "{}'s {} states the default window {window:?}; a window nobody \
                 published is not a window, and this row is where that would \
                 enter",
                agent.as_str(),
                row.tool
            );
        }
        assert!(read_shaped > 0, "no read-shaped row to hold to the rule");

        let unbounded = ReadRequest {
            path: "/repo/src/lib.rs".to_owned(),
            span: ReadSpan::Unknown,
            channel: ReadChannel::Tool,
        };
        assert_eq!(unbounded.conservative_lines(), None, "no claimed lines");
    }

    /// Every row is well formed, and no family names one tool twice — two
    /// rows for one tool is two answers to "what is this call", and the second
    /// is never reached.
    #[test]
    fn the_tool_field_table_is_well_formed() {
        use tool_fields::{ToolShape, TOOL_FIELDS};

        let mut seen: Vec<(&str, &str)> = Vec::new();
        for (agent, row) in TOOL_FIELDS {
            assert!(!row.tool.is_empty(), "a row with no tool name");
            let pair = (agent.as_str(), row.tool);
            assert!(
                !seen.contains(&pair),
                "{} names {} twice",
                agent.as_str(),
                row.tool
            );
            seen.push(pair);

            assert!(!row.facts.is_empty(), "{} states no facts", row.tool);
            for fact in row.facts.split(' ') {
                let (key, value) = fact
                    .split_once('=')
                    .unwrap_or_else(|| panic!("{}: {fact} is not key=value", row.tool));
                assert!(
                    !key.is_empty() && !value.is_empty(),
                    "{}: {fact} has an empty half",
                    row.tool
                );
            }
            // A shape that reads nothing must say why not; a fixture reader
            // deserves the reason, not a blank.
            if row.shape == ToolShape::None {
                assert!(
                    row.fact("reason").is_some(),
                    "{} is read as nothing and gives no reason",
                    row.tool
                );
            }
            // The rendering is what a fixture is compared against, so it has
            // to round-trip through the row's own accessor.
            let rendered = row.to_string();
            assert!(rendered.starts_with(&format!("tool={} shape=", row.tool)));
            assert!(rendered.ends_with(row.facts));
        }
    }

    /// The default answers are the ones that cost a saving rather than the
    /// ones that cost a mistake. An adapter that has said nothing about its
    /// tools must not be read as having said "not read-shaped, go ahead" for
    /// some tools and something else for others — it says nothing, for all.
    #[test]
    fn an_adapter_that_pins_nothing_recognises_nothing() {
        struct Silent;
        impl AgentAdapter for Silent {
            fn agent_id(&self) -> AgentId {
                AgentId::CLAUDE_CODE
            }
            fn map(
                &self,
                _verb: &str,
                _native: &serde_json::Value,
            ) -> Result<Vec<CanonicalEvent>, AdapterError> {
                Ok(Vec::new())
            }
            fn verbs(&self) -> &'static [&'static str] {
                &[]
            }
            fn compat_notes(&self) -> CompatNotes {
                CompatNotes {
                    agent: AgentId::CLAUDE_CODE,
                    lifecycle_gaps: &[],
                    surfaces_token_usage: false,
                    best_effort: true,
                    note: "",
                }
            }
        }

        let payload = serde_json::json!({
            "tool_name": "Read",
            "tool_input": {"file_path": "/repo/a.rs", "offset": 1, "limit": 50},
        });
        assert!(Silent.read_request("PreToolUse", &payload).is_none());
        assert!(Silent.edit_request("PreToolUse", &payload).is_none());
        assert!(Silent.search_request("PreToolUse", &payload).is_none());
    }

    /// A pinned line range reaches as far as its bound says and no further.
    /// Unknown and byte spans do not masquerade as line coverage.
    #[test]
    fn a_recorded_range_never_reaches_past_its_own_bound() {
        let unbounded = ReadRequest {
            path: "/repo/src/lib.rs".to_owned(),
            span: ReadSpan::Unknown,
            channel: ReadChannel::Tool,
        };
        assert_eq!(unbounded.conservative_lines(), None);
        assert_eq!(
            ReadRequest {
                span: ReadSpan::lines(40, HiKind::Explicit(89)),
                ..unbounded.clone()
            }
            .conservative_lines(),
            Some((40, 89))
        );
        assert_eq!(
            ReadRequest {
                span: ReadSpan::lines(40, HiKind::DefaultWindow(2039)),
                ..unbounded.clone()
            }
            .conservative_lines(),
            Some((40, 2039))
        );
        // A degenerate range never reaches backwards past its first line.
        assert_eq!(
            ReadRequest {
                span: ReadSpan::lines(40, HiKind::Explicit(3)),
                ..unbounded
            }
            .conservative_lines(),
            Some((40, 40))
        );
        assert_eq!(
            ReadRequest {
                path: "/repo/src/lib.rs".to_owned(),
                span: ReadSpan::VerifiedBytes {
                    offset: 1_024,
                    length: 512,
                },
                channel: ReadChannel::Tool,
            }
            .conservative_lines(),
            None
        );
    }

    /// The slice ceiling is the **measured** read bound, not the documented
    /// command bound it was first mistaken for.
    ///
    /// The recorded observation is a 25,000-token truncation cap on a read
    /// result that is never written to a file; at the four-bytes-a-token
    /// density this tokenizer measures, that is 100,000 bytes, and the ceiling
    /// sits under it with room for the numbering a read adds. The 30,000
    /// characters in the same fixture bound a *command's* result, which is a
    /// different channel and a different record.
    #[test]
    fn the_slice_ceiling_stays_under_the_measured_read_truncation_cap() {
        let measured_read_token_cap: usize = "25000".parse().expect("the pin states a decimal");
        let bytes_a_token = 4;
        assert_eq!(DEFAULT_MAX_SLICE_BYTES, 96 * 1024);
        assert!(
            DEFAULT_MAX_SLICE_BYTES <= measured_read_token_cap * bytes_a_token,
            "a slice treated as held must fit inside the cap the read was \
             measured to truncate at"
        );
    }

    #[test]
    fn read_count_is_honest_about_absence() {
        let v = serde_json::json!({"usage": {"input_tokens": 12}});
        assert_eq!(read_count(&v, &["usage", "input_tokens"]), Some(12));
        assert_eq!(read_count(&v, &["usage", "output_tokens"]), None);
        assert_eq!(read_count(&v, &["nope"]), None);
    }
    /// The working directory an agent names about itself, and the provenance
    /// that travels with it. The basis is part of the fact
    /// because "the agent said its cwd was here" and "the IDE had this
    /// workspace open" are different claims, and the row records which one it
    /// rests on.
    #[test]
    fn the_root_field_convention_carries_the_payloads_own_cwd_with_its_basis() {
        let facts = default_facts("PreToolUse", &serde_json::json!({"cwd": "/repo/member"}));
        let cwd = facts.cwd.expect("a payload cwd is a fact");
        assert_eq!(cwd.path, "/repo/member");
        assert_eq!(cwd.source, CwdSource::PayloadCwd);
        assert_eq!(cwd.source.as_str(), "payload_cwd");
        assert_eq!(CwdSource::WorkspacePath.as_str(), "workspace_path");

        // Absent and empty are both UNKNOWN — never the empty path, which
        // `discover` would resolve against the hook process's own cwd and so
        // label the wrong worktree.
        assert!(default_facts("PreToolUse", &serde_json::json!({}))
            .cwd
            .is_none());
        assert!(default_facts("PreToolUse", &serde_json::json!({"cwd": ""}))
            .cwd
            .is_none());
        assert!(default_facts("PreToolUse", &serde_json::json!({"cwd": 7}))
            .cwd
            .is_none());
    }
}
