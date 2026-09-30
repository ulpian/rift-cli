//! How often a context window is emptied, and how much it reads straight after.
//!
//! A compaction is where a window loses what it held and starts reading again,
//! so any lever that answers that reading is sized by two numbers: how many
//! evictions a session sees, and what the reading after each one costs. Both
//! are folds over the same journal lines the rest of the window-journal block
//! walks, so they are computed in that same pass; they live in their own
//! module because they are a question of their own, with their own
//! attribution rule, and because the block's file is already long.

use std::collections::BTreeMap;
use std::path::Path;

use super::super::render::pad;
use super::POST_COMPACTION_TURNS;

/// The reading one eviction was followed by: the reads inside
/// [`POST_COMPACTION_TURNS`] of it that no later eviction claimed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Storm {
    /// The eviction's reason, as the journal spells it.
    pub(super) reason: &'static str,
    /// Reads credited to it, measured or not.
    pub(super) reads: u64,
    /// Their measured tokens.
    pub(super) tokens: u64,
    /// Of those reads, the ones whose bytes could not be re-counted.
    pub(super) unmeasured: u64,
}

/// The distribution of the storms over every eviction this member recorded.
///
/// One storm per eviction, empty ones included: an eviction nobody read
/// after is an answer too, and leaving it out would inflate the median. The
/// median is the lower of the two middle values on an even count, so it is
/// always a storm that happened rather than an average of two.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub(in crate::memory) struct Storms {
    /// Storms, one per eviction.
    pub(in crate::memory) count: u64,
    /// Storms with no read in them.
    pub(in crate::memory) empty: u64,
    /// Reads across every storm.
    pub(in crate::memory) reads: u64,
    /// Measured tokens across every storm — the post-compaction total.
    pub(in crate::memory) tokens: u64,
    /// The median storm's measured tokens.
    pub(in crate::memory) median_tokens: u64,
    /// The largest storm's measured tokens.
    pub(in crate::memory) max_tokens: u64,
    /// The most reads one storm took.
    pub(in crate::memory) max_reads: u64,
    /// Storms containing at least one read that could not be re-counted, so
    /// their token figure is a floor.
    pub(in crate::memory) with_unmeasured: u64,
}

/// How often this member's windows were emptied, per session, and the
/// reading each emptying was followed by.
///
/// **Where the session comes from.** A journal file is named by the hash of
/// its session id and agent id together, so its name cannot say which
/// session it belongs to. The window's injection marker,
/// `.rift/state/injection/<same key>.json`, is written under the same key and
/// names the session in its `session` field: that is the id counted here, and
/// a subagent's window lands under its parent's session because its hooks
/// carry the parent's id. A window without a marker is counted as one, and
/// its evictions are left out of the per-session rate rather than guessed
/// onto a session.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub(in crate::memory) struct Compaction {
    /// The turns after an eviction a read still counts as recovery.
    pub(in crate::memory) post_compaction_turns: u32,
    /// Evictions recorded, every reason — the block's own eviction count.
    pub(in crate::memory) evictions: u64,
    /// The same evictions by reason: a truncation and a compaction are both
    /// emptyings but not the same event, and one compaction can announce
    /// itself more than once.
    pub(in crate::memory) by_reason: BTreeMap<String, u64>,
    /// Distinct sessions the windows' markers name.
    pub(in crate::memory) sessions: u64,
    /// Of those, the sessions with at least one eviction.
    pub(in crate::memory) sessions_with_eviction: u64,
    /// Windows no marker names a session for.
    pub(in crate::memory) windows_without_session: u64,
    /// Their evictions, outside the rate below.
    pub(in crate::memory) evictions_without_session: u64,
    /// Evictions in windows with a session, divided by those sessions;
    /// absent when no window names one.
    pub(in crate::memory) compactions_per_session: Option<f64>,
    /// The reading after each eviction.
    pub(in crate::memory) storms: Storms,
}

/// What the fold gathers from each window before the distribution is taken.
#[derive(Default)]
pub(super) struct CompactionFold {
    /// Evictions per session named.
    sessions: BTreeMap<String, u64>,
    windows_without_session: u64,
    evictions_without_session: u64,
    storms: Vec<Storm>,
}

