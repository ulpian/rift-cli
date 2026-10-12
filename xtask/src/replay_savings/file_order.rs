//! The two rows that depend on what a window did *before* what: reading
//! after a compaction, and the reads that follow a pull's answer.
//!
//! The replay walks the windows of a tree in transcript time, one order over
//! all of them, because a revision one window advances is the one the next
//! window's read is keyed by. Within one window that order is not the order
//! the records were written in: the client stamps records as it receives
//! them, so a window's stamps run backwards in hundreds of places. A
//! compaction is a place in the window's own history, and so is "the next
//! read", so these two are folded over each window's records in file order,
//! from what the replay noted of each call while it walked.

use std::collections::{BTreeMap, BTreeSet};

use super::post_compaction::CompactionWatch;
use super::pull_answers::PendingAnswer;
use super::report::Report;
use super::transcript::{Event, Window};

/// One event of a window, as the file spells it.
#[derive(Clone, Debug)]
enum Order {
    /// An eviction record.
    Evict,
    /// A person's prompt.
    Turn,
    /// A tool call, by id.
    Call(String),
}

/// What the replay noted of one repository-content read call.
#[derive(Clone, Debug, Default)]
pub(super) struct ReadNote {
    /// The paths it named inside the work tree.
    pub(super) paths: Vec<String>,
    /// Of those, the ones the replay resolved to a line count.
    pub(super) resolved: BTreeSet<String>,
    /// Of those, the ones it read from line 1 through the last line.
    pub(super) whole: BTreeSet<String>,
    /// Whether the shell made it, rather than the read tool.
    pub(super) shell: bool,
    /// The tokens its result returned.
    pub(super) tokens: u64,
}

/// A tree's windows in file order, and what the replay noted of their calls.
#[derive(Debug, Default)]
pub(super) struct FileOrder {
    /// Each window's events, in the order its file holds them.
    windows: Vec<Vec<Order>>,
    /// The read calls, by window and call id.
    reads: BTreeMap<(usize, String), ReadNote>,
    /// The answers of pull calls outside a scratch repository, the same way.
    answers: BTreeMap<(usize, String), PendingAnswer>,
}

impl FileOrder {
    /// The file order of a tree's windows.
    pub(super) fn of(windows: &[Window]) -> Self {
        let windows = windows
            .iter()
            .map(|window| {
                window
                    .events
                    .iter()
                    .map(|(_, _, event)| match event {
                        Event::Evict => Order::Evict,
                        Event::Turn => Order::Turn,
                        Event::Call(call) => Order::Call(call.id.clone()),
                    })
                    .collect()
            })
            .collect();
        Self {
            windows,
            ..Self::default()
        }
    }

    /// A read call of `window`.
    pub(super) fn note_read(&mut self, window: usize, call: &str, note: ReadNote) {
        self.reads.insert((window, call.to_owned()), note);
    }

    /// The read call `call` of `window` named `path`, and the replay resolved
    /// it to a line count.
    pub(super) fn note_resolved(&mut self, window: usize, call: &str, path: &str) {
        if let Some(note) = self.reads.get_mut(&(window, call.to_owned())) {
            note.resolved.insert(path.to_owned());
        }
    }

    /// The read call `call` of `window` read `path` from line 1 through its
    /// last line.
    pub(super) fn note_whole(&mut self, window: usize, call: &str, path: &str) {
        if let Some(note) = self.reads.get_mut(&(window, call.to_owned())) {
            note.whole.insert(path.to_owned());
        }
    }

    /// The answer of a pull call of `window`.
    pub(super) fn note_answer(&mut self, window: usize, call: &str, answer: PendingAnswer) {
        self.answers.insert((window, call.to_owned()), answer);
    }

    /// Fold every window, each in its own file order, into the report.
    pub(super) fn fold(&self, report: &mut Report) {
        for (window, events) in self.windows.iter().enumerate() {
            let mut watch = CompactionWatch::default();
            let mut held: Vec<PendingAnswer> = Vec::new();
            let mut turn = 0_u32;
            for event in events {
                match event {
                    Order::Turn => turn = turn.saturating_add(1),
                    Order::Evict => {
                        watch.note_eviction(turn, &mut report.post_compaction);
                        held.clear();
                    }
                    Order::Call(id) => {
                        watch.note_call();
                        let key = (window, id.clone());
                        if let Some(answer) = self.answers.get(&key) {
                            held.push(answer.clone());
                        }
                        if let Some(read) = self.reads.get(&key) {
                            watch.note_read(read, turn, &mut report.post_compaction);
                            report.pulls.note_follow_up(&held, read);
                        }
                    }
                }
            }
        }
    }
}
