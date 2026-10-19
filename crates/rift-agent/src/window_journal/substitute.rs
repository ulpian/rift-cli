//! Which stretches of a terminal result this window provably holds already,
//! and what replacing them with a short frame would save.
//!
//! The proof has two halves and both are required. The output matcher found
//! the delivered lines, byte for byte, among the line hashes of the revision
//! on disk; and the window's own record says it was handed those lines of
//! that same revision by a read (or wrote the whole of it), and has not lost
//! them since. Matched rows are never part of the proof: a delivery that
//! could vouch for itself would let one unproven match justify the next.
//!
//! Pure, like the rest of the journal. Nothing here rewrites a result: the
//! decision is what the lever would do, sized so its credit can be measured
//! before any session is handed a frame instead of its output.

use rift_core::Sha256Id;

use super::{Interval, Journal, Mode};

/// How far a terminal result may be rewritten when no policy says: record
/// what would be substituted, change nothing.
pub const SUBSTITUTE_MODE: Mode = Mode::Observe;

/// The lines of one file a terminal result delivered, as the matcher booked
/// them.
pub struct Delivered<'a> {
    /// The file, as the journal names it.
    pub path: &'a str,
    /// The revision the lines were matched against: the bytes on disk.
    pub hash: Sha256Id,
    /// The runs of file lines the result carried, sorted and disjoint.
    pub runs: &'a [Interval],
    /// The bytes the result spent on one file line, prefix and newline
    /// included: what the model was handed for it.
    pub line_bytes: &'a dyn Fn(u32) -> usize,
}

/// One stretch the window holds: lines `run` of `path` at `hash`, last
/// handed to it in turn `since_turn`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldRun {
    /// The file, as the journal names it.
    pub path: String,
    /// The revision both halves of the proof name.
    pub hash: Sha256Id,
    /// The held lines.
    pub run: Interval,
    /// The latest turn a read handed the window any of them.
    pub since_turn: u32,
    /// The bytes the result spent on them.
    pub delivered_bytes: usize,
    /// The bytes the frame standing for them would cost.
    pub frame_bytes: usize,
}

/// What substituting a result's held stretches would do, at the mode it
/// was decided under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldOutputDecision {
    /// The lever's mode: below `enforce` nothing is rewritten.
    pub mode: Mode,
    /// The stretches a frame would stand for, each cheaper as a frame.
    pub runs: Vec<HeldRun>,
}

impl HeldOutputDecision {
    /// The bytes the substitution saves: delivered minus frame, over the
    /// runs it takes. Never negative, because a run whose frame costs as
    /// much as its lines is not taken.
    #[must_use]
    pub fn credit_bytes(&self) -> usize {
        self.runs
            .iter()
            .map(|held| held.delivered_bytes.saturating_sub(held.frame_bytes))
            .sum()
    }

    /// Whether the result would be rewritten at all.
    #[must_use]
    pub fn rewrites(&self) -> bool {
        self.mode == Mode::Enforce && !self.runs.is_empty()
    }
}

/// The held stretches of `delivered`, priced by `frame_bytes`, under `mode`;
/// `None` when the lever is off or nothing is both held and worth a frame.
#[must_use]
pub fn held_output(
    journal: &Journal,
    delivered: &Delivered<'_>,
    mode: Mode,
    frame_bytes: &dyn Fn(&HeldRun) -> usize,
) -> Option<HeldOutputDecision> {
    if mode == Mode::Off {
        return None;
    }
    let coverage = journal.coverage(delivered.path, delivered.hash);
    let mut runs = Vec::new();
    for &run in delivered.runs {
        for held in coverage.runs() {
            let Some(stretch) = held.intersection(run) else {
                continue;
            };
            let Some(mut candidate) = held_run(journal, delivered, stretch) else {
                continue;
            };
            candidate.frame_bytes = frame_bytes(&candidate);
            if candidate.frame_bytes < candidate.delivered_bytes {
                runs.push(candidate);
            }
        }
    }
    (!runs.is_empty()).then_some(HeldOutputDecision { mode, runs })
}

