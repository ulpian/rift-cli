//! The row cap, applied to the rendered Markdown.
//!
//! Capping happens after conversion rather than inside the engine because the
//! engine renders PDFs straight to Markdown without a document model, so a cap
//! applied to the model would silently miss one of the fourteen formats. A
//! text pass sees every table the same way, which is also what makes it
//! testable without a single binary fixture.
//!
//! The rule a table is recognized by is GitHub-flavoured Markdown's own: a row
//! line, then a delimiter row of dashes, then body rows until a line that is
//! not a row. Fenced code is copied through untouched and never scanned, since
//! a code block quoting a table is not a table.

use crate::SheetInfo;

/// Cut every table to `table_rows` body rows, and report each one.
///
/// Returns the rewritten Markdown and one [`SheetInfo`] per table in document
/// order. Where rows are dropped, a single line — `(N more rows)` — takes their
/// place, so the count is visible to whoever reads the document and a truncated
/// table can never pass for a complete one.
///
/// The output always ends in a newline and uses `\n` line endings, whatever the
/// input used: a sidecar is content-addressed, and two conversions that differ
/// only in line endings would be two files.
#[must_use]
pub fn cap_tables(text: &str, table_rows: usize) -> (String, Vec<SheetInfo>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = String::with_capacity(text.len());
    let mut sheets = Vec::new();
    let mut heading = String::new();
    let mut fence: Option<String> = None;
    let mut at = 0_usize;

    while let Some(line) = lines.get(at) {
        if let Some(open) = fence.take() {
            push_line(&mut out, line);
            if !closes_fence(line, &open) {
                fence = Some(open);
            }
            at += 1;
            continue;
        }
        if let Some(marker) = fence_marker(line) {
            fence = Some(marker);
            push_line(&mut out, line);
            at += 1;
            continue;
        }
        if let Some(title) = heading_text(line) {
            heading = title;
        }

        let delimiter = lines.get(at + 1).copied();
        if is_row(line) && delimiter.is_some_and(is_delimiter_row) {
            push_line(&mut out, line);
            if let Some(delimiter) = delimiter {
                push_line(&mut out, delimiter);
            }
            at += 2;

            let mut rows = 0_u32;
            let mut kept = 0_u32;
            while let Some(row) = lines.get(at) {
                if !is_row(row) {
                    break;
                }
                rows += 1;
                if (kept as usize) < table_rows {
                    push_line(&mut out, row);
                    kept += 1;
                }
                at += 1;
            }
            if rows > kept {
                out.push('\n');
                out.push_str(&format!("({} more rows)\n", rows - kept));
            }
            sheets.push(SheetInfo {
                name: heading.clone(),
                rows,
                rows_kept: kept,
            });
            continue;
        }

        push_line(&mut out, line);
        at += 1;
    }

    (out, sheets)
}

fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

/// A table row: a line whose first non-space character is a pipe.
fn is_row(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

/// The delimiter row under a table's header: pipes and dashes, nothing else.
fn is_delimiter_row(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') {
        return false;
    }
    let inner = trimmed.trim_start_matches('|').trim_end_matches('|');
    let mut cells = 0_usize;
    for cell in inner.split('|') {
        let cell = cell.trim();
        let cell = cell.strip_prefix(':').unwrap_or(cell);
        let cell = cell.strip_suffix(':').unwrap_or(cell);
        if cell.is_empty() || !cell.chars().all(|ch| ch == '-') {
            return false;
        }
        cells += 1;
    }
    cells > 0
}

/// The text of an ATX heading, if this line is one.
fn heading_text(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    Some(
        trimmed
            .trim_start_matches('#')
            .trim()
            .trim_end_matches('#')
            .trim()
            .to_owned(),
    )
}

/// The backtick or tilde run that opens a fenced block, if this line opens one.
fn fence_marker(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    for ch in ['`', '~'] {
        let run: String = trimmed.chars().take_while(|c| *c == ch).collect();
        if run.chars().count() >= 3 {
            return Some(run);
        }
    }
    None
}

