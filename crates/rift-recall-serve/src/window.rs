//! Which lines around a changed place a reader is shown — decided without a
//! parser.
//!
//! A changed range on its own is rarely readable: three added lines in the
//! middle of a function tell a reader what moved and nothing about what it
//! moved inside of. The remedy every editor uses is a window, and the two
//! rules here are the two an editor uses: a fixed number of lines either side
//! ([`CONTEXT_LINES`](crate::window::CONTEXT_LINES)), or a grow-until-the-indentation-ends rule
//! ([`MAX_BLOCK_LINES`](crate::window::MAX_BLOCK_LINES)) that reaches the enclosing blank line or column-zero
//! line and stops.
//!
//! **The block rule is a window rule, and it is named as one.** It is
//! deliberately language-blind: it counts leading whitespace and looks for
//! blank lines, which is a heuristic about how source is laid out and not a
//! claim about scopes, so every served window says which of the two rules
//! produced it ([`WindowRule::name`](crate::window::WindowRule::name)) and nothing here ever calls a window a
//! function, a block or a definition. A parser would answer better and is
//! ruled out for this tree; a rule that claimed to be one would be worse than
//! the heuristic, because a reader would believe it.
//!
//! Everything in this module is pure: lines in, windows out, no store, no
//! clock, no filesystem.

use std::fmt::Write as _;

use rift_recall_ledger::hunks::split_lines;

/// Lines shown either side of a changed place when the block rule does not
/// apply — the ordinary diff context.
pub const CONTEXT_LINES: u32 = 3;

/// The largest window the block rule may produce. A change inside a very long
/// indented run would otherwise pull the whole run in, which costs the caller
/// the file it was trying not to re-read; past this the answer falls back to
/// [`CONTEXT_LINES`].
pub const MAX_BLOCK_LINES: usize = 40;

/// Which rule produced a window. Carried into the payload so a reader is never
/// told a window is more than it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowRule {
    /// Grown to the nearest blank or column-zero line each side.
    Block,
    /// [`CONTEXT_LINES`] either side.
    Context,
}

impl WindowRule {
    /// The payload spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Context => "context",
        }
    }
}

/// One run of lines to show, 1-based and inclusive at both ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    /// First line shown.
    pub lo: u32,
    /// Last line shown.
    pub hi: u32,
    /// Which rule produced it.
    pub rule: WindowRule,
}

/// The lines of `bytes`, terminators stripped, in the numbering
/// [`rift_recall_ledger::hunks`] and [`crate::held_diff`] use.
///
/// The split is the diff engine's own ([`split_lines`]) rather than
/// `str::lines`, because a range names an index into *that* sequence: two
/// splitters that disagree about a final line without a terminator would put a
/// reader one line out at the end of every file.
#[must_use]
pub fn text_lines(bytes: &[u8]) -> Vec<String> {
    split_lines(bytes)
        .into_iter()
        .map(|l| {
            String::from_utf8_lossy(l)
                .trim_end_matches('\n')
                .trim_end_matches('\r')
                .to_owned()
        })
        .collect()
}

/// Whether a line ends a block for the growth rule: blank, or starting at
/// column zero.
///
/// Both halves are about layout, not syntax. A blank line separates paragraphs
/// of code in every language anyone writes this way; a line whose first
/// character is not whitespace is the outermost level of whatever the file is,
/// which in an indented language is the enclosing declaration and in a flat one
/// is simply the previous line. Where the guess is wrong the cost is a window
/// that is bigger or smaller than a person would have chosen, never a wrong
/// line number: the changed range itself is exact and the window only ever
/// grows around it.
fn is_boundary(line: &str) -> bool {
    line.trim().is_empty() || !line.starts_with([' ', '\t'])
}

/// Grow one changed range into a window over `lines`.
///
/// The block rule walks outward from the range to the first boundary line each
/// side and takes it as the edge — the `fn foo() {` above and the blank line
/// below are the two lines that make the change readable, so they are inside
/// the window rather than just outside it. A range that reaches neither edge
/// inside [`MAX_BLOCK_LINES`] gets [`CONTEXT_LINES`] instead.
fn window_for(lines: &[String], lo: u32, hi: u32) -> Window {
    let total = u32::try_from(lines.len()).unwrap_or(u32::MAX);
    let lo = lo.clamp(1, total.max(1));
    let hi = hi.clamp(lo, total.max(1));
    let at = |n: u32| lines.get(n as usize - 1).map_or("", String::as_str);

    let mut start = lo;
    while start > 1 {
        start -= 1;
        if is_boundary(at(start)) {
            break;
        }
    }
    let mut end = hi;
    while end < total {
        end += 1;
        if is_boundary(at(end)) {
            break;
        }
    }
    let span = (end - start + 1) as usize;
    if span <= MAX_BLOCK_LINES {
        return Window {
            lo: start,
            hi: end,
            rule: WindowRule::Block,
        };
    }
    Window {
        lo: lo.saturating_sub(CONTEXT_LINES).max(1),
        hi: hi.saturating_add(CONTEXT_LINES).min(total.max(1)),
        rule: WindowRule::Context,
    }
}

