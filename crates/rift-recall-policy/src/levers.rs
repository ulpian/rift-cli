//! The three active-lever tables: the modes a lever may run in, the formats
//! and channels a document lever chooses between, the shipped bounds, and the
//! `[interception]`, `[search]` and `[doc]` tables themselves. Its own module
//! because these are the vocabulary the policy file, the resolution and every
//! status line share, and none of them reads a file or decides anything.

/// How far a lever may act.
///
/// The variants are ordered by strength (`Off < Observe < Enforce`), which is
/// what lets a cap be a `min`: a family can always be held below what the
/// table asked for, and nothing can be lifted above it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// The lever is not consulted: nothing acts and nothing is recorded.
    Off,
    /// The lever reaches its decision and records it; the agent's call runs
    /// exactly as it was written.
    #[default]
    Observe,
    /// The lever acts on the call.
    Enforce,
}

impl Mode {
    /// The spelling the policy file uses and `rift recall status` prints.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Observe => "observe",
            Self::Enforce => "enforce",
        }
    }
}

/// Which document formats the converter may touch. One accepted value today —
/// a closed set rather than a free string, so a typo is a policy that fails to
/// load rather than a silently narrowed set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Formats {
    /// Every extension the converter declares.
    #[default]
    All,
}

impl Formats {
    /// The spelling the policy file uses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
        }
    }
}

/// How a repository prefers a document read to be answered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocChannel {
    /// Amend the read's path to the sidecar. Nothing is declined and the model
    /// is asked for nothing, so this is the primary channel where a family
    /// documents an amendment at all.
    #[default]
    Rewrite,
    /// Decline the read once, naming the sidecar in the reason. The fallback
    /// when an amendment cannot be relied on, and selectable outright by a
    /// repository that would rather be told than amended.
    Redirect,
}

impl DocChannel {
    /// The spelling the policy file uses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rewrite => "rewrite",
            Self::Redirect => "redirect",
        }
    }
}

/// The slice a single decision may cover: 96 KiB, the measured point at which
/// a read result truncates in place (25,000 tokens, never written to a file),
/// so a decision never covers more than the context was going to keep. The
/// roughly 30,000 characters past which a *command's* output stops being held
/// inline bounds a different channel, and a shell read records an unbounded
/// range regardless.
pub const DEFAULT_MAX_SLICE_BYTES: u64 = 96 * 1024;
/// The shortest slice a decision may act on, in lines. A decline replaces the
/// range with a reason frame, and below some length the frame costs more than
/// the read it replaces: **111 tokens of frame against about 4 tokens a line,
/// so a decline starts paying at 28 lines**, and the floor sits exactly there.
/// It is provisional — the observe window prices the pool it forgoes and
/// confirms or moves it. A slice under the floor is still decided and recorded;
/// it is only never acted on.
///
/// It was 24 against a frame the prose called "about 88 tokens". Two things
/// moved it. The frame grew the provenance handle that tells an agent where a
/// change's story is kept; and the 88 was measured with an unusually
/// cheap-to-tokenize hash — on the renderer's own golden fixture the same frame
/// already cost 98, so 24 had drifted a line under its break-even before this.
/// The number is now **measured** rather than quoted, by
/// `the_reason_frame_pays_for_itself_at_the_floor` in `rift-recall-serve` (the
/// one crate that has both the sealed renderer and the real tokenizer), and a
/// reword that grows the frame reds that test. A floor left behind a grown
/// frame is a lever quietly declining the reads it makes more expensive.
pub const DEFAULT_MIN_SLICE_LINES: u32 = 28;
/// The largest file hashed inline on the gate path; past it the read proceeds.
pub const DEFAULT_MAX_INLINE_HASH_BYTES: u64 = 1024 * 1024;
/// Tokens a recovery map may cost after a compaction.
pub const DEFAULT_RECOVERY_BUDGET: u32 = 800;
/// How long a read-time basis snapshot is kept before `rift clean` collects
/// it. The same thirty days a converted document gets, and for the same
/// reason: losing one costs a delta frame and never an answer — the read that
/// produced it can always be made again, and the revision is either still in
/// the checkout or is a blob git holds.
pub const DEFAULT_BASIS_TTL_DAYS: u32 = 30;
/// The head limit added to a content search that carries none.
pub const DEFAULT_GREP_HEAD_LIMIT: u32 = 40;
/// How many ranked paths an answered listing carries.
pub const DEFAULT_GLOB_TOP_N: u32 = 8;
/// Tokens an answered listing may cost.
pub const DEFAULT_ANSWER_BUDGET_TOKENS: u32 = 160;
/// Rows kept per sheet when a table is converted.
pub const DEFAULT_TABLE_ROWS: u32 = 200;
/// The largest document converted at all.
pub const DEFAULT_MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;
/// Below this many characters per page a PDF is treated as having no text
/// layer, and the read is left alone.
pub const DEFAULT_PDF_MIN_CHARS_PER_PAGE: u32 = 200;
/// The deadline a conversion on the gate path is given before the read
/// proceeds and the work is handed to the detached worker.
pub const DEFAULT_CONVERT_BUDGET_MS: u32 = 250;
/// How long an unused sidecar is kept.
pub const DEFAULT_DOC_TTL_DAYS: u32 = 30;

