//! The filter stage of a pipeline: a command a read may be piped into that
//! keeps or reshapes the lines reaching it and reads nothing of its own.
//!
//! A filter is admitted as much for what it leaves out as for what it does.
//! Each command here reads its standard input when it is given no operand,
//! and the grammar admits it only with none, so the lines it sees are the
//! lines the stage before it produced and never a file it opened itself.
//! What it hands on is a subset or a reshaping of those lines that the
//! command line does not pin, so a read behind a filter is counted and never
//! coverage: it is [`ShellSpan::Filtered`](super::ShellSpan::Filtered).
//!
//! The options are lists of what is admitted, not of what is refused. Every
//! one that would make a filter read or write a file is simply absent —
//! `grep -f` (a pattern file), `grep -r` (which with no operand searches the
//! working directory), `sort -o` (an output file), `sort -m` (inputs named as
//! operands) — so a list can only be short of an option, which costs a
//! saving, and never wrong about one, which would cost a claim. Long options
//! and `--` are absent for the same reason.
//!
//! `awk` is the one filter whose program is a language, so its program is
//! admitted by shape rather than by option: a single `print` of fields and
//! counters inside one action. Everything that lets `awk` open, write or run
//! something — a redirection, a pipe, `getline`, `system`, a `BEGIN` block, a
//! computed field — sits outside that shape, and `-f` and `-v` are absent.

use super::{bare, pattern_arg, Word};

/// The `grep` letters a filter may bundle: ones that choose, number, count
/// or cut the lines that matched, and the two that name the pattern syntax.
const GREP_LETTERS: &str = "niwxcvoFE";

/// The `grep` letters that take a line count, attached or as the next word.
const GREP_CONTEXT_LETTERS: &str = "ABC";

/// The `sort` letters a filter may bundle; `-k` and `-t` take a value.
const SORT_LETTERS: &str = "nrufbdV";

/// The `uniq` letters a filter may bundle. `uniq` takes no value and no
/// operand: its second operand is a file it *writes*.
const UNIQ_LETTERS: &str = "cdui";

/// The `wc` letters a filter may bundle: lines, words, bytes and characters.
const WC_LETTERS: &str = "lwcm";

/// The `cut` letters that take a list of positions.
const CUT_LIST_LETTERS: &str = "bcf";

/// Whether one pipeline stage, already split into words, is a filter.
pub(super) fn is_filter(words: &[Word]) -> bool {
    let Some((head, rest)) = words.split_first() else {
        return false;
    };
    match bare(head) {
        Some("grep" | "egrep" | "fgrep") => grep_filter(rest),
        Some("cut") => cut_filter(rest),
        Some("sort") => sort_filter(rest),
        Some("uniq") => rest.iter().all(|word| bundle(word, UNIQ_LETTERS)),
        Some("wc") => rest.iter().all(|word| bundle(word, WC_LETTERS)),
        Some("awk") => awk_filter(rest),
        _ => false,
    }
}

/// A `grep` that reads its input: pinned options, then exactly one pattern —
/// or none when `-e` gave it — and nothing after, since a word after the
/// pattern is a file `grep` would read in place of the pipe.
fn grep_filter(rest: &[Word]) -> bool {
    let mut given = false;
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        let Some(option) = bare(word).and_then(|text| text.strip_prefix('-')) else {
            return !given && is_pattern(word) && words.next().is_none();
        };
        if option == "e" {
            if !words.next().is_some_and(is_pattern) {
                return false;
            }
            given = true;
        } else if let Some(count) = context_count(option) {
            let ok = match count {
                "" => words
                    .next()
                    .is_some_and(|word| bare(word).is_some_and(is_number)),
                attached => is_number(attached),
            };
            if !ok {
                return false;
            }
        } else if !is_letters(option, GREP_LETTERS) {
            return false;
        }
    }
    given
}

/// The count after a context letter, possibly empty, or [`None`] when the
/// option is not a context option.
fn context_count(option: &str) -> Option<&str> {
    let mut chars = option.chars();
    let letter = chars.next()?;
    GREP_CONTEXT_LETTERS
        .contains(letter)
        .then_some(chars.as_str())
}

/// A pattern word: any quoted or bare text the walk resolved, and never one
/// a search tool would take for an option, quoted or not.
fn is_pattern(word: &Word) -> bool {
    pattern_arg(word).is_some_and(|text| !text.starts_with('-'))
}

