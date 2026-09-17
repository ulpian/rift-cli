//! Matching lines inside files, as a primitive and nothing more.
//!
//! This is the half of search that reads bytes, so it is the half with the
//! rules:
//!
//! - **A literal, never a pattern language.** The needle is a substring, matched
//!   forwards, with an optional ASCII case fold. There is no regular expression
//!   engine here and there is nothing to backtrack, so a query cannot be turned
//!   into a denial of service against the process that answers it.
//! - **Indexed files only, which means tracked and not sensitive.** The
//!   candidates come from the path index, which already excludes what git
//!   ignores and what policy calls sensitive; untracked files are dropped on top
//!   of that, so a stray download in the work tree is not searchable and cannot
//!   put its contents in front of a model.
//! - **Budgeted before it is read, not after.** A file over the byte cap is
//!   skipped without being opened, the number of files is capped, the number of
//!   hits is capped, and each returned line is truncated. The caller sets all
//!   four, because what "affordable" means belongs to the caller's budget and
//!   not to this module.
//! - **A primitive, not an answer.** It returns lines. Whether any of them reach
//!   a model, and through which chokepoint they are neutralised on the way, is
//!   the serving layer's decision — file content is untrusted input, and nothing
//!   here pretends otherwise.

use std::path::Path;

use fff_grep::{LineTerminator, Match, Matcher, NoError, SearcherBuilder, Sink, SinkMatch};

/// How much of a file is read before deciding it is not text.
const BINARY_SNIFF_BYTES: usize = 8_192;

/// One request for matching lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentQuery {
    /// The literal to look for.
    pub needle: String,
    /// Whether ASCII case is ignored.
    pub ignore_case: bool,
    /// The paths to look in, repository-relative. Ordinarily the ranked answer
    /// to the same query, so the lines come back in the order the paths did.
    pub paths: Vec<String>,
    /// The most lines to return, over all files.
    pub max_hits: usize,
    /// The most files to open.
    pub max_files: usize,
    /// The largest file to read, in bytes.
    pub max_file_bytes: u64,
    /// The longest line to return, in bytes; longer ones are cut.
    pub max_line_bytes: usize,
}

impl Default for ContentQuery {
    /// Budgets a hook path could afford: forty lines, two thousand files, a
    /// megabyte apiece, and lines short enough that forty of them stay a
    /// glance rather than a file.
    fn default() -> Self {
        Self {
            needle: String::new(),
            ignore_case: false,
            paths: Vec::new(),
            max_hits: 40,
            max_files: 2_000,
            max_file_bytes: 1_024 * 1_024,
            max_line_bytes: 512,
        }
    }
}

/// One matching line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentHit {
    /// The repository-relative path the line is in.
    pub path: String,
    /// Its one-based line number.
    pub line_number: u64,
    /// The line, without its terminator, cut at the query's line cap.
    pub line: String,
}

/// Search `paths` under `root` for the query's literal.
///
/// Never fails: a file that has been deleted since the index was taken, one
/// that cannot be read, one that is binary, one over the cap — each is skipped.
/// Fewer lines is the safe direction; a line from a file nobody may search is
/// not.
#[must_use]
pub fn search(root: &Path, query: &ContentQuery) -> Vec<ContentHit> {
    if query.needle.is_empty() || query.max_hits == 0 {
        return Vec::new();
    }
    let matcher = Literal::new(&query.needle, query.ignore_case);
    let searcher = SearcherBuilder::new().line_number(true).build();
    let mut hits = Vec::new();
    for path in query.paths.iter().take(query.max_files) {
        if hits.len() >= query.max_hits {
            break;
        }
        let absolute = root.join(path);
        if std::fs::metadata(&absolute).is_ok_and(|m| m.len() > query.max_file_bytes) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&absolute) else {
            continue;
        };
        if is_binary(&bytes) {
            continue;
        }
        let mut sink = Collector {
            path,
            hits: &mut hits,
            max_hits: query.max_hits,
            max_line_bytes: query.max_line_bytes,
        };
        // The searcher's own errors are this sink's, and this sink has none:
        // it collects into memory. A search that somehow stopped early has
        // still collected what it found, which is why the result is ignored
        // rather than propagated.
        let _ = searcher.search_slice(&matcher, &bytes, &mut sink);
    }
    hits
}

/// Whether these bytes are something other than text.
///
/// A NUL byte early in the file, which is what every tool of this kind uses:
/// it is cheap, and being wrong in either direction costs a file that is
/// searched or skipped, never a line attributed to the wrong place.
fn is_binary(bytes: &[u8]) -> bool {
    bytes
        .get(..BINARY_SNIFF_BYTES.min(bytes.len()))
        .is_some_and(|head| head.contains(&0))
}

/// The literal matcher.
///
/// Case folding is ASCII-only and deliberate: a Unicode fold would need a table
/// and a definition of equality that varies by locale, and the needle here is
/// an identifier or a fragment of code far more often than it is prose.
struct Literal {
    needle: Vec<u8>,
    ignore_case: bool,
}

impl Literal {
    fn new(needle: &str, ignore_case: bool) -> Self {
        let bytes = needle.as_bytes();
        Self {
            needle: if ignore_case {
                bytes.to_ascii_lowercase()
            } else {
                bytes.to_vec()
            },
            ignore_case,
        }
    }

    fn equals(&self, window: &[u8]) -> bool {
        if self.ignore_case {
            window.eq_ignore_ascii_case(&self.needle)
        } else {
            window == self.needle.as_slice()
        }
    }
}

impl Matcher for Literal {
    type Error = NoError;

