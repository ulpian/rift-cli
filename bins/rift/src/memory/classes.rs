//! The repeat classes: how the family's captured reads partition into first
//! reads and the two kinds of repeat.
//!
//! This is the baseline the interception work is measured against, and it is a
//! FAMILY fact — the rows come from the capture projection every member shares,
//! never from one checkout's slice of it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rift_core::Result;
use rift_store::{CanonicalStore, SqliteStore};

/// One captured read, reduced to the five fields the classification needs.
///
/// The rows come from the FAMILY's capture projection — one store per
/// repository, shared by every member — so the classes are a family fact for
/// the same reason `reread-report`'s other numbers are: a total that dropped
/// every worktree but this one is a number nobody could audit. Nothing here
/// filters by checkout path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReadFact {
    /// The projected session the read belongs to.
    pub(super) session_id: String,
    /// Repo-relative path read.
    pub(super) path: String,
    /// Tool-call start (epoch ms) — the ordering axis.
    pub(super) started_at: i64,
    /// The tool call, used only to break timestamp ties deterministically.
    pub(super) tool_call_id: String,
    /// The revision identity a repeat is judged against: one immutable
    /// `(path, blob_oid)` fact, so two reads sharing it read the same bytes.
    pub(super) file_revision_id: String,
    /// Measured token count over the read-time bytes, when one was captured.
    /// Reads recorded before the token-count relation existed have none; they
    /// are counted as reads and contribute nothing to the token column, and
    /// the report says how many there were.
    pub(super) input_tokens: Option<u64>,
    /// The family whose session made the call, as the projection recorded it.
    pub(super) agent: String,
    /// The tool that made it, as the family spells it.
    pub(super) tool_name: String,
}

/// Whether this read arrived through a shell command rather than a read tool.
///
/// Decided by the PINNED tool table, not by a list of names retyped here: a
/// family's shell tool is whichever tool its own output pin gives the shell
/// shape. A family or a tool the table does not know is not a shell read —
/// the conservative direction, and the report publishes the unknown count
/// beside the share so nobody has to guess how much it hid.
fn is_shell_read(fact: &ReadFact) -> bool {
    is_shell_tool(&fact.agent, &fact.tool_name)
}

/// Whether `(family, tool)` is that family's shell, from the family's own
/// pinned table and never from a list of tool names retyped here.
///
/// Spelled once and used twice: by the reread report's classes, and by the
/// savings denominator, which splits the repository content a window was
/// handed between the read tool and the command line. A tool the pins do not
/// name is not a shell — the conservative direction, because the alternative
/// is a shell figure that grows every time a family adds a tool.
pub(super) fn is_shell_tool(agent: &str, tool: &str) -> bool {
    rift_agent::tool_fields::tool_field_row(&rift_core::AgentId::new(agent.to_owned()), tool)
        .is_some_and(|row| row.shape == rift_agent::tool_fields::ToolShape::Shell)
}

/// Which of the three classes a read falls in. The three are exhaustive and
/// mutually exclusive by construction: a read either has no earlier read of
/// its path in its session (first), or it has one at the same revision
/// (unchanged repeat), or it has one only at a different revision (changed
/// repeat).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReadClass {
    /// No earlier read of this path in this session.
    FirstRead,
    /// An earlier read of this path at the SAME revision exists.
    RereadUnchanged,
    /// Earlier reads of this path exist, all at OTHER revisions.
    RereadChanged,
}