/// Whether this line closes a fence opened with `open`: the same character, at
/// least as long, and nothing after it.
fn closes_fence(line: &str, open: &str) -> bool {
    let Some(marker) = fence_marker(line) else {
        return false;
    };
    let same_character = marker.chars().next() == open.chars().next();
    let long_enough = marker.chars().count() >= open.chars().count();
    let rest_is_blank = line
        .trim_start()
        .strip_prefix(marker.as_str())
        .is_some_and(|rest| rest.trim().is_empty());
    same_character && long_enough && rest_is_blank
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: usize) -> String {
        let mut out = String::from("## Sheet1\n\n| a | b |\n| --- | --- |\n");
        for row in 0..rows {
            out.push_str(&format!("| {row} | x |\n"));
        }
        out
    }

    #[test]
    fn tables_are_capped_at_the_policy_rows() {
        let (capped, sheets) = cap_tables(&table(500), 200);
        let body = capped.lines().filter(|l| l.starts_with("| 4")).count();
        assert_eq!(capped.matches("\n| ").count(), 202); // header + delimiter + 200
        assert!(capped.contains("(300 more rows)"));
        assert!(!capped.contains("| 200 | x |"));
        assert!(capped.contains("| 199 | x |"));
        assert_eq!(sheets.len(), 1);
        assert_eq!(sheets[0].name, "Sheet1");
        assert_eq!(sheets[0].rows, 500);
        assert_eq!(sheets[0].rows_kept, 200);
        assert!(sheets[0].truncated());
        assert!(body > 0);
    }

    #[test]
    fn a_table_within_the_cap_is_untouched_and_not_reported_as_truncated() {
        let source = table(3);
        let (capped, sheets) = cap_tables(&source, 200);
        assert_eq!(capped, source);
        assert!(!capped.contains("more rows"));
        assert_eq!(sheets.len(), 1);
        assert!(!sheets[0].truncated());
    }

    /// Every sheet is capped, not just the first: a workbook's second sheet is
    /// where the rows nobody looked at usually are.
    #[test]
    fn every_sheet_is_capped_under_its_own_heading() {
        let source = format!("{}\n{}", table(5), table(5).replace("Sheet1", "Sheet2"));
        let (capped, sheets) = cap_tables(&source, 2);
        assert_eq!(sheets.len(), 2);
        assert_eq!(sheets[0].name, "Sheet1");
        assert_eq!(sheets[1].name, "Sheet2");
        for sheet in &sheets {
            assert_eq!(sheet.rows, 5);
            assert_eq!(sheet.rows_kept, 2);
        }
        assert_eq!(capped.matches("(3 more rows)").count(), 2);
    }

    /// A code block that quotes a table is prose about a table, and cutting it
    /// would corrupt the example it is showing.
    #[test]
    fn a_table_inside_fenced_code_is_left_alone() {
        let source = "```\n| a | b |\n| --- | --- |\n| 1 | 2 |\n| 3 | 4 |\n```\n";
        let (capped, sheets) = cap_tables(source, 1);
        assert_eq!(capped, source);
        assert!(sheets.is_empty());
    }

    #[test]
    fn text_without_tables_survives_unchanged_apart_from_a_final_newline() {
        let (capped, sheets) = cap_tables("# Title\n\nA paragraph.", 200);
        assert_eq!(capped, "# Title\n\nA paragraph.\n");
        assert!(sheets.is_empty());
        assert_eq!(cap_tables("", 200).0, "");
    }

    /// Windows line endings are normalized, because a sidecar is addressed by
    /// its content and two spellings of the same document must not be two files.
    #[test]
    fn line_endings_are_normalized() {
        let (crlf, _) = cap_tables("| a |\r\n| --- |\r\n| 1 |\r\n", 200);
        let (lf, _) = cap_tables("| a |\n| --- |\n| 1 |\n", 200);
        assert_eq!(crlf, lf);
    }

    #[test]
    fn a_delimiter_row_is_dashes_and_nothing_else() {
        assert!(is_delimiter_row("| --- | --- |"));
        assert!(is_delimiter_row("|---|"));
        assert!(is_delimiter_row("| :--- | ---: |"));
        assert!(!is_delimiter_row("| a | b |"));
        assert!(!is_delimiter_row("| | |"));
        assert!(!is_delimiter_row("not a row"));
    }
}
