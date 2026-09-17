//! Measuring what a **search** put in front of the model.
//!
//! The savings denominator has always been read-shaped calls: `read_tokens`
//! joins `tool_reads_file`, so only a call that resolved to a file could ever
//! be in it. A search returns repository content too — on the owner's replay,
//! 324 calls and 237,978 tokens, 18.7 % of the whole pool — and none of it was
//! ever measured, which makes every published share the share of a pool
//! smaller than the one the levers act on.
//!
//! A search has no `tool_reads_file` link and often no `tool_call` row, so
//! there is nothing to hang a count on and nothing to count *from*: the hook
//! sees the call, not its result. What it does see is enough to reproduce the
//! call, so that is what is journaled — **identifiers only**, the same rule
//! every other job here lives by — and the detached worker runs the search
//! again, read-only, and counts what comes back with the real tokenizer.
//!
//! Four rules hold it honest.
//!
//! 1. **In process wherever the answer can be matched in process.** A glob is
//!    matched against the path index Rift already keeps and a literal content
//!    search against the same content matcher the pull verb uses, so nothing
//!    is executed at all. What is executed is what cannot be matched here: a
//!    regular expression, a recursive listing, a `find` with predicates.
//! 2. **Allow-listed verbs, argv, no shell.** [`ALLOWED_VERBS`] and nothing
//!    else, spawned with the arguments as a vector — no shell, so no pipe, no
//!    redirect, no substitution and no glob expansion. A command line that
//!    needs any of those is left **unmeasured**, which is a row of its own
//!    rather than a silence: a call nobody measured is a fact, and a figure
//!    nobody measured is not.
//! 3. **Keyed by the tree the search ran against.** The measurement is only
//!    the call's measurement while the tree is the tree the call saw, so the
//!    job carries the path index's own key — `(HEAD oid, newest directory
//!    mtime)`, read from the saved index's header for the cost of one small
//!    read — and the worker discards the count if it has moved. It brackets
//!    its own run with the same key as well, so a tree that moves *under* the
//!    re-execution is caught even where the job carried no key to start from.
//! 4. **Nothing here is an avoided figure.** A search that Rift bounded or
//!    answered books what it always booked
//!    (`docs/guide/08-cost-and-savings.md` §8.6); this is the denominator
//!    only. A measured search makes the published share **smaller**, which is
//!    the direction that tells you it is not being chosen to flatter.
//!
//! What a re-execution is not: it is not the tool's own output. `rg` run here
//! prints what `rg` prints, which is what the model received for the shell
//! channel and a close reconstruction for a tool that wraps it; a glob matched
//! in process is Rift's rendering of the paths, one per line, which is the
//! same shape [`rift_recall_serve::SearchCeiling`] already prices an answered
//! listing at. The two are told apart by the row's `basis` column rather than
//! averaged together.

use std::path::Path;

/// The command words a measurement may execute, and nothing else.
///
/// Read-only tools that print what they found, all four of them already in
/// the read grammar's vocabulary
/// (`docs/guide/07-serving-and-injection.md` §7.4). The list is closed on
/// purpose: it is a list of programs this tool will spawn, so it grows by
/// decision and never by inference from what an agent happened to type.
pub(crate) const ALLOWED_VERBS: [&str; 4] = ["grep", "rg", "find", "ls"];

/// The longest command line a measurement will look at, matching the read
/// grammar's own bound.
const MAX_COMMAND_BYTES: usize = 4096;

/// The most words one executed command may carry. A bound on the work rather
/// than an opinion: a search a person wrote is short.
const MAX_WORDS: usize = 64;

/// How long a re-executed search is given before it is killed and the call is
/// recorded as unmeasured.
///
/// Nobody is waiting on it, so the budget is not about latency; it is the
/// fail-safe for a search that will not finish — a `find /` an agent wrote
/// against a mount that has gone away.
pub(crate) const RUN_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);

/// The most output bytes a measurement reads back. Past it the search is
/// recorded as unmeasured rather than counted over a prefix nobody received:
/// every client cuts a command's result somewhere, and cutting it *here* would
/// put this module's bound into a figure that is supposed to be the tool's.
const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