/// Per-family interception overrides. The three families are named fields
/// rather than a map so an unknown family is a policy that fails to load
/// rather than a line that silently does nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AgentModes {
    /// Claude Code's override.
    #[serde(rename = "claude-code")]
    pub claude_code: Option<Mode>,
    /// Codex's override.
    pub codex: Option<Mode>,
    /// Antigravity's override.
    pub antigravity: Option<Mode>,
}

impl AgentModes {
    /// The override written for `family`, if any.
    #[must_use]
    pub fn for_family(&self, family: &str) -> Option<Mode> {
        match family {
            "claude-code" => self.claude_code,
            "codex" => self.codex,
            "antigravity" => self.antigravity,
            _ => None,
        }
    }
}

/// `[interception]`: the read-interception lever and its bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct InterceptionPolicy {
    /// The repository-wide mode, before any per-family override or cap.
    pub mode: Mode,
    /// The largest slice one decision may cover.
    pub max_slice_bytes: u64,
    /// The shortest slice one decision may act on, in lines.
    pub min_slice_lines: u32,
    /// The largest file hashed inline on the gate path.
    pub max_inline_hash_bytes: u64,
    /// Tokens a recovery map may cost.
    pub recovery_budget: u32,
    /// How long a read-time basis snapshot is kept.
    pub basis_ttl_days: u32,
    /// `[interception.agents]`.
    pub agents: AgentModes,
}

impl Default for InterceptionPolicy {
    fn default() -> Self {
        Self {
            mode: Mode::Enforce,
            max_slice_bytes: DEFAULT_MAX_SLICE_BYTES,
            min_slice_lines: DEFAULT_MIN_SLICE_LINES,
            max_inline_hash_bytes: DEFAULT_MAX_INLINE_HASH_BYTES,
            recovery_budget: DEFAULT_RECOVERY_BUDGET,
            basis_ttl_days: DEFAULT_BASIS_TTL_DAYS,
            agents: AgentModes::default(),
        }
    }
}

/// `[search]`: the search levers and their bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SearchPolicy {
    /// The repository-wide mode, before any cap.
    pub mode: Mode,
    /// The head limit added to a content search that carries none.
    pub grep_head_limit: u32,
    /// How many ranked paths an answered listing carries.
    pub glob_top_n: u32,
    /// Tokens an answered listing may cost.
    pub answer_budget_tokens: u32,
}

impl Default for SearchPolicy {
    fn default() -> Self {
        Self {
            mode: Mode::Enforce,
            grep_head_limit: DEFAULT_GREP_HEAD_LIMIT,
            glob_top_n: DEFAULT_GLOB_TOP_N,
            answer_budget_tokens: DEFAULT_ANSWER_BUDGET_TOKENS,
        }
    }
}

/// `[doc]`: the document levers and their bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DocPolicy {
    /// The repository-wide mode, before any cap.
    pub mode: Mode,
    /// Which formats may be converted.
    pub formats: Formats,
    /// Which channel this repository prefers for a document read.
    pub channel: DocChannel,
    /// Rows kept per sheet.
    pub table_rows: u32,
    /// The largest document converted at all.
    pub max_input_bytes: u64,
    /// The characters-per-page floor below which a PDF is left alone.
    pub pdf_min_chars_per_page: u32,
    /// The gate-path conversion deadline.
    pub convert_budget_ms: u32,
    /// How long an unused sidecar is kept.
    pub ttl_days: u32,
}

impl Default for DocPolicy {
    fn default() -> Self {
        Self {
            mode: Mode::Enforce,
            formats: Formats::All,
            channel: DocChannel::Rewrite,
            table_rows: DEFAULT_TABLE_ROWS,
            max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
            pdf_min_chars_per_page: DEFAULT_PDF_MIN_CHARS_PER_PAGE,
            convert_budget_ms: DEFAULT_CONVERT_BUDGET_MS,
            ttl_days: DEFAULT_DOC_TTL_DAYS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CapReason;

    /// The spellings the file and the status line share, and the strength
    /// order a cap relies on.
    #[test]
    fn the_spellings_and_the_strength_order_are_the_ones_the_file_uses() {
        assert_eq!(Mode::Off.as_str(), "off");
        assert_eq!(Mode::Observe.as_str(), "observe");
        assert_eq!(Mode::Enforce.as_str(), "enforce");
        assert!(Mode::Off < Mode::Observe && Mode::Observe < Mode::Enforce);
        assert_eq!(Mode::default(), Mode::Observe);
        assert_eq!(Formats::All.as_str(), "all");
        assert_eq!(Formats::default(), Formats::All);
        assert_eq!(DocChannel::Rewrite.as_str(), "rewrite");
        assert_eq!(DocChannel::Redirect.as_str(), "redirect");
        assert_eq!(DocChannel::default(), DocChannel::Rewrite);
        for reason in [
            CapReason::UnconfirmedReasonChannel,
            CapReason::NoAmendmentChannel,
        ] {
            assert!(!reason.as_str().is_empty());
        }
    }
}
