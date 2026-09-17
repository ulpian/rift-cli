//! What the query grammar means to this index.
//!
//! The grammar is the finder's — `*.rs` is an extension, `/src/` is a path
//! segment, `!test` is an exclusion, `status:modified` is a git question — and
//! this module is the half that says what each of those does to a list of
//! repository-relative paths.
//!
//! One rule governs the whole module: **a constraint this engine cannot answer
//! keeps every path.** An unknown file type, a glob too long to be worth
//! matching, a git status nobody collected — each filters nothing. The
//! alternative is a query that silently returns an empty answer, which reads to
//! an agent as "there is no such file" and sends it back to reading its way to
//! one. Filtering less is a worse answer; filtering wrongly is a false one.

use fff_query_parser::{Constraint, FFFQuery, FileSearchConfig, FuzzyQuery, GitStatusFilter};

use crate::git::StatusFacts;

/// The longest glob a constraint may carry before it stops being a filter.
///
/// The matcher's cost is the pattern's length times the path's, on every
/// candidate; a pattern longer than this is ignored rather than allowed to
/// multiply out over a whole index.
pub const MAX_GLOB_BYTES: usize = 256;

/// The literal a query asks for *inside* the files it matched.
///
/// The grammar's constraints say which paths a query is about; its fuzzy part
/// is the text the caller typed to find something, and that text is what a
/// content search looks for. A caller that typed `*.rs read gate` is asking
/// about Rust files and about the words `read gate`, and this is the half that
/// belongs in a line matcher.
///
/// Answered here, as an owned string, rather than by handing a caller the
/// parsed query: this crate is the only one that names the grammar, and a
/// caller that had to match on [`FuzzyQuery`] would be a second one.
///
/// Empty when the query carries nothing but constraints — `*.rs !tests/` names
/// a set of files and no text to find in them — which the content primitive
/// answers with no lines rather than with every line.
#[must_use]
pub fn literal_needle(text: &str) -> String {
    match FFFQuery::parse(text, FileSearchConfig).fuzzy_query {
        FuzzyQuery::Empty => String::new(),
        FuzzyQuery::Text(needle) => needle.to_owned(),
        FuzzyQuery::Parts(parts) => parts.join(" "),
    }
}