/// Classify every read, returning one class per input in input order.
///
/// "Earlier" is strictly earlier by `started_at`, exactly as the measurement
/// query defines it: two reads recorded in the same millisecond are each
/// other's peers, not each other's priors, so both can be first reads. The
/// alternative — an arbitrary tie-break — would invent a repeat out of clock
/// granularity, and this report exists to be believed.
pub(super) fn classify_reads(facts: &[ReadFact]) -> Vec<ReadClass> {
    // Group by (session, path): a repeat is only ever a repeat within one
    // session's context window, and only of the same path.
    let mut groups: BTreeMap<(&str, &str), Vec<usize>> = BTreeMap::new();
    for (i, f) in facts.iter().enumerate() {
        groups
            .entry((f.session_id.as_str(), f.path.as_str()))
            .or_default()
            .push(i);
    }

    let mut out = vec![ReadClass::FirstRead; facts.len()];
    for idx in groups.values() {
        let mut ordered: Vec<usize> = idx.clone();
        ordered.sort_by(|&a, &b| {
            let (fa, fb) = (facts.get(a), facts.get(b));
            match (fa, fb) {
                (Some(x), Some(y)) => (x.started_at, x.tool_call_id.as_str())
                    .cmp(&(y.started_at, y.tool_call_id.as_str())),
                _ => std::cmp::Ordering::Equal,
            }
        });
        // Revisions read strictly before the fact under judgement. `pending`
        // holds the ones at the current timestamp, which are peers until the
        // clock moves.
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut pending: Vec<(i64, &str)> = Vec::new();
        for &i in &ordered {
            let Some(f) = facts.get(i) else { continue };
            let mut still: Vec<(i64, &str)> = Vec::new();
            for (at, rev) in pending.drain(..) {
                if at < f.started_at {
                    seen.insert(rev);
                } else {
                    still.push((at, rev));
                }
            }
            pending = still;
            let same_rev = seen.contains(f.file_revision_id.as_str());
            let other_rev = seen.len() > usize::from(same_rev);
            if let Some(slot) = out.get_mut(i) {
                *slot = if same_rev {
                    ReadClass::RereadUnchanged
                } else if other_rev {
                    ReadClass::RereadChanged
                } else {
                    ReadClass::FirstRead
                };
            }
            pending.push((f.started_at, f.file_revision_id.as_str()));
        }
    }
    out
}

/// Reads and tokens for one class.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub(super) struct ClassTotals {
    /// How many read calls fell in the class.
    pub(super) reads: u64,
    /// Measured tokens over those reads (uncounted reads contribute 0).
    pub(super) tokens: u64,
}

impl ClassTotals {
    fn add(&mut self, tokens: Option<u64>) {
        self.reads = self.reads.saturating_add(1);
        self.tokens = self.tokens.saturating_add(tokens.unwrap_or(0));
    }
}

/// One session's three classes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct SessionClasses {
    /// The projected session id.
    pub(super) session_id: String,
    /// First reads in this session.
    pub(super) first_read: ClassTotals,
    /// Unchanged repeats in this session.
    pub(super) reread_unchanged: ClassTotals,
    /// Changed repeats in this session.
    pub(super) reread_changed: ClassTotals,
}

/// The baseline the interception work is measured against: how the family's
/// captured reads partition into first reads and the two kinds of repeat.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct RepeatClasses {
    /// The capture projection these rows came from (named so the number can be
    /// re-derived by hand).
    pub(super) projection: String,
    /// Sessions with at least one captured read.
    pub(super) sessions: u64,
    /// First read of a path in a session.
    pub(super) first_read: ClassTotals,
    /// Re-read of a revision the session had already read.
    pub(super) reread_unchanged: ClassTotals,
    /// Re-read of a path the session had only read at other revisions.
    pub(super) reread_changed: ClassTotals,
    /// Reads with no measured token count (counted as reads, worth 0 tokens).
    pub(super) reads_without_token_counts: u64,
    /// Reads that arrived as a shell command rather than through a read tool.
    pub(super) shell_reads: u64,
    /// Reads whose `(family, tool)` the pinned table does not know, and which
    /// are therefore counted as not-shell.
    pub(super) reads_of_unknown_tools: u64,
    /// Per session, descending by tokens then session id.
    pub(super) per_session: Vec<SessionClasses>,
    /// Why the numbers are absent, when they are.
    pub(super) note: Option<String>,
}

impl RepeatClasses {
    /// Every read, regardless of class — the denominator of the read column.
    pub(super) fn total_reads(&self) -> u64 {
        self.first_read
            .reads
            .saturating_add(self.reread_unchanged.reads)
            .saturating_add(self.reread_changed.reads)
    }

    /// Every measured token, regardless of class — the share denominator.
    pub(super) fn total_tokens(&self) -> u64 {
        self.first_read
            .tokens
            .saturating_add(self.reread_unchanged.tokens)
            .saturating_add(self.reread_changed.tokens)
    }

    /// The addressable pool: both kinds of repeat together.
    pub(super) fn repeats(&self) -> ClassTotals {
        ClassTotals {
            reads: self
                .reread_unchanged
                .reads
                .saturating_add(self.reread_changed.reads),
            tokens: self
                .reread_unchanged
                .tokens
                .saturating_add(self.reread_changed.tokens),
        }
    }

