//! What one replay concluded, and the two folds that build it.
//!
//! The report is a value and nothing else: every figure on it is a count or a
//! token total some other module handed it, so a reader can see at a glance
//! what the command can possibly publish.

use std::collections::BTreeMap;

use super::bucket::Bucket;
use super::census::{CensusRow, FirstSightSizes};
use super::worktree::FactsSource;

/// How many calls landed somewhere, and the tokens their results returned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    /// How many calls.
    pub calls: u64,
    /// The tokens their results returned, counted with the real tokenizer.
    pub tokens: u64,
}

impl Tally {
    /// Fold one call in.
    pub(super) fn add(&mut self, tokens: u64) {
        self.calls = self.calls.saturating_add(1);
        self.tokens = self.tokens.saturating_add(tokens);
    }

    /// Fold another tally in.
    fn merge(&mut self, other: Self) {
        self.calls = self.calls.saturating_add(other.calls);
        self.tokens = self.tokens.saturating_add(other.tokens);
    }
}

/// Everything one replay concluded.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// How many session trees were walked.
    pub trees: u64,
    /// How many context windows — a main session, or one subagent file.
    pub windows: u64,
    /// How many tool calls were replayed, of every tool.
    pub tool_calls: u64,
    /// Every tool result in the corpus, of every tool: the informational
    /// ceiling everything else is a part of.
    pub all_results: Tally,
    /// Per-bucket totals, keyed by [`Bucket::key`].
    pub buckets: BTreeMap<&'static str, Tally>,
    /// The read channels: a read tool, or a recognised shell read.
    pub reads: Tally,
    /// The search channel.
    pub searches: Tally,
    /// Reads plus searches: the denominator the share is taken over.
    pub repo_content: Tally,
    /// What the journal found already held, acted on or not.
    pub declinable: Tally,
    /// What this build would actually have declined.
    pub declined: Tally,
    /// What the declines' own reason frames cost, counted with the same
    /// tokenizer over the sealed renderer's output.
    pub frame_tokens: u64,
    /// The reads that can never be declined: the first sight of a revision in
    /// a window.
    pub first_read_floor: Tally,
    /// `(path, revision)` pairs a session tree read in two or more windows.
    pub cross_window_pairs: u64,
    /// What the second and later windows' first reads of those pairs cost.
    pub cross_window: Tally,
    /// The same reads, split by the route each could have been inherited
    /// through, in the order of
    /// [`Inheritance::ALL`](super::inheritance::Inheritance::ALL).
    pub sibling_by_inheritance: [Tally; 4],
    /// Repository-content reads of machine-written files, keyed by
    /// [`MachineFormat::key`](super::census::MachineFormat::key).
    pub machine_formats: BTreeMap<&'static str, CensusRow>,
    /// The first-sight calls, by the size of the file they read.
    pub first_sight_sizes: FirstSightSizes,
    /// Where the path facts came from.
    pub facts: FactsSource,
    /// Shell calls that look like reads and that the grammar does not
    /// recognise: invisible to the gate, and outside every figure above.
    ///
    /// The loose end of a bound. A read word at a command position catches
    /// `… | tail -20` over a build log as readily as `sed -n '1,40p' <file>`,
    /// so this figure is an over-count by construction and is published beside
    /// the tight end rather than on its own.
    pub invisible_shell: Tally,
    /// The tight end of the same bound: of those, the calls that also name a
    /// file this checkout actually has. A command that names a work-tree file
    /// and prints part of it is a read whatever else it does.
    pub invisible_shell_named_file: Tally,
    /// Every call to the shell tool, recognised or not.
    pub shell_calls: u64,
    /// How many of them the pinned grammar recognised at all.
    pub shell_recognised: u64,
    /// Calls that perform more than one read — a chained command line.
    pub calls_with_several_reads: u64,
    /// Every read every repository-content call performs, members included.
    pub read_members: u64,
    /// Members the journal would have declined, allowed because a sibling read
    /// on the same command line was not held. The price of the all-or-nothing
    /// rule, stated rather than absorbed.
    pub declines_lost_to_a_sibling: u64,
    /// Repository-content calls whose result carried the client's own result
    /// object at all.
    pub results_with_a_response: u64,
    /// Of those, the ones the pinned rule could read a statement out of. The
    /// gap between the two is the corpus saying nothing rather than the rule
    /// refusing it, and a zero credit means one of those two things.
    pub statements_read: u64,
    /// Members the client's own statement credited coverage for after the
    /// call, which density could not vouch for before it.
    pub deliveries_credited: u64,
    /// Members whose result stated less than the window had been credited
    /// with, each of them an emptying and a re-record.
    pub deliveries_taken_back: u64,
    /// Reads of files outside the work tree, which scope containment takes out
    /// of the denominator.
    pub outside_work_tree: Tally,
    /// Reads of a file the window had just edited with something other than a
    /// whole-file write — the pool this harness declines to model as coverage.
    pub verification_after_edit: Tally,
    /// The two repeat pools a narrowing works on — overlapping held coverage,
    /// and over unheld lines of a read revision — each split by how the
    /// request stands against what is held: narrowable, multi, and not
    /// narrowable, in that order.
    pub narrowing_split: [[Tally; 3]; 2],
    /// Calls this build would have narrowed with narrowing enforced, and what
    /// that spares: the held lines' share of the result, net of the frames.
    pub narrowed_if_enforced: Tally,
    /// What those narrowing frames cost.
    pub narrowed_frame_tokens: u64,
    /// Of [`Report::narrowed_if_enforced`], the calls that were composed
    /// chains of several reads and what they spare. Apart because a chain is
    /// narrowed only when every member is held or narrowable, so it is the
    /// share a single unheld member forfeits.
    pub narrowed_chains_if_enforced: Tally,
    /// How many declines rest on the window's own whole-file write rather than
    /// on a read, so a reader can take them out.
    pub declines_resting_on_a_write: u64,
    /// How many context-window emptyings were replayed.
    pub evictions: u64,
    /// How many mutating tool calls advanced a revision.
    pub edits: u64,
    /// The basis of every token count in this report.
    pub tokenizer: &'static str,
    /// Tool names whose calls are none of the shapes above, and how many.
    pub other_tools: BTreeMap<String, u64>,
}