/// The windows for a complete list of changed ranges, in line order, with
/// touching or overlapping windows merged.
///
/// A merge takes the weaker of the two rules: a window that is `block` on one
/// half and `context` on the other is not a block, and saying so would be the
/// one kind of over-claim this module exists to avoid.
#[must_use]
pub fn windows(lines: &[String], ranges: &[(u32, u32)]) -> Vec<Window> {
    let mut out: Vec<Window> = Vec::new();
    for &(lo, hi) in ranges {
        if lines.is_empty() {
            break;
        }
        let w = window_for(lines, lo, hi);
        match out.last_mut() {
            // Adjacent counts as overlapping: a one-line gap between two
            // windows would be elided as "(1 unchanged line)", which costs
            // more to read than the line itself.
            Some(prev) if w.lo <= prev.hi.saturating_add(1) => {
                prev.hi = prev.hi.max(w.hi);
                if w.rule == WindowRule::Context {
                    prev.rule = WindowRule::Context;
                }
            }
            _ => out.push(w),
        }
    }
    out
}

/// Render `windows` over `lines` as numbered current-file lines, with every
/// unchanged run between them replaced by its count.
///
/// The elision is what makes the answer smaller than the file while staying
/// complete: every line of the file is either shown or counted, so a reader
/// can see that nothing was quietly dropped, and the numbers are the current
/// file's own — the numbering the reader will see if they open it.
#[must_use]
pub fn render(lines: &[String], windows: &[Window]) -> String {
    let total = u32::try_from(lines.len()).unwrap_or(u32::MAX);
    let mut out = String::new();
    let mut cursor = 1u32;
    for w in windows {
        elide(&mut out, w.lo.saturating_sub(cursor));
        for n in w.lo..=w.hi {
            if let Some(line) = lines.get(n as usize - 1) {
                let _ = writeln!(out, "{n}: {line}");
            }
        }
        cursor = w.hi.saturating_add(1);
    }
    elide(&mut out, total.saturating_add(1).saturating_sub(cursor));
    out
}

