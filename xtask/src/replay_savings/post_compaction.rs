//! Reading after a compaction: how often a window, once the client has
//! compacted it, reads again what it had read before.
//!
//! A module of its own because it asks a question no bucket does — whether
//! the context a compaction empties is what the next turns go back for — and
//! every figure it adds is on rows of its own, so every row before it reads
//! the same with or without it.
//!
//! **What a compaction is.** The client announces one with two records, a
//! boundary and a summary the model is handed in the emptied window's place.
//! The table's "windows emptied" counts records, so it counts every
//! compaction twice. Here a run of eviction records with no tool call between
//! them is one compaction, and the turn it is counted at is the turn of the
//! last record of the run.
//!
//! **What a re-read is.** A repository-content read call made after a
//! compaction, in the same window and before the next one, that names a path
//! some earlier read call of the window had named before the compaction
//! began. A call is counted once whatever number of its members repeat, and
//! a path is "named" whether or not the call resolved it to a line count.
//!
//! **How far after.** The distance is the number of user prompts between the
//! compaction and the call, so a call in the compaction's own turn is at
//! distance 0.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::file_order::ReadNote;
use super::render::{commas, row};
use super::report::{Report, Tally};

/// The farthest distance, in turns, that still counts as the storm right
/// after a compaction.
const STORM_TURNS: u32 = 3;

/// The farthest distance of the middle band, in turns.
const MIDDLE_TURNS: u32 = 14;

/// The distance bands, in the order the table prints them: a key and a label.
const BANDS: [(&str, &str); 3] = [
    ("up_to_3", "  3 turns or fewer after"),
    ("from_4_to_14", "  4 to 14 turns after"),
    ("from_15", "  15 turns or more after"),
];

/// The channels a re-read travelled by, in the order the table prints them.
const CHANNELS: [(&str, &str); 2] = [
    ("read_tool", "  by the read tool"),
    ("shell", "  by the shell"),
];

/// What a replay saw of reading after a compaction.
#[derive(Clone, Debug, Default)]
pub struct PostCompaction {
    /// How many compactions the windows went through.
    pub compactions: u64,
    /// Every read call within three turns after one, of whatever path.
    storm: Tally,
    /// Of those, the calls repeating a path held before the compaction.
    storm_held: Tally,
    /// Reads of a path held before the compaction, at any later turn, by
    /// channel.
    by_channel: [Tally; 2],
    /// The same reads by their distance in turns.
    by_distance: [Tally; 3],
    /// Of those reads, the ones whose path was resolved to a line count both
    /// times: before the compaction and again after it.
    resolved_rereads: Tally,
}

/// What one window remembers between its calls so a compaction can be told
/// from the records that announce it, and a re-read from a first read.
#[derive(Debug, Default)]
pub(super) struct CompactionWatch {
    /// Every path any read call of the window has named so far.
    named: BTreeSet<String>,
    /// The paths named before the latest compaction began.
    held_at_compaction: BTreeSet<String>,
    /// The same two, for the paths the replay resolved to a line count.
    named_resolved: BTreeSet<String>,
    held_resolved_at_compaction: BTreeSet<String>,
    /// The turn of the latest compaction, once there is one.
    turn: Option<u32>,
    /// Whether the last event of the window was an eviction record.
    open: bool,
}

impl CompactionWatch {
    /// An eviction record at `turn`.
    pub(super) fn note_eviction(&mut self, turn: u32, pool: &mut PostCompaction) {
        if !self.open {
            pool.compactions = pool.compactions.saturating_add(1);
            self.held_at_compaction.clone_from(&self.named);
            self.held_resolved_at_compaction
                .clone_from(&self.named_resolved);
        }
        self.open = true;
        self.turn = Some(turn);
    }

    /// A tool call of any kind: the run of eviction records is over.
    pub(super) fn note_call(&mut self) {
        self.open = false;
    }

