//! The first sights cut by the two first-contact floors: every call of the
//! first-sight bucket lands in exactly one class, so the rows add up to the
//! bucket.
//!
//! A module of its own because it answers a question the size histogram
//! cannot — what the agent asked for of a large file — and every figure it
//! adds is on rows of its own, so every row before it reads the same with or
//! without it.
//!
//! **The floors** are the first-contact ones: a file is large when it has at
//! least 400 lines or at least 16,384 bytes, as the replay resolved it.
//!
//! **The classes**, taken in this order, a call in the first one it fits:
//!
//! 1. *Several reads in the call, or one that could not be read.* A call with
//!    more than one read member, or whose one member the checkout could say
//!    nothing about. The row beneath it counts those holding a large code
//!    member that was read.
//! 2. *Under both floors.* One read of a small file.
//! 3. *Large, not code.* One read of a large file whose extension is not a
//!    code one (the shipped test of what is code); the row beneath it counts
//!    the Markdown ones.
//! 4. *Large code, read whole.* One read of a large code file whose request
//!    named no end and began at line 1 — `cat file`, a read without a limit.
//! 5. *Large code, a range from line 1.* The request bounded its end, or
//!    counted from the bottom, and the lines it delivers begin at line 1: the
//!    first page of the file.
//! 6. *Large code, a range from mid-file.* The same, beginning anywhere else.

use rift_recall_validity::{is_code_path, is_markdown_path};
use serde_json::{json, Map, Value};

use super::census::{FIRST_CONTACT_BYTES, FIRST_CONTACT_LINES};
use super::render::{commas, row};
use super::report::{Report, Tally};

/// The classes, in the order the table prints them: a key and a label.
const CLASSES: [(&str, &str); 6] = [
    ("under_both_floors", "under both floors"),
    ("large_not_code", "large, not code"),
    ("code_whole", "large code, read whole (no range)"),
    ("code_first_page", "large code, a range from line 1"),
    ("code_mid_file", "large code, a range from mid-file"),
    ("multi_member_or_blocked", "multi-member or blocked"),
];

/// What the replay knows of one read member of a first sight.
#[derive(Clone, Copy, Debug)]
pub(super) struct Sight<'a> {
    /// The file, in the one spelling coverage is keyed by.
    pub(super) key: &'a str,
    /// How many lines the file has.
    pub(super) lines: u64,
    /// How many bytes the file has.
    pub(super) bytes: u64,
    /// Whether the request named no end and began at line 1.
    pub(super) unranged: bool,
    /// Whether the lines the request delivers begin at line 1.
    pub(super) from_line_one: bool,
}

/// The first sights by class.
#[derive(Clone, Debug, Default)]
pub struct FirstSightSplit {
    /// One tally per entry of `CLASSES`, in its order.
    classes: [Tally; 6],
    /// Of the large-not-code class, the Markdown files.
    markdown: Tally,
    /// Of the multi-member-or-blocked class, the calls holding a large code
    /// member that was read.
    multi_with_large_code: Tally,
}

impl Sight<'_> {
    /// Whether the file meets either first-contact floor.
    fn large(&self) -> bool {
        self.lines >= FIRST_CONTACT_LINES || self.bytes >= FIRST_CONTACT_BYTES
    }

    /// Whether this is a large code file.
    fn large_code(&self) -> bool {
        self.large() && is_code_path(self.key)
    }
}