/// A search as identifiers, journaled for the worker to run again.
///
/// Every variant is a *description of a call*, never its result: nothing of
/// what the search found is written to the journal, exactly as no capture
/// bytes are.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "how", rename_all = "kebab-case")]
pub(crate) enum MeasuredSearch {
    /// Run these words, in this order, with no shell.
    Argv {
        /// The command word and its arguments. The first is checked against
        /// [`ALLOWED_VERBS`] again at execution time, so a journal an older or
        /// hostile writer left cannot widen the list.
        words: Vec<String>,
    },
    /// Match a glob against the path index this member keeps.
    Glob {
        /// The repository-relative directory the search was scoped to; empty
        /// for the whole tree.
        dir: String,
        /// The pattern, as the agent wrote it.
        pattern: String,
    },
    /// Match a literal inside the files the index holds.
    Content {
        /// The literal.
        needle: String,
        /// Whether ASCII case was ignored.
        ignore_case: bool,
    },
    /// Recognised as a search over repository content, and not reproducible
    /// here. Recorded so the call is counted, with no tokens attached.
    Unmeasured,
}

impl MeasuredSearch {
    /// The `basis` token a successful measurement of this shape records.
    pub(crate) const fn basis(&self) -> &'static str {
        match self {
            Self::Argv { .. } => "reexecuted",
            Self::Glob { .. } | Self::Content { .. } => "in_process",
            Self::Unmeasured => "unmeasured",
        }
    }
}

/// Split a command line into words the way a POSIX shell would, and **refuse**
/// anything a shell would do more than split.
///
/// The refusal is the whole value. A measurement executes its words directly,
/// so a line carrying a pipe, a redirect, a substitution, a glob, a variable
/// or a separator would be run as literal text and measure something the agent
/// never ran. Rather than try to reproduce a shell, this returns `None` and
/// the call is recorded as unmeasured.
///
/// Quoting is honoured — a metacharacter *inside* quotes is text, which is how
/// `grep -E "a|b"` and `find . -name "*.rs"` reach the tool that understands
/// them — and an unterminated quote is a line this cannot read.
pub(crate) fn words_of(command: &str) -> Option<Vec<String>> {
    if command.len() > MAX_COMMAND_BYTES {
        return None;
    }
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut has_word = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            // A backslash inside double quotes, or outside any, escapes the
            // next character — which is the one way a metacharacter may be
            // literal without quotes around it.
            (None | Some('"'), '\\') => match chars.next() {
                Some(next) => {
                    word.push(next);
                    has_word = true;
                }
                None => return None,
            },
            (Some(_), _) => {
                word.push(c);
                has_word = true;
            }
            (None, '\'' | '"') => {
                quote = Some(c);
                has_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if has_word {
                    words.push(std::mem::take(&mut word));
                    has_word = false;
                }
            }
            // Everything a shell would *interpret*. Enumerated rather than
            // allow-listed because the set is small, closed and published by
            // POSIX, and because the failure direction of a miss here is a
            // call left unmeasured rather than a wrong number.
            (None, c) if "|&;<>()$`*?[]{}!~#\n\r".contains(c) => return None,
            (None, c) => {
                word.push(c);
                has_word = true;
            }
        }
        if words.len() > MAX_WORDS {
            return None;
        }
    }
    if quote.is_some() {
        return None;
    }
    if has_word {
        words.push(word);
    }
    (!words.is_empty() && words.len() <= MAX_WORDS).then_some(words)
}

/// Whether `words` is a command this module may execute: an allow-listed verb,
/// and no argument that is an option this module cannot reason about.
///
/// The verb is matched on the bare name, so a path to the program (`/bin/grep`)
/// is not one: what is allow-listed is the program this machine resolves the
/// name to, and a named path is a different program.
pub(crate) fn is_allowed(words: &[String]) -> bool {
    words
        .first()
        .is_some_and(|verb| ALLOWED_VERBS.contains(&verb.as_str()))
}

