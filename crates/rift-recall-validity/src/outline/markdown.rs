//! The heading outline of a Markdown file: each ATX heading (`#` to
//! `######`) is a definition, spanning the lines up to the next heading of
//! its own level or a higher one.
//!
//! A module of its own because it is a second grammar over the same outline
//! type, and the rules that make it exact are read together: a heading is
//! only a line the CommonMark ATX rule makes one, a fenced block hides every
//! line inside it, and a Setext underline is not read at all — telling one
//! from a table rule or a front-matter fence takes more than a line scan
//! sees, and a heading this cannot be sure of is better absent than wrong.

use super::{line_number, Definition, DefinitionKind, MAX_NAME_CHARS};

/// The non-alphanumeric characters a heading name keeps. Every other
/// character becomes a space, so a name is already in the alphabet a frame
/// renders and punctuation never costs a reader the whole heading.
const NAME_PUNCTUATION: &[char] = &[' ', '.', '_', '/', '@', ':', '+', '-'];

/// An open fenced block: its fence character and the length of its run.
struct Fence {
    marker: char,
    length: usize,
}

/// The headings of `lines`, in file order, each with its span.
pub(super) fn headings(lines: &[&str]) -> Vec<Definition> {
    let found = atx_headings(lines);
    let mut definitions = Vec::new();
    for (at, (index, level, name)) in found.iter().enumerate() {
        let Some(name) = name else {
            continue;
        };
        let next = found
            .iter()
            .skip(at.saturating_add(1))
            .find(|(_, other, _)| other <= level)
            .map_or(lines.len(), |(next, _, _)| *next);
        let last = last_filled(lines, *index, next.saturating_sub(1));
        definitions.push(Definition {
            kind: DefinitionKind::Heading(*level),
            name: name.clone(),
            first_line: line_number(*index),
            last_line: line_number(last),
        });
    }
    definitions
}

/// Every ATX heading outside a fenced block: its line index, its level, and
/// its name — `None` for a heading with no text, which still ends the span
/// of the heading above it.
fn atx_headings(lines: &[&str]) -> Vec<(usize, u8, Option<String>)> {
    let mut found = Vec::new();
    let mut fence: Option<Fence> = None;
    for (index, line) in lines.iter().enumerate() {
        if let Some(open) = &fence {
            if closes(line, open) {
                fence = None;
            }
            continue;
        }
        if let Some(opened) = opens_fence(line) {
            fence = Some(opened);
            continue;
        }
        if let Some((level, name)) = atx_heading(line) {
            found.push((index, level, name));
        }
    }
    found
}

/// The last non-blank line from `first` to `last`, or `first` itself.
fn last_filled(lines: &[&str], first: usize, last: usize) -> usize {
    (first..=last.max(first))
        .rev()
        .find(|at| lines.get(*at).is_some_and(|line| !line.trim().is_empty()))
        .unwrap_or(first)
}

/// `line` without the up to three spaces a block may be indented by, or
/// `None` when it is indented further (an indented code line).
fn unindented(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches(' ');
    (line.len().saturating_sub(rest.len()) <= 3).then_some(rest)
}

/// The fence `line` opens, if it opens one: three or more backticks or
/// tildes, and for backticks an info string with no backtick in it.
fn opens_fence(line: &str) -> Option<Fence> {
    let rest = unindented(line)?;
    let marker = rest.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let length = rest.chars().take_while(|c| *c == marker).count();
    if length < 3 {
        return None;
    }
    let info = rest.get(length..).unwrap_or_default();
    if marker == '`' && info.contains('`') {
        return None;
    }
    Some(Fence { marker, length })
}

/// Whether `line` closes `open`: a run of its marker at least as long, and
/// nothing after it but spaces.
fn closes(line: &str, open: &Fence) -> bool {
    let Some(rest) = unindented(line) else {
        return false;
    };
    let length = rest.chars().take_while(|c| *c == open.marker).count();
    length >= open.length
        && rest
            .get(length..)
            .is_some_and(|after| after.trim().is_empty())
}

/// The level and name of an ATX heading line, or `None` when the line is
/// not one: one to six `#`, then a space, a tab or the end of the line.
fn atx_heading(line: &str) -> Option<(u8, Option<String>)> {
    let rest = unindented(line)?;
    let level = rest.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let after = rest.get(level..)?;
    if !(after.is_empty() || after.starts_with([' ', '\t'])) {
        return None;
    }
    let name = heading_name(without_closing_run(after.trim()));
    Some((u8::try_from(level).ok()?, name))
}

/// A heading's text without its optional closing run of `#`, which counts
/// only when a space separates it from the text or it is the whole text.
fn without_closing_run(text: &str) -> &str {
    let open = text.trim_end_matches('#');
    if open.len() == text.len() {
        return text;
    }
    if open.is_empty() {
        return "";
    }
    if open.ends_with([' ', '\t']) {
        return open.trim_end();
    }
    text
}

/// The name a heading's text becomes: every character outside the frame
/// alphabet turned into a space, runs of spaces collapsed, and the result cut
/// at the name ceiling — or `None` when nothing is left.
fn heading_name(text: &str) -> Option<String> {
    let kept: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || NAME_PUNCTUATION.contains(&c) {
                c
            } else {
                ' '
            }
        })
        .collect();
    let name: String = kept
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_NAME_CHARS)
        .collect();
    let name = name.trim_end().to_owned();
    (!name.is_empty()).then_some(name)
}