impl FirstSightSplit {
    /// Count one first-sight call, given one entry per member in the order
    /// the command line performs them, `None` for a member nothing could be
    /// said about.
    pub(super) fn note(&mut self, members: &[Option<Sight<'_>>], tokens: u64) {
        let class = if let [Some(sight)] = members {
            self.single(sight, tokens)
        } else {
            if members.iter().flatten().any(Sight::large_code) {
                self.multi_with_large_code.add(tokens);
            }
            5
        };
        if let Some(tally) = self.classes.get_mut(class) {
            tally.add(tokens);
        }
    }

    /// The class of a call of one read, noting the Markdown part.
    fn single(&mut self, sight: &Sight<'_>, tokens: u64) -> usize {
        if !sight.large() {
            return 0;
        }
        if !is_code_path(sight.key) {
            if is_markdown_path(sight.key) {
                self.markdown.add(tokens);
            }
            return 1;
        }
        match (sight.unranged, sight.from_line_one) {
            (true, _) => 2,
            (false, true) => 3,
            (false, false) => 4,
        }
    }
}

/// The table's rows for the first sights cut by the two floors.
pub(super) fn first_sight_split_rows(report: &Report, denominator: u64) -> String {
    let split = &report.first_sight_split;
    let mut out = format!(
        "\n  first sight, split by the two floors (≥ {} lines or ≥ {} bytes)\n",
        commas(FIRST_CONTACT_LINES),
        commas(FIRST_CONTACT_BYTES)
    );
    for ((key, label), tally) in CLASSES.iter().zip(split.classes) {
        out.push_str(&row(label, tally, denominator));
        match *key {
            "large_not_code" => {
                out.push_str(&row("  of it, Markdown", split.markdown, denominator));
            }
            "multi_member_or_blocked" => out.push_str(&row(
                "  of it, holding a large code member",
                split.multi_with_large_code,
                denominator,
            )),
            _ => (),
        }
    }
    out
}

/// The same rows for the machine-readable document.
pub(super) fn first_sight_split_json(report: &Report) -> Value {
    let split = &report.first_sight_split;
    let tally = |t: Tally| json!({ "calls": t.calls, "tokens": t.tokens });
    let mut map = Map::new();
    for ((key, _), t) in CLASSES.iter().zip(split.classes) {
        map.insert((*key).to_owned(), tally(t));
    }
    map.insert("large_not_code_markdown".to_owned(), tally(split.markdown));
    map.insert(
        "multi_member_or_blocked_with_large_code".to_owned(),
        tally(split.multi_with_large_code),
    );
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sight(key: &str, lines: u64, unranged: bool, from_line_one: bool) -> Sight<'_> {
        Sight {
            key,
            lines,
            bytes: lines * 30,
            unranged,
            from_line_one,
        }
    }

    /// ★ One call of each class lands in its own row, the Markdown and
    /// large-code-member parts are counted beneath their classes, and the
    /// classes add up to the calls noted.
    #[test]
    fn every_first_sight_lands_in_one_class_and_the_classes_add_up() {
        let mut split = FirstSightSplit::default();
        split.note(&[Some(sight("a.rs", 100, true, true))], 1);
        split.note(&[Some(sight("doc.md", 500, true, true))], 2);
        split.note(&[Some(sight("data.json", 20_000, false, true))], 4);
        split.note(&[Some(sight("big.rs", 800, true, true))], 8);
        split.note(&[Some(sight("big.rs", 800, false, true))], 16);
        split.note(&[Some(sight("big.rs", 800, false, false))], 32);
        split.note(&[None], 64);
        split.note(
            &[
                Some(sight("a.rs", 10, true, true)),
                Some(sight("big.rs", 900, true, true)),
            ],
            128,
        );
        let tokens: Vec<u64> = split.classes.iter().map(|t| t.tokens).collect();
        assert_eq!(tokens, [1, 2 + 4, 8, 16, 32, 64 + 128]);
        assert_eq!(
            split.markdown,
            Tally {
                calls: 1,
                tokens: 2
            }
        );
        assert_eq!(
            split.multi_with_large_code,
            Tally {
                calls: 1,
                tokens: 128
            }
        );
        let calls: u64 = split.classes.iter().map(|t| t.calls).sum();
        assert_eq!(calls, 8, "each call is in exactly one class");
    }

    #[test]
    fn either_floor_makes_a_file_large_and_a_blocked_member_is_never_code() {
        let mut split = FirstSightSplit::default();
        let by_bytes = Sight {
            key: "x.rs",
            lines: 10,
            bytes: FIRST_CONTACT_BYTES,
            unranged: true,
            from_line_one: true,
        };
        split.note(&[Some(by_bytes)], 5);
        split.note(&[Some(sight("x.rs", 399, true, true))], 3);
        split.note(&[None, None], 9);
        assert_eq!(split.classes[2].tokens, 5, "16,384 bytes alone is large");
        assert_eq!(split.classes[0].tokens, 3, "399 lines of 30 bytes is not");
        assert_eq!(split.multi_with_large_code, Tally::default());
    }

    #[test]
    fn the_rows_carry_no_path_and_the_document_names_every_class() {
        let mut split = FirstSightSplit::default();
        split.note(&[Some(sight("private/name.md", 900, true, true))], 10);
        let report = Report {
            first_sight_split: split,
            ..Report::default()
        };
        let table = first_sight_split_rows(&report, 1_000);
        assert!(table.contains("of it, Markdown"), "{table}");
        assert!(!table.contains("private"), "no path reaches the table");
        let document = first_sight_split_json(&report);
        assert_eq!(document["large_not_code"]["calls"], 1);
        assert_eq!(document["large_not_code_markdown"]["tokens"], 10);
        assert_eq!(document["code_whole"]["calls"], 0);
    }
}