impl CompactionFold {
    /// Add one window's storms under the session its marker names.
    pub(super) fn add_window(&mut self, session: Option<String>, storms: Vec<Storm>) {
        let evictions = u64::try_from(storms.len()).unwrap_or(u64::MAX);
        self.storms.extend(storms);
        let Some(id) = session else {
            self.windows_without_session = self.windows_without_session.saturating_add(1);
            self.evictions_without_session =
                self.evictions_without_session.saturating_add(evictions);
            return;
        };
        let slot = self.sessions.entry(id).or_insert(0);
        *slot = slot.saturating_add(evictions);
    }

    /// The account, with the rate and the distribution taken.
    pub(super) fn finish(self) -> Compaction {
        let sessions = u64::try_from(self.sessions.len()).unwrap_or(u64::MAX);
        let attributed: u64 = self.sessions.values().sum();
        let compactions_per_session = match sessions {
            0 => None,
            n => Some(attributed as f64 / n as f64),
        };
        let mut by_reason: BTreeMap<String, u64> = BTreeMap::new();
        for storm in &self.storms {
            let slot = by_reason.entry(storm.reason.to_owned()).or_insert(0);
            *slot = slot.saturating_add(1);
        }
        Compaction {
            post_compaction_turns: POST_COMPACTION_TURNS,
            evictions: u64::try_from(self.storms.len()).unwrap_or(u64::MAX),
            by_reason,
            sessions,
            sessions_with_eviction: self.sessions.values().filter(|n| **n > 0).count() as u64,
            windows_without_session: self.windows_without_session,
            evictions_without_session: self.evictions_without_session,
            compactions_per_session,
            storms: distribution(&self.storms),
        }
    }
}

/// The session a window's injection marker names, or `None` without one.
///
/// The marker is agent-reachable run state, so only its `session` string is
/// taken, and it is counted, never printed.
pub(super) fn session_of(work_tree: &Path, key: &str) -> Option<String> {
    let marker = work_tree
        .join(".rift")
        .join("state")
        .join("injection")
        .join(format!("{key}.json"));
    let text = std::fs::read_to_string(marker).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let session = value.get("session")?.as_str()?;
    if session.is_empty() {
        return None;
    }
    Some(session.to_owned())
}

/// The compaction lines of the window-journal block (golden-locked), printed
/// straight after its eviction lines; nothing when the block had no journals
/// to read.
pub(in crate::memory) fn render_compaction(c: &Compaction, note: Option<&str>) -> String {
    if note.is_some() {
        return String::new();
    }
    let mut out = String::new();
    let row = |label: &str, value: String| format!("  {}{value}\n", pad(label, 23));
    out.push_str(&row(
        "sessions:",
        format!(
            "{} ({} with an eviction; {} window(s) name no session)",
            c.sessions, c.sessions_with_eviction, c.windows_without_session
        ),
    ));
    let rate = match c.compactions_per_session {
        Some(rate) => format!(
            "{rate:.2} ({} left out, in windows naming no session)",
            c.evictions_without_session
        ),
        None => "n/a (no window names its session)".to_owned(),
    };
    out.push_str(&row("evictions per session:", rate));
    let reasons: Vec<String> = c
        .by_reason
        .iter()
        .map(|(r, n)| format!("{r} {n}"))
        .collect();
    let reasons = if reasons.is_empty() {
        "(none)".to_owned()
    } else {
        reasons.join(", ")
    };
    out.push_str(&row("evictions by reason:", reasons));
    let s = &c.storms;
    out.push_str(&format!(
        "  after each eviction (reads within {} turns, credited to the latest):\n",
        c.post_compaction_turns
    ));
    out.push_str(&row(
        "  storms:",
        format!(
            "{} ({} empty, {} with an unmeasured read)",
            s.count, s.empty, s.with_unmeasured
        ),
    ));
    out.push_str(&row(
        "  tokens:",
        format!(
            "{} total, median {}, max {}",
            s.tokens, s.median_tokens, s.max_tokens
        ),
    ));
    out.push_str(&row(
        "  reads:",
        format!("{} total, max {}", s.reads, s.max_reads),
    ));
    out
}

/// Count, median, maximum and the unmeasured share over the storms.
fn distribution(storms: &[Storm]) -> Storms {
    let mut tokens: Vec<u64> = storms.iter().map(|s| s.tokens).collect();
    tokens.sort_unstable();
    let median_tokens = match tokens.len() {
        0 => 0,
        n => tokens.get((n - 1) / 2).copied().unwrap_or(0),
    };
    Storms {
        count: u64::try_from(storms.len()).unwrap_or(u64::MAX),
        empty: storms.iter().filter(|s| s.reads == 0).count() as u64,
        reads: storms.iter().map(|s| s.reads).sum(),
        tokens: tokens.iter().sum(),
        median_tokens,
        max_tokens: tokens.last().copied().unwrap_or(0),
        max_reads: storms.iter().map(|s| s.reads).max().unwrap_or(0),
        with_unmeasured: storms.iter().filter(|s| s.unmeasured > 0).count() as u64,
    }
}