    /// An empty result carrying the reason it is empty.
    fn unavailable(projection: String, note: String) -> Self {
        Self {
            projection,
            sessions: 0,
            first_read: ClassTotals::default(),
            reread_unchanged: ClassTotals::default(),
            reread_changed: ClassTotals::default(),
            reads_without_token_counts: 0,
            shell_reads: 0,
            reads_of_unknown_tools: 0,
            per_session: Vec::new(),
            note: Some(note),
        }
    }
}

/// Aggregate classified reads into the report's rows.
pub(super) fn repeat_classes(projection: String, facts: &[ReadFact]) -> RepeatClasses {
    let classes = classify_reads(facts);
    let mut totals = RepeatClasses {
        projection,
        sessions: 0,
        first_read: ClassTotals::default(),
        reread_unchanged: ClassTotals::default(),
        reread_changed: ClassTotals::default(),
        reads_without_token_counts: 0,
        shell_reads: 0,
        reads_of_unknown_tools: 0,
        per_session: Vec::new(),
        note: None,
    };
    let mut by_session: BTreeMap<&str, SessionClasses> = BTreeMap::new();
    for (i, f) in facts.iter().enumerate() {
        let Some(class) = classes.get(i).copied() else {
            continue;
        };
        if f.input_tokens.is_none() {
            totals.reads_without_token_counts = totals.reads_without_token_counts.saturating_add(1);
        }
        if is_shell_read(f) {
            totals.shell_reads = totals.shell_reads.saturating_add(1);
        } else if rift_agent::tool_fields::tool_field_row(
            &rift_core::AgentId::new(f.agent.clone()),
            &f.tool_name,
        )
        .is_none()
        {
            totals.reads_of_unknown_tools = totals.reads_of_unknown_tools.saturating_add(1);
        }
        let session = by_session
            .entry(f.session_id.as_str())
            .or_insert_with(|| SessionClasses {
                session_id: f.session_id.clone(),
                first_read: ClassTotals::default(),
                reread_unchanged: ClassTotals::default(),
                reread_changed: ClassTotals::default(),
            });
        match class {
            ReadClass::FirstRead => {
                totals.first_read.add(f.input_tokens);
                session.first_read.add(f.input_tokens);
            }
            ReadClass::RereadUnchanged => {
                totals.reread_unchanged.add(f.input_tokens);
                session.reread_unchanged.add(f.input_tokens);
            }
            ReadClass::RereadChanged => {
                totals.reread_changed.add(f.input_tokens);
                session.reread_changed.add(f.input_tokens);
            }
        }
    }
    totals.sessions = u64::try_from(by_session.len()).unwrap_or(u64::MAX);
    let mut per_session: Vec<SessionClasses> = by_session.into_values().collect();
    per_session.sort_by(|a, b| {
        let at = a.first_read.tokens + a.reread_unchanged.tokens + a.reread_changed.tokens;
        let bt = b.first_read.tokens + b.reread_unchanged.tokens + b.reread_changed.tokens;
        bt.cmp(&at).then_with(|| a.session_id.cmp(&b.session_id))
    });
    totals.per_session = per_session;
    totals
}

/// Read every captured read fact of this family, oldest first.
///
/// The joins are the measurement query's, unchanged: a read is a `tool_call`
/// with a resolved `tool_reads_file` link, attributed to its session through
/// its turn, with the token count LEFT joined because it is recorded late and
/// sparsely.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub(super) async fn read_facts(store: &SqliteStore) -> Result<Vec<ReadFact>> {
    let rows = store
        .query(
            "SELECT t.session_id, fr.path, tc.started_at, tc.tool_call_id, \
             fr.file_revision_id, frt.input_tokens, s.agent, tc.tool_name \
             FROM tool_call tc \
             JOIN turn t ON t.turn_id = tc.turn_id \
             JOIN tool_reads_file trf ON trf.tool_call_id = tc.tool_call_id \
             JOIN file_revision fr ON fr.file_revision_id = trf.file_revision_id \
             LEFT JOIN file_read_tokens frt ON frt.tool_call_id = tc.tool_call_id \
             LEFT JOIN session s ON s.session_id = t.session_id \
             ORDER BY t.session_id ASC, fr.path ASC, tc.started_at ASC, tc.tool_call_id ASC",
            &[],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(ReadFact {
            session_id: r.get_text(0)?,
            path: r.get_text(1)?,
            started_at: r.get_i64(2)?,
            tool_call_id: r.get_text(3)?,
            file_revision_id: r.get_text(4)?,
            input_tokens: r.get_opt_i64(5)?.and_then(|t| u64::try_from(t).ok()),
            // LEFT joined on purpose: a session row the projection has not
            // caught up with must not drop the read out of the classification.
            agent: r.get_text(6).unwrap_or_default(),
            tool_name: r.get_text(7)?,
        });
    }
    Ok(out)
}