    /// A repository-content read call at `turn`.
    pub(super) fn note_read(&mut self, read: &ReadNote, turn: u32, pool: &mut PostCompaction) {
        if let Some(compacted) = self.turn {
            let distance = turn.saturating_sub(compacted);
            let repeats = read
                .paths
                .iter()
                .any(|path| self.held_at_compaction.contains(path));
            if distance <= STORM_TURNS {
                pool.storm.add(read.tokens);
                if repeats {
                    pool.storm_held.add(read.tokens);
                }
            }
            if repeats {
                pool.note_reread(read.shell, distance, read.tokens);
            }
            if read
                .resolved
                .iter()
                .any(|path| self.held_resolved_at_compaction.contains(path))
            {
                pool.resolved_rereads.add(read.tokens);
            }
        }
        self.named.extend(read.paths.iter().cloned());
        self.named_resolved.extend(read.resolved.iter().cloned());
    }
}

impl PostCompaction {
    /// One re-read: its channel, and the band its distance falls in.
    fn note_reread(&mut self, shell: bool, distance: u32, tokens: u64) {
        if let Some(tally) = self.by_channel.get_mut(usize::from(shell)) {
            tally.add(tokens);
        }
        let band = if distance <= STORM_TURNS {
            0
        } else {
            usize::from(distance > MIDDLE_TURNS) + 1
        };
        if let Some(tally) = self.by_distance.get_mut(band) {
            tally.add(tokens);
        }
    }
}

/// The table's rows for reading after a compaction.
pub(super) fn post_compaction_rows(report: &Report, denominator: u64) -> String {
    let pool = &report.post_compaction;
    let mut out = String::from(
        "\n  after a compaction (a boundary and its summary record are one compaction)\n",
    );
    out.push_str(&format!(
        "  {:<48}{:>9}\n",
        "compactions",
        commas(pool.compactions)
    ));
    out.push_str(&row(
        "reads within three turns after one",
        pool.storm,
        denominator,
    ));
    out.push_str(&row(
        "  of a path held before it",
        pool.storm_held,
        denominator,
    ));
    out.push_str("  reads of a path held before a compaction, at any later turn\n");
    for ((_, label), tally) in CHANNELS.iter().zip(pool.by_channel) {
        out.push_str(&row(label, tally, denominator));
    }
    for ((_, label), tally) in BANDS.iter().zip(pool.by_distance) {
        out.push_str(&row(label, tally, denominator));
    }
    out.push_str(&row(
        "  of them, a path resolved to a line count",
        pool.resolved_rereads,
        denominator,
    ));
    out
}