/// The classification of one shell command line for measurement.
///
/// `None` means this is not a search Rift can see: not an allow-listed verb at
/// all, which is most of what an agent runs. `Some(Unmeasured)` means it *is*
/// one and cannot be reproduced — a pipe, a glob, a substitution — which is a
/// fact worth a row.
pub(crate) fn classify_command(command: &str) -> Option<MeasuredSearch> {
    let head = command
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    if !ALLOWED_VERBS.contains(&head.as_str()) {
        return None;
    }
    match words_of(command) {
        Some(words) if is_allowed(&words) => Some(MeasuredSearch::Argv { words }),
        _ => Some(MeasuredSearch::Unmeasured),
    }
}

/// Whether `pattern` is a plain literal — no regular-expression syntax — and
/// so can be matched by the in-process content matcher, which is a literal
/// matcher by design (`rift_recall_search::content`).
pub(crate) fn is_literal(pattern: &str) -> bool {
    !pattern.is_empty() && !pattern.chars().any(|c| r"\^$.|?*+()[]{}".contains(c))
}

/// The classification of one **tool-shaped** search, from the arguments the
/// family's pinned row names.
///
/// `kind` is the row's own `kind` fact (`glob`, `grep`, `listing`); the
/// arguments are read by the names the row pins and by no name inferred here.
pub(crate) fn classify_tool(
    kind: &str,
    args: &serde_json::Map<String, serde_json::Value>,
    pattern_field: &str,
    dir_field: &str,
) -> Option<MeasuredSearch> {
    let text = |field: &str| {
        args.get(field)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    match kind {
        // A name search: the index answers it exactly, so nothing runs.
        "glob" | "listing" => {
            let pattern = text(pattern_field);
            if pattern.is_empty() {
                // A listing with no pattern is every path under the
                // directory, which is what an empty pattern means to the
                // matcher below.
                return Some(MeasuredSearch::Glob {
                    dir: text(dir_field),
                    pattern: String::new(),
                });
            }
            Some(MeasuredSearch::Glob {
                dir: text(dir_field),
                pattern,
            })
        }
        // A content search. A literal is matched in process; a regular
        // expression is the one shape that has to be executed, because the
        // in-process matcher is deliberately not a regex engine.
        "grep" => {
            let pattern = text(pattern_field);
            if pattern.is_empty() {
                return Some(MeasuredSearch::Unmeasured);
            }
            let ignore_case = args
                .get("-i")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if is_literal(&pattern) {
                return Some(MeasuredSearch::Content {
                    needle: pattern,
                    ignore_case,
                });
            }
            let mut words = vec!["rg".to_owned(), "--no-heading".to_owned()];
            if ignore_case {
                words.push("-i".to_owned());
            }
            words.push("--".to_owned());
            words.push(pattern);
            let path = text("path");
            if !path.is_empty() {
                words.push(path);
            }
            Some(MeasuredSearch::Argv { words })
        }
        _ => None,
    }
}

// ------------------------------------------------------- the worker's half ---

/// Run `search` against `work_tree`, read-only, and return the text a caller
/// counts.
///
/// `None` is "not measured", and every way of failing lands there: a verb the
/// allow-list does not carry (checked again here, so a journal cannot widen
/// it), a program this machine does not have, a non-zero exit that is not the
/// ordinary "no matches", output past the ceiling, output that is not text.
///
/// Read-only by construction: the four programs read, the in-process arms
/// open only indexed files, and nothing here writes anywhere.
pub(crate) fn run(work_tree: &Path, search: &MeasuredSearch) -> Option<String> {
    match search {
        MeasuredSearch::Unmeasured => None,
        MeasuredSearch::Argv { words } => run_argv(work_tree, words),
        MeasuredSearch::Glob { dir, pattern } => run_glob(work_tree, dir, pattern),
        MeasuredSearch::Content {
            needle,
            ignore_case,
        } => run_content(work_tree, needle, *ignore_case),
    }
}

/// Spawn one allow-listed program with these exact arguments and no shell.
fn run_argv(work_tree: &Path, words: &[String]) -> Option<String> {
    if !is_allowed(words) || words.len() > MAX_WORDS {
        return None;
    }
    let (verb, args) = words.split_first()?;
    let mut child = std::process::Command::new(verb)
        .args(args)
        .current_dir(work_tree)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let out = wait_bounded(&mut child)?;
    // `grep` and `rg` exit 1 for "no matches", which is a real answer and an
    // empty one. Anything past that is a call that did not run the way the
    // agent's did, and a count over its output would be a count of an error.
    if !matches!(out.status.code(), Some(0 | 1)) {
        return None;
    }
    (out.stdout.len() <= MAX_OUTPUT_BYTES)
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Wait for `child` within [`RUN_BUDGET`], killing it past that.
fn wait_bounded(child: &mut std::process::Child) -> Option<std::process::Output> {
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => return None,
        }
        if started.elapsed() > RUN_BUDGET {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // The child has exited; `wait_with_output` collects the pipe it left.
    let stdout = child.stdout.take();
    let status = child.try_wait().ok().flatten()?;
    let mut buffer = Vec::new();
    if let Some(mut pipe) = stdout {
        use std::io::Read as _;
        let _ = std::io::Read::by_ref(&mut pipe)
            .take(MAX_OUTPUT_BYTES as u64 + 1)
            .read_to_end(&mut buffer);
    }
    Some(std::process::Output {
        status,
        stdout: buffer,
        stderr: Vec::new(),
    })
}

/// Match a glob against this member's path index, and render the paths the way
/// a listing puts them in front of a model: one per line, cut at the ceiling
/// the same shape's clients publish.
#[cfg(feature = "search")]
fn run_glob(work_tree: &Path, dir: &str, pattern: &str) -> Option<String> {
    let index =
        rift_recall_search::PathIndex::saved_key(&rift_recall_search::index_path(work_tree))
            .and_then(|key| {
                rift_recall_search::PathIndex::load(
                    &rift_recall_search::index_path(work_tree),
                    &key,
                )
            })?;
    // The same matcher the answered-listing lever uses, over the same index:
    // one rule for "is this path one of this search's matches", so a figure
    // and an answer cannot disagree about what the search found.
    let mut out = String::new();
    let mut named = 0usize;
    for entry in index.entries() {
        let Some(rest) = under_scope(dir, &entry.path) else {
            continue;
        };
        if !pattern.is_empty() && !rift_config::glob_match(pattern, rest) {
            continue;
        }
        out.push_str(&entry.path);
        out.push('\n');
        named += 1;
        if named >= rift_recall_serve::NATIVE_LISTING_PATHS {
            break;
        }
    }
    Some(out)
}

/// A path relative to the directory a search was scoped to, or `None` when it
/// is not under it at all. Spelled as the answered-listing lever spells it.
#[cfg(feature = "search")]
fn under_scope<'a>(dir: &str, path: &'a str) -> Option<&'a str> {
    if dir.is_empty() {
        return Some(path);
    }
    path.strip_prefix(dir)?.strip_prefix('/')
}