/// The repeat classes for the family `repo` belongs to.
///
/// A projection this process cannot open is a NOTE, not a failure: the rest of
/// the reread report is computed from a different store and stays useful, and
/// a baseline that refuses to print is a baseline nobody records.
pub(super) async fn family_repeat_classes(repo: &Path) -> RepeatClasses {
    let db = match crate::family::projection_db(repo) {
        Ok(db) => db,
        Err(e) => return RepeatClasses::unavailable("(unresolved)".to_owned(), e.to_string()),
    };
    let shown = db.display().to_string();
    if !db.exists() {
        return RepeatClasses::unavailable(
            shown,
            "no capture projection yet — nothing has been captured for this family".to_owned(),
        );
    }
    // Read-only: a measurement must not migrate the projection the owner's
    // live sessions are appending to, and a report is never worth a write.
    let store = match SqliteStore::open_read_only(&db) {
        Ok(store) => store,
        Err(e) => return RepeatClasses::unavailable(shown, e.to_string()),
    };
    match read_facts(&store).await {
        Ok(facts) => repeat_classes(shown, &facts),
        Err(e) => RepeatClasses::unavailable(shown, e.to_string()),
    }
}

/// One class row: name, reads, tokens, share of measured read tokens.
fn class_row(label: &str, t: ClassTotals, denominator: u64) -> String {
    let share = if denominator == 0 {
        0.0
    } else {
        (t.tokens as f64) * 100.0 / (denominator as f64)
    };
    format!(
        "  {label:<18}{:>8}{:>12}{:>9.1}%\n",
        t.reads, t.tokens, share
    )
}