/// One held stretch with its delivered size, or `None` when no read names
/// the turn it was handed in: a proof that cannot name its turn cannot be
/// stated in a frame, so it substitutes nothing.
fn held_run(journal: &Journal, delivered: &Delivered<'_>, stretch: Interval) -> Option<HeldRun> {
    let since_turn = journal.read_turn(delivered.path, delivered.hash, stretch)?;
    let delivered_bytes = (stretch.lo..=stretch.hi)
        .map(|line| (delivered.line_bytes)(line))
        .sum();
    Some(HeldRun {
        path: delivered.path.to_owned(),
        hash: delivered.hash,
        run: stretch,
        since_turn,
        delivered_bytes,
        frame_bytes: 0,
    })
}

/// The result text with each held stretch replaced by its frame, or `None`
/// when no stretch could be replaced.
///
/// A stretch is replaced only where the result carries the file's own lines
/// `lo..=hi` byte for byte, starting at a line start and ending at a line
/// break. The proof above is about the file; this check is about the result,
/// so a stretch the result printed any other way (a `12:` prefix, a cut last
/// line, a different revision) is left alone rather than trusted. Every byte
/// outside a replaced stretch is kept as it came, and stretches are taken in
/// order, each after the one before it.
#[must_use]
pub fn substitute_held_lines(
    result: &str,
    file: &[u8],
    stretches: &[(Interval, String)],
) -> Option<String> {
    let lines: Vec<&[u8]> = file.split_inclusive(|byte| *byte == b'\n').collect();
    let mut out = String::with_capacity(result.len());
    let mut from = 0;
    for (run, frame) in stretches {
        let lo = usize::try_from(run.lo.saturating_sub(1)).ok()?;
        let hi = usize::try_from(run.hi).ok()?;
        let Some(held) = lines.get(lo..hi).map(<[&[u8]]>::concat) else {
            continue;
        };
        let Ok(held) = std::str::from_utf8(&held) else {
            continue;
        };
        let Some(at) = line_aligned_find(result, from, held) else {
            continue;
        };
        out.push_str(result.get(from..at)?);
        out.push_str(frame);
        out.push('\n');
        from = at + held.len();
    }
    if from == 0 && out.is_empty() {
        return None;
    }
    out.push_str(result.get(from..)?);
    Some(out)
}