/// A build with no search seam matches nothing in process, so the call is
/// recorded as unmeasured rather than as an empty answer.
#[cfg(not(feature = "search"))]
fn run_glob(_work_tree: &Path, _dir: &str, _pattern: &str) -> Option<String> {
    None
}

/// Match a literal inside the files the index holds, rendered the way a
/// content search puts its hits in front of a model.
#[cfg(feature = "search")]
fn run_content(work_tree: &Path, needle: &str, ignore_case: bool) -> Option<String> {
    let index =
        rift_recall_search::PathIndex::saved_key(&rift_recall_search::index_path(work_tree))
            .and_then(|key| {
                rift_recall_search::PathIndex::load(
                    &rift_recall_search::index_path(work_tree),
                    &key,
                )
            })?;
    let hits = rift_recall_search::content::search(
        work_tree,
        &rift_recall_search::ContentQuery {
            needle: needle.to_owned(),
            ignore_case,
            paths: index.entries().iter().map(|e| e.path.clone()).collect(),
            ..rift_recall_search::ContentQuery::default()
        },
    );
    let mut out = String::new();
    for hit in &hits {
        out.push_str(&format!("{}:{}:{}\n", hit.path, hit.line_number, hit.line));
    }
    Some(out)
}

#[cfg(not(feature = "search"))]
fn run_content(_work_tree: &Path, _needle: &str, _ignore_case: bool) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quoted_metacharacter_is_text_and_a_bare_one_ends_the_line() {
        assert_eq!(
            words_of(r#"grep -E "a|b" src"#),
            Some(vec![
                "grep".to_owned(),
                "-E".to_owned(),
                "a|b".to_owned(),
                "src".to_owned()
            ]),
            "the pipe inside quotes is the pattern, not a shell pipe"
        );
        assert_eq!(
            words_of(r#"find . -name "*.rs""#),
            Some(vec![
                "find".to_owned(),
                ".".to_owned(),
                "-name".to_owned(),
                "*.rs".to_owned()
            ])
        );
        for hostile in [
            "grep x src | head -5",
            "rg x > out.txt",
            "ls $HOME",
            "ls *.rs",
            "grep x `whoami`",
            "rg x && rm -rf .",
            "grep 'unterminated",
        ] {
            assert_eq!(words_of(hostile), None, "{hostile}");
        }
    }

    /// A line the shell would interpret is a search left unmeasured, which is
    /// a row; a line that is not a search at all is nothing.
    #[test]
    fn only_an_allow_listed_verb_is_a_search_at_all() {
        assert!(matches!(
            classify_command("rg --files src"),
            Some(MeasuredSearch::Argv { .. })
        ));
        assert_eq!(
            classify_command("grep -rn x src | head -5"),
            Some(MeasuredSearch::Unmeasured),
            "an allow-listed verb in a composition nothing can run is counted"
        );
        assert_eq!(classify_command("cargo test"), None);
        assert_eq!(classify_command("git log --oneline"), None);
        assert_eq!(classify_command(""), None);
        // A named path is a different program from the name this list carries.
        assert_eq!(classify_command("/usr/bin/grep x src"), None);
    }

    /// The re-executed form is never widened by the journal: the check that
    /// let the words in is asked again at the moment they would be spawned.
    #[test]
    fn a_journal_cannot_widen_the_allow_list() {
        let tmp = tempfile::tempdir().unwrap();
        let forged = MeasuredSearch::Argv {
            words: vec!["cmd".to_owned(), "/c".to_owned(), "dir".to_owned()],
        };
        assert_eq!(run(tmp.path(), &forged), None);
        assert_eq!(run(tmp.path(), &MeasuredSearch::Unmeasured), None);
    }

    /// A literal goes to the in-process matcher and a regular expression to
    /// the one program that understands it — one decision, so a pattern can
    /// never be matched as one shape and priced as the other.
    #[test]
    fn a_literal_is_matched_in_process_and_a_regex_is_executed() {
        let args = |pattern: &str| {
            let mut m = serde_json::Map::new();
            m.insert("pattern".to_owned(), serde_json::json!(pattern));
            m
        };
        assert_eq!(
            classify_tool("grep", &args("TurnBudget"), "pattern", "path"),
            Some(MeasuredSearch::Content {
                needle: "TurnBudget".to_owned(),
                ignore_case: false
            })
        );
        let regex = classify_tool("grep", &args("fn +\\w+"), "pattern", "path");
        assert!(
            matches!(regex, Some(MeasuredSearch::Argv { .. })),
            "{regex:?}"
        );
        assert_eq!(
            regex.as_ref().map(MeasuredSearch::basis),
            Some("reexecuted")
        );
        assert_eq!(
            classify_tool("glob", &args("**/*.rs"), "pattern", "path")
                .as_ref()
                .map(MeasuredSearch::basis),
            Some("in_process"),
            "a glob never runs a program"
        );
        assert_eq!(classify_tool("edit", &args("x"), "pattern", "path"), None);
        assert!(is_literal("TurnBudget"));
        assert!(!is_literal("fn +"));
        assert!(!is_literal(""));
    }
}