/// A `cut` that reads its input: list and delimiter options only, at least
/// one list among them.
fn cut_filter(rest: &[Word]) -> bool {
    let mut listed = false;
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        let Some(option) = bare(word).and_then(|text| text.strip_prefix('-')) else {
            return false;
        };
        let mut chars = option.chars();
        let (letter, attached) = (chars.next(), chars.as_str());
        let ok = match letter {
            Some(letter) if CUT_LIST_LETTERS.contains(letter) => {
                listed = true;
                value(attached, &mut words, is_position_list)
            }
            Some('d') => value(attached, &mut words, |text| text.chars().count() == 1),
            Some('s') => attached.is_empty(),
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    listed
}

/// A `sort` that reads its input: bundled letters, a key and a separator.
fn sort_filter(rest: &[Word]) -> bool {
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        let Some(option) = bare(word).and_then(|text| text.strip_prefix('-')) else {
            return false;
        };
        let mut chars = option.chars();
        let (letter, attached) = (chars.next(), chars.as_str());
        let ok = match letter {
            Some('k') => value(attached, &mut words, is_sort_key),
            Some('t') => value(attached, &mut words, |text| text.chars().count() == 1),
            _ => is_letters(option, SORT_LETTERS),
        };
        if !ok {
            return false;
        }
    }
    true
}

/// An `awk` that reads its input: an optional one-character `-F`, then one
/// quoted program that prints fields, and nothing after — a word after the
/// program is a file `awk` would read in place of the pipe.
fn awk_filter(rest: &[Word]) -> bool {
    let mut words = rest.iter();
    let Some(mut program) = words.next() else {
        return false;
    };
    if let Some(attached) = bare(program).and_then(|text| text.strip_prefix("-F")) {
        if !value(attached, &mut words, |text| text.chars().count() == 1) {
            return false;
        }
        let Some(next) = words.next() else {
            return false;
        };
        program = next;
    }
    program.quoted && is_print_program(&program.text) && words.next().is_none()
}

/// A program that is one action printing fields and counters: `{print}`,
/// `{ print $1, $NF; }`. The shape is the whole admission, so whitespace is
/// the only freedom it leaves.
fn is_print_program(text: &str) -> bool {
    let Some(body) = text
        .trim()
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
    else {
        return false;
    };
    let body = body.trim();
    let body = body.strip_suffix(';').unwrap_or(body).trim_end();
    let Some(items) = body.strip_prefix("print") else {
        return false;
    };
    if items.is_empty() {
        return true;
    }
    items.starts_with([' ', '\t']) && items.split(',').all(|item| is_print_item(item.trim()))
}

/// A field or a counter: `$0`, `$3`, `$NF`, `NR`, `NF`.
fn is_print_item(item: &str) -> bool {
    match item.strip_prefix('$') {
        Some(field) => field == "NF" || is_number(field),
        None => matches!(item, "NR" | "NF"),
    }
}

/// An option's value, attached to it or as the next word, checked by `valid`.
///
/// The next word is taken only when nothing was attached, which is how
/// `-c1-80` and `-c 1-80` read alike and `-c1-80 file` still leaves the
/// `file` to be refused as an operand.
fn value<'w>(
    attached: &str,
    words: &mut impl Iterator<Item = &'w Word>,
    valid: impl Fn(&str) -> bool,
) -> bool {
    if !attached.is_empty() {
        return valid(attached);
    }
    words.next().is_some_and(|word| valid(&word.text))
}

/// An unquoted `-` and a run of letters, every one from `letters`.
fn bundle(word: &Word, letters: &str) -> bool {
    bare(word)
        .and_then(|text| text.strip_prefix('-'))
        .is_some_and(|option| is_letters(option, letters))
}

/// A non-empty run of letters, every one from `letters`.
fn is_letters(option: &str, letters: &str) -> bool {
    !option.is_empty() && option.chars().all(|c| letters.contains(c))
}

/// Plain ASCII digits, and at least one.
fn is_number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// A `cut` position list: digits, commas and ranges, as `1-80` or `2,4-`.
fn is_position_list(text: &str) -> bool {
    text.bytes().any(|b| b.is_ascii_digit())
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b',' || b == b'-')
}

/// A `sort` key: field and character positions with the ordering letters.
fn is_sort_key(text: &str) -> bool {
    text.bytes().next().is_some_and(|b| b.is_ascii_digit())
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ',' | '.') || SORT_LETTERS.contains(c))
}
