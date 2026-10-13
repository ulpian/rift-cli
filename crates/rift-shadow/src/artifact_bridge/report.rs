//! What one bridge run reports: the refused shards, the counts, and the
//! bounded parse notes that name what was dropped.
//!
//! Kept apart because the note cap and the token bound are the report's own
//! invariants, shared by every step of the walk that writes a note.

/// Why a shard was refused for projection (report + audit; never a panic).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedShard {
    /// The shard's checkpoint id.
    pub checkpoint_id: String,
    /// The refusal reason (category only, never payload bytes).
    pub reason: String,
}

/// What one bridge run did — the crash-resume + idempotency evidence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BridgeReport {
    /// The branch walked.
    pub branch: String,
    /// The watermark token recorded (the highest shard id ingested), if any.
    pub tip: Option<String>,
    /// Shards enumerated on the branch.
    pub shards_seen: usize,
    /// Shards projected by this walk (valid redaction report).
    pub shards_projected: usize,
    /// Shards this walk did not open because a completed earlier walk
    /// projected them already (`shards_seen` still counts them).
    pub shards_skipped: usize,
    /// Shards refused, with reasons.
    pub refused: Vec<RefusedShard>,
    /// Canonical events re-derived from artifacts.
    pub events_derived: usize,
    /// `captured_event` rows actually inserted (0 on a re-run or over a
    /// hook-fed projection — the dual-source set-union no-op).
    pub events_inserted: usize,
    /// `tasks/<tool-use-id>/` subtrees the projected shards list whose
    /// metadata parsed — subagent transcripts sealed inside their parent
    /// checkpoint. Counted, never stored: the shard is their record, and the
    /// projection holds no row for them.
    pub task_subtrees: usize,
    /// Spilled tool outputs the projected shards' manifests list whose sealed
    /// body read back with its recorded digest — beside the session
    /// transcript or a subagent's. Counted, never stored, like the subtrees.
    pub spills_sealed: usize,
    /// Spills the manifests record as lost (`spill_missing` and the rest of
    /// the closed vocabulary): named by the shard, sealed nowhere.
    pub spills_lost: usize,
    /// Lenient-parse issues encountered (enumerated, not fatal).
    pub parse_issues: usize,
    /// What those issues **were**, for the ones a human can act on — bounded by
    /// [`MAX_PARSE_NOTES`].
    ///
    /// A count alone is what let a whole archived dialect disappear: every
    /// dot-spelled record failed [`EventKind::from_token`](rift_core::EventKind::from_token), was skipped, and
    /// added 1 to a number that says nothing about which records or why. A
    /// dropped record is a note now, and a note names its cause.
    pub parse_notes: Vec<String>,
}

/// How many parse notes a bridge run keeps.
///
/// Bounded because the input is untrusted: a corrupt or hostile `full.jsonl`
/// can carry an unbounded number of unknown kinds, and a report that grows with
/// it is a memory footgun and an unreadable one. The count is exact regardless;
/// only the naming is capped, and the cap says so in its own line.
pub const MAX_PARSE_NOTES: usize = 8;

impl BridgeReport {
    /// Render the report as the stable, human-readable summary (snapshot-locked
    /// in tests; brand-free).
    #[must_use]
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(s, "bridge: {}", self.branch);
        // The skipped count is printed only when a walk skipped something, so
        // a full walk renders exactly as it always has.
        let skipped = match self.shards_skipped {
            0 => String::new(),
            n => format!(", {n} already projected"),
        };
        let _ = writeln!(
            s,
            "  shards: {} seen, {} projected{skipped}, {} refused",
            self.shards_seen,
            self.shards_projected,
            self.refused.len()
        );
        for r in &self.refused {
            let _ = writeln!(s, "  refused {}: {}", r.checkpoint_id, r.reason);
        }
        let _ = writeln!(
            s,
            "  events: {} derived, {} inserted",
            self.events_derived, self.events_inserted
        );
        // Printed only when a walk met a subtree, so a taskless walk renders
        // exactly as it always has.
        if self.task_subtrees > 0 {
            let _ = writeln!(s, "  task subtrees: {}", self.task_subtrees);
        }
        // The same rule for spills: a spill-less walk renders as it always has.
        if self.spills_sealed + self.spills_lost > 0 {
            let _ = writeln!(
                s,
                "  spills: {} sealed, {} recorded lost",
                self.spills_sealed, self.spills_lost
            );
        }
        let _ = writeln!(s, "  parse issues: {}", self.parse_issues);
        for n in &self.parse_notes {
            let _ = writeln!(s, "  note: {n}");
        }
        if let Some(extra) = self
            .parse_issues
            .checked_sub(self.parse_notes.len())
            .filter(|n| *n > 0 && self.parse_notes.len() >= MAX_PARSE_NOTES)
        {
            let _ = writeln!(s, "  note: and {extra} more not named");
        }
        match &self.tip {
            Some(t) => {
                let _ = writeln!(s, "  watermark: {t}");
            }
            None => {
                let _ = writeln!(s, "  watermark: (unchanged)");
            }
        }
        s
    }
}

/// Record a parse note, up to [`MAX_PARSE_NOTES`]. `parse_issues` is counted by
/// the caller either way, so capping the naming never hides the fact that
/// something was dropped — only which one it was, past the cap.
pub(super) fn note(report: &mut BridgeReport, text: String) {
    if report.parse_notes.len() < MAX_PARSE_NOTES {
        report.parse_notes.push(text);
    }
}

/// The longest a shard-supplied token may be when it is quoted back in a note.
///
/// The note is built from bytes an untrusted `full.jsonl` supplied, so it is
/// bounded before it reaches a report a hook prints. Identifiers only — a kind
/// token and a session id — never payload text.
const MAX_NOTE_TOKEN: usize = 48;

/// Bound a shard-supplied identifier for quoting in a note (char-boundary safe).
pub(super) fn truncate_token(token: &str) -> String {
    match token.char_indices().nth(MAX_NOTE_TOKEN) {
        None => token.to_owned(),
        Some((cut, _)) => format!("{}…", token.get(..cut).unwrap_or_default()),
    }
}