#[cfg(test)]
mod tests {
    use rift_agent::window_journal::{EvictReason, Interval, Line as JournalLine};

    use super::super::tests::{read_line, revision_id, seeded_tree, window_file};
    use super::super::{fold_window, window_journals, SliceMeter, WindowJournals};
    use super::*;

    fn evict(turn: u32, reason: EvictReason) -> JournalLine {
        JournalLine::Evict {
            turn,
            reason,
            at_ms: 7,
        }
    }

    /// A read of a file that is not in the tree: counted, never measured.
    fn unmeasured_read(turn: u32, rev: &str) -> JournalLine {
        JournalLine::Read {
            turn,
            path: "gone.txt".to_owned(),
            hash: revision_id(rev),
            range: Interval::new(1, 10).unwrap(),
            total_lines: 10,
            at_ms: 1,
        }
    }

    /// The tokens the seeded file's lines `lo..=hi` measure at.
    fn tokens_of(lo: u32, hi: u32) -> u64 {
        let text = (lo..=hi).fold(String::new(), |mut acc, n| {
            use core::fmt::Write as _;
            let _ = writeln!(acc, "line {n}");
            acc
        });
        u64::from(rift_tokens::count(&text))
    }

    /// Write the injection marker that names a window's session.
    fn marker(root: &Path, session: &str, agent: Option<&str>) -> String {
        let key = crate::window_journal::key(session, agent);
        let dir = root.join(".rift").join("state").join("injection");
        std::fs::create_dir_all(&dir).unwrap();
        let body =
            serde_json::json!({ "schema_version": 1, "session": session, "agent_id": agent });
        std::fs::write(dir.join(format!("{key}.json")), body.to_string()).unwrap();
        key
    }

    /// The subagent window: two evictions two turns apart, so the turns after
    /// the first overlap the turns after the second.
    fn overlapping(rev: &str) -> Vec<JournalLine> {
        vec![
            evict(2, EvictReason::PreCompact),
            read_line(3, rev, 61, 70),
            evict(4, EvictReason::Compact),
            read_line(5, rev, 71, 100),
            read_line(6, rev, 101, 200),
        ]
    }

    /// Two sessions, one with a subagent window, one window no marker names,
    /// and a session with no eviction at all.
    fn fixture() -> (tempfile::TempDir, String) {
        let (tmp, rev) = seeded_tree(200);
        let main = marker(tmp.path(), "s1", None);
        window_file(
            tmp.path(),
            &main,
            &[
                read_line(1, &rev, 1, 50),
                evict(2, EvictReason::Compact),
                read_line(3, &rev, 1, 20),
                read_line(9, &rev, 1, 50),
                evict(10, EvictReason::PostCompact),
                read_line(11, &rev, 21, 60),
                unmeasured_read(11, &rev),
            ],
        );
        let sub = marker(tmp.path(), "s1", Some("agent-1"));
        window_file(tmp.path(), &sub, &overlapping(&rev));
        let quiet = marker(tmp.path(), "s2", None);
        window_file(tmp.path(), &quiet, &[read_line(1, &rev, 1, 5)]);
        window_file(tmp.path(), "orphan", &[evict(1, EvictReason::Resume)]);
        (tmp, rev)
    }