/// Where `held` occurs in `result` at or after `from`, beginning a line and
/// ending with a line break.
fn line_aligned_find(result: &str, from: usize, held: &str) -> Option<usize> {
    if held.is_empty() || !held.ends_with('\n') {
        return None;
    }
    let mut start = from;
    while let Some(found) = result.get(start..)?.find(held) {
        let at = start + found;
        if at == 0 || result.as_bytes().get(at - 1) == Some(&b'\n') {
            return Some(at);
        }
        start = at + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{hash, read_line, run};
    use super::super::{matched_lines, EvictReason, Line};
    use super::*;

    const FRAME: usize = 60;

    fn decide(journal: &Journal, runs: &[Interval], mode: Mode) -> Option<HeldOutputDecision> {
        let delivered = Delivered {
            path: "src/a.rs",
            hash: hash(1),
            runs,
            line_bytes: &|_| 40,
        };
        held_output(journal, &delivered, mode, &|_| FRAME)
    }

    /// ★ Lines a read handed the window, delivered again at the same
    /// revision, are held: one stretch per held part, the turn named, the
    /// credit delivered minus frame, and nothing rewritten below `enforce`.
    #[test]
    fn substitute_names_the_held_part_of_a_delivery_and_its_credit() {
        let journal = Journal::from_lines(vec![read_line(3, "src/a.rs", 1, 10, 29)]);
        let decision = decide(&journal, &[run(1, 40)], SUBSTITUTE_MODE).unwrap();
        assert_eq!(decision.runs.len(), 1);
        let held = &decision.runs[0];
        assert_eq!((held.run, held.since_turn), (run(10, 29), 3));
        assert_eq!(held.delivered_bytes, 20 * 40);
        assert_eq!(decision.credit_bytes(), 20 * 40 - FRAME);
        assert!(!decision.rewrites());
        let enforced = decide(&journal, &[run(1, 40)], Mode::Enforce).unwrap();
        assert!(enforced.rewrites());
    }

    /// ★ Another revision, a lost context, a matched row standing alone, or
    /// a lever that is off: nothing is held.
    #[test]
    fn substitute_takes_nothing_the_window_cannot_prove_it_holds() {
        let other_revision = Journal::from_lines(vec![read_line(3, "src/a.rs", 2, 1, 40)]);
        assert_eq!(decide(&other_revision, &[run(1, 40)], Mode::Enforce), None);
        let evicted = Journal::from_lines(vec![
            read_line(3, "src/a.rs", 1, 1, 40),
            Line::Evict {
                turn: 4,
                at_ms: 0,
                reason: EvictReason::Compact,
            },
        ]);
        assert_eq!(decide(&evicted, &[run(1, 40)], Mode::Enforce), None);
        let only_matched = Journal::from_lines(matched_lines(
            "src/a.rs",
            hash(1),
            &[run(1, 40)],
            120,
            0,
            2,
            0,
        ));
        assert_eq!(decide(&only_matched, &[run(1, 40)], Mode::Enforce), None);
        let held = Journal::from_lines(vec![read_line(3, "src/a.rs", 1, 1, 40)]);
        assert_eq!(decide(&held, &[run(1, 40)], Mode::Off), None);
    }

    /// A held stretch whose frame costs at least what it delivered is kept
    /// verbatim.
    #[test]
    fn substitute_keeps_a_stretch_shorter_than_its_frame() {
        let journal = Journal::from_lines(vec![read_line(3, "src/a.rs", 1, 5, 5)]);
        assert_eq!(decide(&journal, &[run(1, 40)], Mode::Enforce), None);
    }

    const FILE: &[u8] = b"one\ntwo\nthree\nfour\nfive\n";

    #[test]
    fn substitute_held_lines_replaces_exactly_the_held_stretches_and_keeps_the_rest() {
        let result = "$ build\none\ntwo\nthree\nmiddle\nfour\nfive\nok\n";
        let stretches = [
            (run(1, 3), "[frame A]".to_owned()),
            (run(4, 5), "[frame B]".to_owned()),
        ];
        assert_eq!(
            substitute_held_lines(result, FILE, &stretches).as_deref(),
            Some("$ build\n[frame A]\nmiddle\n[frame B]\nok\n")
        );
    }

    #[test]
    fn substitute_never_replaces_a_stretch_whose_bytes_differ_from_the_held_lines() {
        let numbered = "1:one\n2:two\n3:three\n";
        let stretches = [(run(1, 3), "[frame]".to_owned())];
        assert_eq!(substitute_held_lines(numbered, FILE, &stretches), None);
        let cut_short = "one\ntwo\nthre";
        assert_eq!(substitute_held_lines(cut_short, FILE, &stretches), None);
        let mid_line = "xone\ntwo\nthree\n";
        assert_eq!(substitute_held_lines(mid_line, FILE, &stretches), None);
    }

    #[test]
    fn substitute_leaves_an_unmatched_stretch_verbatim_beside_a_replaced_one() {
        let result = "one\ntwo\nFOUR\nfive\n";
        let stretches = [
            (run(1, 2), "[frame]".to_owned()),
            (run(4, 5), "[other]".to_owned()),
        ];
        assert_eq!(
            substitute_held_lines(result, FILE, &stretches).as_deref(),
            Some("[frame]\nFOUR\nfive\n")
        );
    }
}