/// The same figures for the machine-readable document.
pub(super) fn post_compaction_json(report: &Report) -> Value {
    let pool = &report.post_compaction;
    let tally = |t: Tally| json!({ "calls": t.calls, "tokens": t.tokens });
    let mut channels = serde_json::Map::new();
    for ((key, _), t) in CHANNELS.iter().zip(pool.by_channel) {
        channels.insert((*key).to_owned(), tally(t));
    }
    let mut distances = serde_json::Map::new();
    for ((key, _), t) in BANDS.iter().zip(pool.by_distance) {
        distances.insert((*key).to_owned(), tally(t));
    }
    json!({
        "compactions": pool.compactions,
        "reads_within_three_turns": tally(pool.storm),
        "reads_within_three_turns_of_a_held_path": tally(pool.storm_held),
        "held_path_reads_by_channel": channels,
        "held_path_reads_by_turn_distance": distances,
        "held_path_reads_resolved_to_a_line_count": tally(pool.resolved_rereads),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A read naming `names`, of which the replay resolved `resolved`.
    fn read(names: &[&str], resolved: &[&str], shell: bool, tokens: u64) -> ReadNote {
        let owned = |list: &[&str]| list.iter().map(|name| (*name).to_owned()).collect();
        ReadNote {
            paths: owned(names),
            resolved: resolved.iter().map(|name| (*name).to_owned()).collect(),
            shell,
            tokens,
            ..ReadNote::default()
        }
    }

    /// ★ Two eviction records with no call between them are one compaction;
    /// a read of a path named before it is a re-read by its channel and
    /// distance; a path first named after it, and a read before any
    /// compaction, are neither.
    #[test]
    fn a_compaction_is_a_run_of_records_and_a_reread_names_a_held_path() {
        let mut pool = PostCompaction::default();
        let mut watch = CompactionWatch::default();
        watch.note_call();
        watch.note_read(&read(&["a.rs"], &["a.rs"], false, 100), 1, &mut pool);
        assert_eq!(pool.by_channel, [Tally::default(); 2], "no compaction yet");
        watch.note_eviction(2, &mut pool);
        watch.note_eviction(2, &mut pool);
        assert_eq!(pool.compactions, 1, "boundary and summary are one");
        watch.note_call();
        // A first read after it names nothing held.
        watch.note_read(&read(&["b.rs"], &["b.rs"], false, 40), 2, &mut pool);
        // Right after, by the shell, two members of which one is held.
        watch.note_read(
            &read(&["b.rs", "a.rs"], &["b.rs", "a.rs"], true, 30),
            3,
            &mut pool,
        );
        // Much later, by the read tool.
        watch.note_read(&read(&["a.rs"], &["a.rs"], false, 7), 20, &mut pool);
        assert_eq!(pool.storm.calls, 2, "the two reads in turns 2 and 3");
        assert_eq!(
            pool.storm_held,
            Tally {
                calls: 1,
                tokens: 30
            }
        );
        assert_eq!(
            pool.by_channel[0],
            Tally {
                calls: 1,
                tokens: 7
            }
        );
        assert_eq!(
            pool.by_channel[1],
            Tally {
                calls: 1,
                tokens: 30
            }
        );
        assert_eq!(pool.by_distance[0].calls, 1);
        assert_eq!(pool.by_distance[1].calls, 0);
        assert_eq!(
            pool.by_distance[2],
            Tally {
                calls: 1,
                tokens: 7
            }
        );
        // A second compaction is counted once the calls have intervened, and
        // holds what the first segment named.
        watch.note_eviction(21, &mut pool);
        watch.note_call();
        watch.note_read(&read(&["b.rs"], &["b.rs"], false, 5), 40, &mut pool);
        assert_eq!(pool.compactions, 2);
        assert_eq!(
            pool.by_distance[1],
            Tally {
                calls: 0,
                tokens: 0
            }
        );
        assert_eq!(
            pool.by_distance[2].calls, 2,
            "b.rs was named in segment one"
        );
        // A path the replay could not give a line count is still a re-read,
        // and is not one of the resolved ones.
        let resolved = pool.resolved_rereads.calls;
        watch.note_read(&read(&["b.rs"], &[], false, 3), 41, &mut pool);
        assert_eq!(pool.by_distance[2].calls, 3);
        assert_eq!(pool.resolved_rereads.calls, resolved, "not resolved");
    }

    #[test]
    fn the_band_edges_are_three_and_fourteen_turns() {
        let mut pool = PostCompaction::default();
        for distance in [0, 3, 4, 14, 15] {
            pool.note_reread(true, distance, 1);
        }
        assert_eq!(pool.by_distance.map(|t| t.calls), [2, 2, 1]);
    }

    #[test]
    fn the_rows_and_the_document_carry_the_same_counts_and_no_path() {
        let mut pool = PostCompaction::default();
        let mut watch = CompactionWatch::default();
        watch.note_read(
            &read(&["secret/path.rs"], &["secret/path.rs"], false, 10),
            1,
            &mut pool,
        );
        watch.note_eviction(1, &mut pool);
        watch.note_call();
        watch.note_read(
            &read(&["secret/path.rs"], &["secret/path.rs"], true, 10),
            2,
            &mut pool,
        );
        let report = Report {
            post_compaction: pool,
            ..Report::default()
        };
        let table = post_compaction_rows(&report, 1_000);
        assert!(table.contains("compactions"), "{table}");
        assert!(!table.contains("secret"), "no path reaches the table");
        let document = post_compaction_json(&report);
        assert_eq!(document["compactions"], 1);
        assert_eq!(document["held_path_reads_by_channel"]["shell"]["calls"], 1);
        assert_eq!(
            document["reads_within_three_turns_of_a_held_path"]["tokens"],
            10
        );
    }
}