    /// ★ Where two evictions' turns overlap, a read is credited to the LATEST
    /// eviction only — the rule the post-compaction total already applies —
    /// so no read is in two storms and the storms partition the total.
    #[test]
    fn an_overlapping_read_is_credited_to_the_latest_eviction_only() {
        let (tmp, rev) = seeded_tree(200);
        let mut meter = SliceMeter::new(tmp.path());
        let mut out = WindowJournals::unavailable(String::new(), String::new());
        let storms = fold_window(&overlapping(&rev), &mut meter, &mut out);
        let shape: Vec<(&str, u64, u64)> = storms
            .iter()
            .map(|s| (s.reason, s.reads, s.tokens))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("pre_compact", 1, tokens_of(61, 70)),
                ("compact", 2, tokens_of(71, 100) + tokens_of(101, 200)),
            ],
            "turn 5 is inside both evictions' turns and lands in the second"
        );
        let sum: u64 = storms.iter().map(|s| s.tokens).sum();
        assert_eq!(sum, out.post_compaction_tokens);
    }

    /// ★ The rate counts sessions, not windows: a subagent's window lands under
    /// its parent's session, a session with no eviction still divides, and a
    /// window no marker names is reported beside the rate, not inside it.
    #[test]
    fn evictions_per_session_divide_by_the_sessions_the_markers_name() {
        let (tmp, _) = fixture();
        let w = window_journals(tmp.path(), 98_304, 24);
        let c = &w.compaction;
        assert_eq!((w.windows, w.evictions, c.evictions), (4, 5, 5));
        assert_eq!((c.sessions, c.sessions_with_eviction), (2, 1));
        assert_eq!(
            (c.windows_without_session, c.evictions_without_session),
            (1, 1)
        );
        assert_eq!(
            c.compactions_per_session,
            Some(2.0),
            "four evictions, two sessions"
        );
        let reasons: Vec<(&str, u64)> = c.by_reason.iter().map(|(r, n)| (r.as_str(), *n)).collect();
        assert_eq!(
            reasons,
            vec![
                ("compact", 2),
                ("post_compact", 1),
                ("pre_compact", 1),
                ("resume", 1)
            ]
        );
    }

    /// ★ The storms are a partition of the post-compaction total, and their
    /// distribution is taken over every eviction, the empty one included.
    #[test]
    fn the_storm_distribution_sums_to_the_post_compaction_total() {
        let (tmp, _) = fixture();
        let w = window_journals(tmp.path(), 98_304, 24);
        let s = &w.compaction.storms;
        let mut each = [
            tokens_of(1, 20),
            tokens_of(21, 60),
            tokens_of(61, 70),
            tokens_of(71, 100) + tokens_of(101, 200),
            0,
        ];
        each.sort_unstable();
        assert_eq!(
            s.tokens, w.post_compaction_tokens,
            "the storms sum to the total"
        );
        assert_eq!(s.tokens, each.iter().sum::<u64>());
        assert_eq!((s.count, s.empty, s.with_unmeasured), (5, 1, 1));
        assert_eq!(
            (s.reads, s.max_reads),
            (6, 2),
            "the unmeasured read is a read"
        );
        assert_eq!(s.median_tokens, each[2]);
        assert_eq!(s.max_tokens, each[4]);
    }

    /// With no window naming a session the rate is absent, never a division
    /// by zero and never a zero.
    #[test]
    fn a_member_with_no_session_marker_publishes_no_rate() {
        let (tmp, _) = seeded_tree(10);
        window_file(tmp.path(), "orphan", &[evict(1, EvictReason::Clear)]);
        let c = window_journals(tmp.path(), 98_304, 24).compaction;
        assert_eq!((c.sessions, c.compactions_per_session), (0, None));
        assert!(render_compaction(&c, None).contains("n/a (no window names its session)"));
    }

    /// ★ The compaction lines, locked: they are appended to the block and
    /// never change a line above them.
    #[test]
    fn the_compaction_lines_are_locked() {
        let c = Compaction {
            post_compaction_turns: 3,
            evictions: 5,
            by_reason: BTreeMap::from([("compact".to_owned(), 3), ("truncate".to_owned(), 2)]),
            sessions: 2,
            sessions_with_eviction: 1,
            windows_without_session: 1,
            evictions_without_session: 1,
            compactions_per_session: Some(2.0),
            storms: Storms {
                count: 5,
                empty: 1,
                reads: 6,
                tokens: 250,
                median_tokens: 40,
                max_tokens: 120,
                max_reads: 2,
                with_unmeasured: 1,
            },
        };
        let golden = "  sessions:              2 (1 with an eviction; 1 window(s) name no session)
  evictions per session: 2.00 (1 left out, in windows naming no session)
  evictions by reason:   compact 3, truncate 2
  after each eviction (reads within 3 turns, credited to the latest):
    storms:              5 (1 empty, 1 with an unmeasured read)
    tokens:              250 total, median 40, max 120
    reads:               6 total, max 2
";
        assert_eq!(render_compaction(&c, None), golden);
        assert_eq!(render_compaction(&c, Some("unreadable")), "");
    }
}