/// Render the repeat-class block (golden-locked).
pub(super) fn render_repeat_classes(c: &RepeatClasses) -> String {
    let mut out = String::new();
    out.push_str("repeat classes (capture projection):\n");
    out.push_str(&format!("  projection:            {}\n", c.projection));
    if let Some(note) = &c.note {
        out.push_str(&format!("  not measured:          {note}\n"));
        return out;
    }
    out.push_str(&format!("  sessions with reads:   {}\n", c.sessions));
    out.push_str(&format!("  reads classified:      {}\n", c.total_reads()));
    out.push_str(&format!(
        "  reads without counts:  {}\n",
        c.reads_without_token_counts
    ));
    let shell_share = if c.total_reads() == 0 {
        0.0
    } else {
        (c.shell_reads as f64) * 100.0 / (c.total_reads() as f64)
    };
    out.push_str(&format!(
        "  shell reads:           {} ({shell_share:.1}%, {} through a tool the pins do not name)\n",
        c.shell_reads, c.reads_of_unknown_tools
    ));
    let denominator = c.total_tokens();
    out.push_str("  class                reads      tokens    share\n");
    out.push_str(&class_row("first-read", c.first_read, denominator));
    out.push_str(&class_row(
        "reread-unchanged",
        c.reread_unchanged,
        denominator,
    ));
    out.push_str(&class_row("reread-changed", c.reread_changed, denominator));
    out.push_str(&class_row("total repeats", c.repeats(), denominator));
    out.push_str("per session:\n");
    if c.per_session.is_empty() {
        out.push_str("  (none)\n");
    }
    for s in &c.per_session {
        let short: String = s.session_id.chars().take(12).collect();
        out.push_str(&format!(
            "  {short:<12}  first {}  unchanged {}  changed {}\n",
            s.first_read.reads, s.reread_unchanged.reads, s.reread_changed.reads
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a read fact; `rev` is the revision identity a repeat is judged
    /// against, so two facts with the same `rev` are the same bytes.
    fn fact(session: &str, path: &str, at: i64, call: &str, rev: &str) -> ReadFact {
        ReadFact {
            session_id: session.to_owned(),
            path: path.to_owned(),
            started_at: at,
            tool_call_id: call.to_owned(),
            file_revision_id: rev.to_owned(),
            input_tokens: Some(100),
            agent: "claude-code".to_owned(),
            tool_name: "Read".to_owned(),
        }
    }

    /// The claim the whole baseline rests on: every captured read lands in
    /// exactly one of the three classes, so the three counts sum to the read
    /// count and no read can be spent twice or lost. The fixture also pins
    /// each boundary the classification has to get right — a different path,
    /// a different session, a different revision, and the timestamp tie.
    #[test]
    fn repeat_classes_partition_the_reads() {
        let facts = vec![
            // s1/a: first, then the same bytes again, then changed bytes.
            fact("s1", "a.rs", 10, "c1", "rev-a1"),
            fact("s1", "a.rs", 20, "c2", "rev-a1"),
            fact("s1", "a.rs", 30, "c3", "rev-a2"),
            // …and back to the first revision: an earlier read at THAT
            // revision exists, so this is an unchanged repeat, not a changed
            // one — the class is about what the context holds, not about the
            // most recent read.
            fact("s1", "a.rs", 40, "c4", "rev-a1"),
            // A different path in the same session is its own first read.
            fact("s1", "b.rs", 50, "c5", "rev-b1"),
            // The same path in a DIFFERENT session is a first read: a repeat
            // is only a repeat within one context window.
            fact("s2", "a.rs", 60, "c6", "rev-a1"),
            // Two reads in the same millisecond are peers, never priors:
            // both are first reads.
            fact("s2", "c.rs", 70, "c7", "rev-c1"),
            fact("s2", "c.rs", 70, "c8", "rev-c1"),
        ];

        let classes = classify_reads(&facts);
        assert_eq!(
            classes,
            vec![
                ReadClass::FirstRead,
                ReadClass::RereadUnchanged,
                ReadClass::RereadChanged,
                ReadClass::RereadUnchanged,
                ReadClass::FirstRead,
                ReadClass::FirstRead,
                ReadClass::FirstRead,
                ReadClass::FirstRead,
            ]
        );

        let c = repeat_classes("p.db".to_owned(), &facts);
        assert_eq!(c.total_reads(), facts.len() as u64, "every read is classed");
        assert_eq!(c.first_read.reads, 5);
        assert_eq!(c.reread_unchanged.reads, 2);
        assert_eq!(c.reread_changed.reads, 1);
        assert_eq!(c.repeats().reads, 3);
        assert_eq!(c.total_tokens(), 800);
        assert_eq!(c.sessions, 2);
        assert_eq!(c.reads_without_token_counts, 0);
        assert!(c.note.is_none());

        // The partition holds over the per-session rows too — the total is
        // the sum of the parts, in both columns.
        let (mut reads, mut tokens) = (0u64, 0u64);
        for s in &c.per_session {
            reads += s.first_read.reads + s.reread_unchanged.reads + s.reread_changed.reads;
            tokens += s.first_read.tokens + s.reread_unchanged.tokens + s.reread_changed.tokens;
        }
        assert_eq!(reads, c.total_reads());
        assert_eq!(tokens, c.total_tokens());
    }

    /// A read captured before the token-count relation existed is a read that
    /// happened: it is counted, it is worth nothing in the token column, and
    /// the report says how many such reads it is standing on.
    #[test]
    fn uncounted_reads_are_counted_as_reads_and_declared() {
        let mut facts = vec![fact("s1", "a.rs", 10, "c1", "rev-a1")];
        facts.push(ReadFact {
            input_tokens: None,
            ..fact("s1", "a.rs", 20, "c2", "rev-a1")
        });
        let c = repeat_classes("p.db".to_owned(), &facts);
        assert_eq!(c.total_reads(), 2);
        assert_eq!(c.total_tokens(), 100);
        assert_eq!(c.reads_without_token_counts, 1);
        assert_eq!(c.reread_unchanged.reads, 1);
        assert_eq!(c.reread_unchanged.tokens, 0);
    }

    /// The repeat-class block is a locked artifact: the baseline recorded
    /// against it and every later run are compared line for line, so the
    /// shape is pinned here. The lever columns that once read
    /// `not recorded yet` are gone from it: every one of them now has a
    /// number, in the blocks the whole-report golden below locks.
    #[test]
    fn repeat_classes_block_is_locked() {
        let classes = RepeatClasses {
            projection: "/cache/rift/fam/projection.db".to_owned(),
            sessions: 2,
            first_read: ClassTotals {
                reads: 307,
                tokens: 2_011_810,
            },
            reread_unchanged: ClassTotals {
                reads: 98,
                tokens: 500_428,
            },
            reread_changed: ClassTotals {
                reads: 58,
                tokens: 866_705,
            },
            reads_without_token_counts: 4,
            shell_reads: 12,
            reads_of_unknown_tools: 1,
            per_session: vec![
                SessionClasses {
                    session_id: "session-aaaa-1111".to_owned(),
                    first_read: ClassTotals {
                        reads: 200,
                        tokens: 1_500_000,
                    },
                    reread_unchanged: ClassTotals {
                        reads: 60,
                        tokens: 300_000,
                    },
                    reread_changed: ClassTotals {
                        reads: 30,
                        tokens: 400_000,
                    },
                },
                SessionClasses {
                    session_id: "session-bbbb-2222".to_owned(),
                    first_read: ClassTotals {
                        reads: 107,
                        tokens: 511_810,
                    },
                    reread_unchanged: ClassTotals {
                        reads: 38,
                        tokens: 200_428,
                    },
                    reread_changed: ClassTotals {
                        reads: 28,
                        tokens: 466_705,
                    },
                },
            ],
            note: None,
        };
        let golden = "\
repeat classes (capture projection):
  projection:            /cache/rift/fam/projection.db
  sessions with reads:   2
  reads classified:      463
  reads without counts:  4
  shell reads:           12 (2.6%, 1 through a tool the pins do not name)
  class                reads      tokens    share
  first-read             307     2011810     59.5%
  reread-unchanged        98      500428     14.8%
  reread-changed          58      866705     25.7%
  total repeats          156     1367133     40.5%
per session:
  session-aaaa  first 200  unchanged 60  changed 30
  session-bbbb  first 107  unchanged 38  changed 28
";
        assert_eq!(render_repeat_classes(&classes), golden);
    }

    /// A projection that could not be read prints WHY and stops — never a
    /// row of zeroes a reader would mistake for "no repeats here".
    #[test]
    fn an_unreadable_projection_renders_its_reason_not_zeroes() {
        let rendered = render_repeat_classes(&RepeatClasses::unavailable(
            "/cache/rift/fam/projection.db".to_owned(),
            "no capture projection yet".to_owned(),
        ));
        let golden = "\
repeat classes (capture projection):
  projection:            /cache/rift/fam/projection.db
  not measured:          no capture projection yet
";
        assert_eq!(rendered, golden);
    }

    /// ★ A read that arrived as a shell command is counted as one, and the
    /// family's own pinned table is what decides that — never a list of tool
    /// names retyped here. A tool the pins do not name is not a shell read,
    /// and the count of those is published so the share cannot quietly hide
    /// a family Rift has not pinned.
    #[test]
    fn the_shell_read_share_comes_from_the_pinned_tool_table() {
        let shell = |agent: &str, tool: &str| ReadFact {
            session_id: "s1".to_owned(),
            path: "a.rs".to_owned(),
            started_at: 1,
            tool_call_id: format!("{agent}:{tool}"),
            file_revision_id: "rev1".to_owned(),
            input_tokens: Some(10),
            agent: agent.to_owned(),
            tool_name: tool.to_owned(),
        };
        let facts = vec![
            shell("claude-code", "Read"),
            shell("claude-code", "Bash"),
            shell("antigravity", "run_command"),
            shell("codex", "Bash"),
            shell("gemini", "read_file"),
        ];
        let c = repeat_classes("db".to_owned(), &facts);
        assert_eq!(c.shell_reads, 3, "Bash twice and run_command once");
        assert_eq!(
            c.reads_of_unknown_tools, 1,
            "the family with no pinned row is declared, not counted as shell"
        );
        assert!(render_repeat_classes(&c).contains("shell reads:           3 (60.0%"));
    }
}