    fn find_at(&self, haystack: &[u8], at: usize) -> Result<Option<Match>, NoError> {
        if self.needle.is_empty() {
            return Ok(None);
        }
        let Some(tail) = haystack.get(at..) else {
            return Ok(None);
        };
        Ok(tail
            .windows(self.needle.len())
            .position(|window| self.equals(window))
            .map(|offset| Match::new(at + offset, at + offset + self.needle.len())))
    }

    fn line_terminator(&self) -> Option<LineTerminator> {
        // Telling the searcher that a match never spans a line is what keeps
        // it in its line-at-a-time mode; a needle that does contain a newline
        // says nothing, and the searcher handles it the slow, general way.
        (!self.needle.contains(&b'\n')).then(|| LineTerminator::byte(b'\n'))
    }
}

/// Collects matching lines, stopping at the query's cap.
struct Collector<'a> {
    path: &'a str,
    hits: &'a mut Vec<ContentHit>,
    max_hits: usize,
    max_line_bytes: usize,
}

impl Sink for Collector<'_> {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &fff_grep::Searcher,
        mat: &SinkMatch<'_>,
    ) -> std::io::Result<bool> {
        if self.hits.len() >= self.max_hits {
            return Ok(false);
        }
        let bytes = mat.bytes();
        let trimmed = bytes
            .get(..bytes.len().min(self.max_line_bytes))
            .unwrap_or_default();
        self.hits.push(ContentHit {
            path: self.path.to_owned(),
            line_number: mat.line_number().unwrap_or_default(),
            line: String::from_utf8_lossy(trimmed).trim_end().to_owned(),
        });
        Ok(self.hits.len() < self.max_hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> (tempfile::TempDir, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/bus.rs"),
            "use std::sync::mpsc;\nfn spawn_worker() {}\n// spawn_worker again\n",
        )
        .unwrap();
        std::fs::write(root.join("src/ui.rs"), "fn draw() {}\nSPAWN_WORKER\n").unwrap();
        std::fs::write(root.join("src/blob.bin"), b"spawn_worker\0\0binary\n").unwrap();
        let paths = vec![
            "src/bus.rs".to_owned(),
            "src/ui.rs".to_owned(),
            "src/blob.bin".to_owned(),
            "src/gone.rs".to_owned(),
        ];
        (dir, paths)
    }

    fn query(needle: &str, paths: &[String]) -> ContentQuery {
        ContentQuery {
            needle: needle.to_owned(),
            paths: paths.to_vec(),
            ..ContentQuery::default()
        }
    }

    /// The lines come back with their file, their number and their text, in
    /// the order the paths were given, and a file that is not there is one
    /// file skipped rather than an error.
    #[test]
    fn matching_lines_come_back_with_their_place() {
        let (dir, paths) = tree();
        let hits = search(dir.path(), &query("spawn_worker", &paths));
        assert_eq!(
            hits,
            vec![
                ContentHit {
                    path: "src/bus.rs".to_owned(),
                    line_number: 2,
                    line: "fn spawn_worker() {}".to_owned(),
                },
                ContentHit {
                    path: "src/bus.rs".to_owned(),
                    line_number: 3,
                    line: "// spawn_worker again".to_owned(),
                },
            ]
        );
    }

    /// Case folding is asked for, never assumed.
    #[test]
    fn case_is_respected_unless_the_caller_folds_it() {
        let (dir, paths) = tree();
        let folded = ContentQuery {
            ignore_case: true,
            ..query("SPAWN_worker", &paths)
        };
        let hits = search(dir.path(), &folded);
        assert_eq!(hits.len(), 3, "two in bus.rs and the shouted one in ui.rs");
        assert!(search(dir.path(), &query("SPAWN_worker", &paths)).is_empty());
    }

    /// A binary file is not searched, so its bytes cannot reach an answer.
    #[test]
    fn a_binary_file_is_never_searched() {
        let (dir, paths) = tree();
        let hits = search(dir.path(), &query("binary", &paths));
        assert!(hits.is_empty());
        assert!(is_binary(b"a\0b"));
        assert!(!is_binary(b"plain text\n"));
    }

    /// Every budget bites, and each of them bites before the bytes are read.
    #[test]
    fn every_budget_is_enforced() {
        let (dir, paths) = tree();
        let capped = ContentQuery {
            max_hits: 1,
            ..query("spawn_worker", &paths)
        };
        assert_eq!(search(dir.path(), &capped).len(), 1);

        let one_file = ContentQuery {
            max_files: 1,
            ignore_case: true,
            ..query("spawn_worker", &paths)
        };
        assert_eq!(search(dir.path(), &one_file).len(), 2, "bus.rs only");

        let tiny = ContentQuery {
            max_file_bytes: 4,
            ..query("spawn_worker", &paths)
        };
        assert!(search(dir.path(), &tiny).is_empty());

        let short = ContentQuery {
            max_line_bytes: 6,
            ..query("spawn_worker", &paths)
        };
        assert_eq!(search(dir.path(), &short)[0].line, "fn spa");

        assert!(search(dir.path(), &query("", &paths)).is_empty());
        let no_hits = ContentQuery {
            max_hits: 0,
            ..query("spawn_worker", &paths)
        };
        assert!(search(dir.path(), &no_hits).is_empty());
    }

    /// The matcher is a literal: the characters a pattern language would treat
    /// as syntax are characters.
    #[test]
    fn the_needle_is_a_literal_and_not_a_pattern() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a.c\nabc\n.*\n").unwrap();
        let paths = vec!["a.txt".to_owned()];
        let hits = search(dir.path(), &query("a.c", &paths));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, "a.c");
        assert_eq!(search(dir.path(), &query(".*", &paths))[0].line, ".*");
    }
}