impl Report {
    /// One bucket's totals.
    #[must_use]
    pub fn bucket(&self, bucket: Bucket) -> Tally {
        self.buckets.get(bucket.key()).copied().unwrap_or_default()
    }

    /// What a decline is worth once its own reason frame is paid for.
    #[must_use]
    pub fn declined_net_tokens(&self) -> u64 {
        self.declined.tokens.saturating_sub(self.frame_tokens)
    }

    /// The tokens no channel above accounts for.
    #[must_use]
    pub fn remainder_tokens(&self) -> u64 {
        self.all_results
            .tokens
            .saturating_sub(self.repo_content.tokens)
    }
}

/// A percentage, with `0 / 0` defined as `0` — the same guard every other
/// rate in this tree divides behind.
#[must_use]
pub fn share_percent(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 * 100.0 / denominator as f64
    }
}

/// Fold one call into its bucket.
pub(super) fn land(report: &mut Report, bucket: Bucket, tokens: u64) {
    report.buckets.entry(bucket.key()).or_default().add(tokens);
}

/// The totals every other figure is a part of.
pub(super) fn finish(report: &mut Report) {
    let mut reads = Tally::default();
    let mut searches = Tally::default();
    for bucket in Bucket::ALL {
        let tally = report.bucket(bucket);
        if bucket == Bucket::Search {
            searches.merge(tally);
        } else {
            reads.merge(tally);
        }
    }
    report.reads = reads;
    report.searches = searches;
    let mut total = reads;
    total.merge(searches);
    report.repo_content = total;
}

#[cfg(test)]
mod tests {
    use super::super::replay::replay;
    use super::super::tests::fixture;
    use super::{share_percent, Bucket, Tally};

    #[test]
    fn the_share_is_the_declined_tokens_over_the_repository_content() {
        let tmp = tempfile::tempdir().unwrap();
        let report = replay(&fixture(tmp.path())).unwrap();
        // Every bucket is part of the denominator and nothing else is.
        let summed: u64 = Bucket::ALL.iter().map(|b| report.bucket(*b).tokens).sum();
        assert_eq!(summed, report.repo_content.tokens, "buckets add up");
        assert_eq!(
            report.reads.tokens + report.searches.tokens,
            report.repo_content.tokens,
            "the two channels are the whole denominator"
        );
        assert!(report.declined.tokens > 0, "something was declined");
        assert!(report.frame_tokens > 0, "the frames cost something");
        assert!(
            report.declined.tokens > report.frame_tokens,
            "the declines pay for their own frames: {} vs {}",
            report.declined.tokens,
            report.frame_tokens
        );
        assert_eq!(
            report.declined_net_tokens(),
            report.declined.tokens - report.frame_tokens,
            "net is gross less the frames"
        );
        assert!(
            report.declinable.tokens > report.declined.tokens,
            "the floor keeps something out of the declined pool"
        );
        let share = share_percent(report.declined.tokens, report.repo_content.tokens);
        assert!(share > 0.0 && share < 100.0, "a real share: {share}");
        assert_eq!(share_percent(1, 0), 0.0, "zero over zero is zero");
    }

    #[test]
    fn a_tally_saturates() {
        let mut tally = Tally {
            calls: u64::MAX,
            tokens: u64::MAX,
        };
        tally.add(5);
        assert_eq!(tally.calls, u64::MAX);
    }
}