/// One elided run, or nothing when the run is empty.
fn elide(out: &mut String, n: u32) {
    if n == 0 {
        return;
    }
    let unit = if n == 1 { "line" } else { "lines" };
    let _ = writeln!(out, "… ({n} unchanged {unit})");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file with one indented run bounded above by a column-zero line and
    /// below by a blank line, then a second declaration.
    fn fixture() -> Vec<String> {
        text_lines(
            b"use std::io;\n\
              \n\
              fn alpha() {\n    \
                  let a = 1;\n    \
                  let b = 2;\n    \
                  let c = 3;\n\
              }\n\
              \n\
              fn bravo() {\n    \
                  let d = 4;\n\
              }\n",
        )
    }

    /// The growth rule stops where the layout says the block does — at the
    /// column-zero line above and the closing column-zero line below — and it
    /// says which rule it used.
    #[test]
    fn block_expansion_stops_at_blank_or_column_zero_and_the_cap() {
        let lines = fixture();
        // Line 5 (`let b = 2;`) is inside `fn alpha`, whose signature is line
        // 3 and whose closing brace is line 7 — both at column zero.
        let ws = windows(&lines, &[(5, 5)]);
        assert_eq!(
            ws,
            [Window {
                lo: 3,
                hi: 7,
                rule: WindowRule::Block
            }],
            "{ws:?}"
        );

        // The same change in a run longer than the cap falls back to context.
        let mut long = vec!["fn wide() {".to_owned()];
        long.extend((0..MAX_BLOCK_LINES + 4).map(|n| format!("    let x{n} = {n};")));
        long.push("}".to_owned());
        let ws = windows(&long, &[(20, 20)]);
        assert_eq!(
            ws,
            [Window {
                lo: 20 - CONTEXT_LINES,
                hi: 20 + CONTEXT_LINES,
                rule: WindowRule::Context
            }],
            "{ws:?}"
        );
        // THE CAP ITSELF, one line either side of it. `fn wide() {` and its
        // closing `}` are the two boundary lines, so a body of `n` indented
        // lines makes a window of exactly `n + 2` — forty lines and then
        // forty-one, written as the numbers they are rather than as
        // `MAX_BLOCK_LINES ± k`: a fixture spelled in terms of the constant
        // moves with it, and a boundary that moves with the thing it bounds
        // pins nothing.
        let framed = |body: usize| {
            let mut lines = vec!["fn wide() {".to_owned()];
            lines.extend((0..body).map(|n| format!("    let x{n} = {n};")));
            lines.push("}".to_owned());
            windows(&lines, &[(2, 2)])
        };
        let at_cap = framed(38);
        assert_eq!(
            at_cap,
            [Window {
                lo: 1,
                hi: 40,
                rule: WindowRule::Block
            }],
            "a forty-line window is still a block: {at_cap:?}"
        );
        let over_cap = framed(39);
        assert_eq!(
            over_cap,
            [Window {
                lo: 1,
                hi: 2 + CONTEXT_LINES,
                rule: WindowRule::Context
            }],
            "a forty-one-line window is not: {over_cap:?}"
        );

        assert_eq!(WindowRule::Block.name(), "block");
        assert_eq!(WindowRule::Context.name(), "context");
    }

    /// A blank line is an edge as much as a column-zero line is.
    #[test]
    fn a_blank_line_is_an_edge_of_the_window() {
        let lines = text_lines(b"a = 1\n\n    b = 2\n    c = 3\n\n    d = 4\n");
        let ws = windows(&lines, &[(4, 4)]);
        assert_eq!(ws[0].lo, 2, "the blank line above: {ws:?}");
        assert_eq!(ws[0].hi, 5, "the blank line below: {ws:?}");
    }

    /// Two changed places whose windows touch are one window, and a merge
    /// that mixes the rules reports the weaker of the two: a window that is a
    /// block on one half and three lines of context on the other is not a
    /// block, and calling it one is the single over-claim this module exists
    /// to avoid.
    #[test]
    fn touching_windows_merge_and_keep_the_weaker_rule() {
        let mut lines = vec!["fn a() {".to_owned(), "    x();".to_owned(), "}".to_owned()];
        lines.extend((1..=67).map(|n| format!("    p{n}();")));
        // Line 2 sits in a three-line block; line 4 opens a run far longer
        // than the cap, so its own window is a context window.
        let ws = windows(&lines, &[(2, 2), (4, 4)]);
        assert_eq!(
            ws,
            [Window {
                lo: 1,
                hi: 7,
                rule: WindowRule::Context
            }],
            "{ws:?}"
        );
    }

    /// Every line of the file is either shown with its current number or
    /// counted in an elision — the property that lets a reader trust that the
    /// gaps are unchanged rather than dropped.
    #[test]
    fn unchanged_runs_are_elided_with_their_count_and_nothing_is_dropped() {
        let lines = fixture();
        let text = render(&lines, &windows(&lines, &[(5, 5)]));
        assert_eq!(
            text,
            "… (2 unchanged lines)\n\
             3: fn alpha() {\n\
             4:     let a = 1;\n\
             5:     let b = 2;\n\
             6:     let c = 3;\n\
             7: }\n\
             … (4 unchanged lines)\n"
        );
        let shown = text.lines().filter(|l| !l.starts_with('…')).count();
        let counted: usize = text
            .lines()
            .filter_map(|l| l.strip_prefix("… ("))
            .filter_map(|l| l.split(' ').next())
            .filter_map(|n| n.parse::<usize>().ok())
            .sum();
        assert_eq!(shown + counted, lines.len(), "{text}");
    }

    /// A single elided line is singular, because a frame that cannot count to
    /// one is a frame nobody believes about anything else either.
    #[test]
    fn one_elided_line_is_singular() {
        let mut out = String::new();
        elide(&mut out, 1);
        assert_eq!(out, "… (1 unchanged line)\n");
    }

    /// The line splitter is the diff engine's own, including its answer for a
    /// last line with no terminator.
    #[test]
    fn the_line_split_matches_the_diff_engines() {
        assert_eq!(text_lines(b"a\nb"), ["a", "b"]);
        assert_eq!(text_lines(b"a\r\nb\n"), ["a", "b"]);
        assert!(text_lines(b"").is_empty());
        assert!(windows(&text_lines(b""), &[(1, 1)]).is_empty());
    }

    /// A range at either end of the file cannot walk off it.
    #[test]
    fn a_range_at_the_edge_of_the_file_stays_inside_it() {
        let lines = fixture();
        let ws = windows(&lines, &[(1, 1), (11, 11)]);
        assert_eq!(ws[0].lo, 1);
        assert_eq!(ws.last().unwrap().hi, 11);
        let text = render(&lines, &ws);
        assert!(text.starts_with("1: use std::io;\n"), "{text}");
        assert!(text.ends_with("11: }\n"), "{text}");
    }
}