/// Whether a path survives every constraint in a query.
#[must_use]
pub fn keeps(constraints: &[Constraint<'_>], path: &str, status: &StatusFacts) -> bool {
    constraints
        .iter()
        .all(|constraint| satisfies(constraint, path, status))
}

/// Whether one constraint keeps this path.
#[must_use]
pub fn satisfies(constraint: &Constraint<'_>, path: &str, status: &StatusFacts) -> bool {
    match constraint {
        Constraint::Extension(ext) => extension(path).is_some_and(|e| e.eq_ignore_ascii_case(ext)),
        Constraint::Glob(pattern) => {
            pattern.len() > MAX_GLOB_BYTES || rift_config::glob_match(pattern, path)
        }
        Constraint::Text(text) => contains_ignore_case(path, text),
        Constraint::Parts(parts) => parts.iter().all(|part| contains_ignore_case(path, part)),
        Constraint::Exclude(parts) => !parts.iter().any(|part| contains_ignore_case(path, part)),
        Constraint::PathSegment(segment) => path.split('/').any(|part| part == *segment),
        Constraint::FilePath(suffix) => ends_at_boundary(path, suffix),
        Constraint::FileType(name) => match extensions_for_type(name) {
            Some(exts) => extension(path).is_some_and(|e| exts.contains(&e.as_str())),
            None => true,
        },
        Constraint::GitStatus(filter) => match filter {
            GitStatusFilter::Modified => status.modified.contains(path),
            GitStatusFilter::Staged => status.staged.contains(path),
            GitStatusFilter::Untracked => status.is_untracked(path),
            GitStatusFilter::Unmodified => !status.is_changed(path) && !status.is_untracked(path),
        },
        Constraint::Not(inner) => !satisfies(inner, path, status),
    }
}

/// A path's extension, lowercased, without the dot.
#[must_use]
pub fn extension(path: &str) -> Option<String> {
    let name = path.rsplit('/').next()?;
    let dot = name.rfind('.')?;
    let ext = name.get(dot + 1..)?;
    (!ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// Whether `path` ends with `suffix` at a path boundary.
///
/// The boundary is what keeps `src/bus.rs` from being answered by
/// `crates/rift/src/nimbus.rs`: a suffix constraint names a file, not a
/// sequence of characters that happens to end one.
fn ends_at_boundary(path: &str, suffix: &str) -> bool {
    if path == suffix {
        return true;
    }
    path.strip_suffix(suffix)
        .is_some_and(|head| head.ends_with('/'))
}

/// ASCII-case-insensitive substring test.
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

/// The extensions a `type:` name stands for.
///
/// A short table on purpose: it covers the languages this workspace and the
/// repositories it runs on are written in, and every name outside it is a
/// constraint that filters nothing rather than a constraint that filters
/// everything.
fn extensions_for_type(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "rust" => &["rs"],
        "python" => &["py", "pyi"],
        "js" | "javascript" => &["js", "jsx", "mjs", "cjs"],
        "ts" | "typescript" => &["ts", "tsx"],
        "go" => &["go"],
        "c" => &["c", "h"],
        "cpp" | "c++" => &["cc", "cpp", "cxx", "hpp", "hh"],
        "java" => &["java"],
        "ruby" => &["rb"],
        "md" | "markdown" => &["md", "markdown"],
        "toml" => &["toml"],
        "json" => &["json"],
        "yaml" | "yml" => &["yaml", "yml"],
        "html" => &["html", "htm"],
        "css" => &["css", "scss"],
        "sh" | "shell" => &["sh", "bash", "zsh"],
        "sql" => &["sql"],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fff_query_parser::{FFFQuery, FileSearchConfig};
    use std::collections::BTreeSet;

    const PATHS: [&str; 6] = [
        "crates/rift-canopy/src/layout.rs",
        "crates/rift-canopy-data/src/bus.rs",
        "docs/guide/03-layout.md",
        "bins/rift/src/main.rs",
        "notes/scratch.md",
        "Cargo.toml",
    ];

    fn kept(query: &str, status: &StatusFacts) -> Vec<&'static str> {
        let parsed = FFFQuery::parse(query, FileSearchConfig);
        PATHS
            .iter()
            .filter(|path| keeps(&parsed.constraints, path, status))
            .copied()
            .collect()
    }

    /// Each form of the grammar narrows the list the way its spelling promises.
    #[test]
    fn every_constraint_narrows_the_list_the_way_it_reads() {
        let none = StatusFacts::default();
        assert_eq!(
            kept("layout *.md", &none),
            vec!["docs/guide/03-layout.md", "notes/scratch.md"],
            "extension"
        );
        assert_eq!(
            kept("layout /docs/", &none),
            vec!["docs/guide/03-layout.md"],
            "path segment"
        );
        assert_eq!(
            kept("layout !docs", &none),
            vec![
                "crates/rift-canopy/src/layout.rs",
                "crates/rift-canopy-data/src/bus.rs",
                "bins/rift/src/main.rs",
                "notes/scratch.md",
                "Cargo.toml",
            ],
            "exclusion"
        );
        assert_eq!(
            kept("bus **/rift-canopy-data/**", &none),
            vec!["crates/rift-canopy-data/src/bus.rs"],
            "glob"
        );
        assert_eq!(
            kept("main type:rust", &none),
            vec![
                "crates/rift-canopy/src/layout.rs",
                "crates/rift-canopy-data/src/bus.rs",
                "bins/rift/src/main.rs",
            ],
            "file type"
        );
    }

    /// A constraint with no facts behind it filters nothing, because an empty
    /// answer reads as "there is no such file" and sends the agent back to
    /// reading.
    #[test]
    fn a_constraint_this_engine_cannot_answer_keeps_every_path() {
        let none = StatusFacts::default();
        assert_eq!(kept("layout type:cobol", &none).len(), PATHS.len());
        let long = format!("**/{}/*.rs", "a".repeat(MAX_GLOB_BYTES));
        assert_eq!(kept(&format!("layout {long}"), &none).len(), PATHS.len());
    }

    /// Git constraints are answered from the facts a caller collected, and
    /// each of the four means a different question.
    #[test]
    fn git_constraints_read_the_status_a_caller_collected() {
        let status = StatusFacts {
            modified: BTreeSet::from(["crates/rift-canopy/src/layout.rs".to_owned()]),
            untracked: BTreeSet::from(["notes/scratch.md".to_owned()]),
            staged: BTreeSet::from(["Cargo.toml".to_owned()]),
        };
        assert_eq!(
            kept("status:modified", &status),
            vec!["crates/rift-canopy/src/layout.rs"]
        );
        assert_eq!(kept("status:staged", &status), vec!["Cargo.toml"]);
        assert_eq!(kept("status:untracked", &status), vec!["notes/scratch.md"]);
        // The grammar spells "not touched" `status:clean`.
        assert_eq!(
            kept("status:clean", &status),
            vec![
                "crates/rift-canopy-data/src/bus.rs",
                "docs/guide/03-layout.md",
                "bins/rift/src/main.rs",
            ]
        );
    }

    /// A file-path constraint names a file, not a sequence of characters that
    /// happens to end one.
    #[test]
    fn a_path_suffix_matches_only_at_a_boundary() {
        assert!(ends_at_boundary("crates/rift/src/bus.rs", "src/bus.rs"));
        assert!(ends_at_boundary("src/bus.rs", "src/bus.rs"));
        assert!(!ends_at_boundary("crates/rift/src/nimbus.rs", "bus.rs"));
        assert!(!ends_at_boundary("bus.rs", "s.rs"));
    }

    /// Extensions are read from the last dot of the last segment, and a
    /// dotfile with no extension has none.
    #[test]
    fn an_extension_is_the_tail_of_the_file_name() {
        assert_eq!(extension("a/b/c.RS").as_deref(), Some("rs"));
        assert_eq!(extension("a.b/c").as_deref(), None);
        assert_eq!(extension(".gitignore").as_deref(), Some("gitignore"));
        assert_eq!(extension("a/b/c.").as_deref(), None);
    }

    /// The grammar has more shapes than the file-search configuration emits,
    /// and every one of them is a value this module can be handed: the
    /// parser's other configurations produce a multi-part text constraint, an
    /// exclusion list and a file-path suffix, and a constraint arriving from
    /// anywhere must mean here what its spelling promises. Each is answered
    /// against the path directly, since the queries this crate parses today
    /// spell the same questions with `Not` and `Text`.
    #[test]
    fn every_shape_the_grammar_can_produce_is_answered_here() {
        let none = StatusFacts::default();
        let both_words = Constraint::Parts(&["canopy", "layout"]);
        assert!(satisfies(
            &both_words,
            "crates/rift-canopy/src/layout.rs",
            &none
        ));
        assert!(
            !satisfies(&both_words, "crates/rift-canopy/src/bus.rs", &none),
            "every part has to be in the path, not just one"
        );

        let neither = Constraint::Exclude(&["docs", "notes"]);
        assert!(!satisfies(&neither, "docs/guide/03-layout.md", &none));
        assert!(!satisfies(&neither, "notes/scratch.md", &none));
        assert!(satisfies(&neither, "bins/rift/src/main.rs", &none));

        assert!(satisfies(
            &Constraint::FilePath("src/main.rs"),
            "bins/rift/src/main.rs",
            &none
        ));
        assert!(
            !satisfies(
                &Constraint::FilePath("main.rs"),
                "bins/rift/src/domain.rs",
                &none
            ),
            "a suffix constraint names a file, and `domain.rs` is not one"
        );

        assert!(
            satisfies(&Constraint::Text(""), "bins/rift/src/main.rs", &none),
            "an empty needle is no restriction rather than one nothing meets"
        );
    }

    /// The words a caller typed are what a content search looks for; the
    /// constraints that said which files to look in are not.
    #[test]
    fn the_content_literal_is_the_typed_text_without_the_constraints() {
        assert_eq!(literal_needle("read gate"), "read gate");
        assert_eq!(literal_needle("*.rs read gate"), "read gate");
        assert_eq!(literal_needle("hooks"), "hooks");
        // Constraints alone name a set of files and no text to find in them.
        assert_eq!(literal_needle("*.rs !tests/"), "");
        assert_eq!(literal_needle(""), "");
    }
}
