//! Where an agent keeps its own transcripts, and writing one back there.
//!
//! The convention is stated only where it is known — Claude Code today —
//! because a guessed location is worse than no answer: `attach` would adopt
//! the wrong file and `resume` would write beside the checkout.

use std::path::{Path, PathBuf};

/// The longest project directory name Claude Code keeps unshortened, in
/// UTF-16 code units (`i7e` in Claude Code 2.1.186).
const PROJECT_NAME_MAX_UNITS: usize = 200;

/// The agent's per-repo transcript DIRECTORY (the bulk-import source).
/// Stated only where the convention is known — claude-code today.
pub(super) fn default_transcript_dir(agent: &str, work_tree: &Path) -> Option<PathBuf> {
    let projects = claude_code_projects(agent)?;
    Some(transcript_dir_under(&projects, work_tree))
}

/// Write the stored redacted transcript bytes to the agent's native location.
pub(super) fn write_transcript(path: &Path, bytes: &[u8]) -> bool {
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    std::fs::write(path, bytes).is_ok()
}

/// The agent's own default transcript location, for the journal-free path
/// (fresh clone). Stated only where the convention is known; `None` is the
/// honest answer everywhere else — the caller says so instead of guessing.
pub(super) fn default_transcript_path(
    agent: &str,
    work_tree: &Path,
    session_id: &str,
) -> Option<PathBuf> {
    let projects = claude_code_projects(agent)?;
    Some(transcript_path_under(&projects, work_tree, session_id))
}

/// Claude Code's `<home>/.claude/projects`, for Claude Code only.
fn claude_code_projects(agent: &str) -> Option<PathBuf> {
    if agent != "claude-code" {
        return None;
    }
    Some(claude_code_home()?.join(".claude").join("projects"))
}

/// The first candidate transcript that exists and is non-empty (Claude Code
/// skips an empty one too), else the exact plain-spelling candidate — the
/// location `resume` writes to and a miss reports.
fn transcript_path_under(projects: &Path, work_tree: &Path, session_id: &str) -> PathBuf {
    let file = format!("{session_id}.jsonl");
    let found = candidate_dirs(projects, work_tree)
        .into_iter()
        .map(|dir| dir.join(&file))
        .find(|path| std::fs::metadata(path).is_ok_and(|meta| meta.len() > 0));
    match found {
        Some(path) => path,
        None => plain_project_dir(projects, work_tree).join(file),
    }
}

/// The first candidate directory that exists, else the exact plain one.
fn transcript_dir_under(projects: &Path, work_tree: &Path) -> PathBuf {
    let found = candidate_dirs(projects, work_tree)
        .into_iter()
        .find(|dir| dir.is_dir());
    match found {
        Some(dir) => dir,
        None => plain_project_dir(projects, work_tree),
    }
}

/// The exact project directory for the plain spelling of `work_tree` — the
/// one Rift has always named, never the verbatim `\\?\` form the family
/// plane resolves, so the name stays relative under `projects`.
fn plain_project_dir(projects: &Path, work_tree: &Path) -> PathBuf {
    let plain = rift_git::child_process_path(work_tree);
    projects.join(project_dir_name(&plain.to_string_lossy()))
}

/// Every directory Claude Code may have kept this work tree's transcripts
/// in, in lookup order. Claude Code names the directory after the realpath
/// of its cwd (falling back to the path as given); Rift holds the plain
/// spelling, so both are asked, plain first. For each spelling the exact
/// name comes first, then — for a shortened name only — any directory that
/// starts with the 200-unit prefix and `-`, which is how Claude Code itself
/// accepts a name another runtime hashed differently.
fn candidate_dirs(projects: &Path, work_tree: &Path) -> Vec<PathBuf> {
    let plain = rift_git::child_process_path(work_tree);
    let mut spellings = vec![plain.to_string_lossy().into_owned()];
    if let Ok(real) = std::fs::canonicalize(work_tree) {
        let real = rift_git::child_process_path(&real)
            .to_string_lossy()
            .into_owned();
        if !spellings.contains(&real) {
            spellings.push(real);
        }
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    for spelling in &spellings {
        let exact = projects.join(project_dir_name(spelling));
        let shortened = prefix_matches(projects, spelling, &exact);
        for dir in std::iter::once(exact).chain(shortened) {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    dirs
}

/// Directories under `projects` that share a shortened name's 200-unit
/// prefix and `-` but are not `exact`, sorted so the lookup is
/// deterministic. Empty for a name Claude Code never shortens.
fn prefix_matches(projects: &Path, spelling: &str, exact: &Path) -> Vec<PathBuf> {
    let folded = fold_project_path(spelling);
    let Some(prefix) = folded.get(..PROJECT_NAME_MAX_UNITS) else {
        return Vec::new();
    };
    if folded.len() == PROJECT_NAME_MAX_UNITS {
        return Vec::new();
    }
    let prefix = format!("{prefix}-");
    let Ok(entries) = std::fs::read_dir(projects) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .map(|entry| entry.path())
        .filter(|path| path != exact)
        .collect();
    found.sort();
    found
}

/// Claude Code's project directory name for `path`, as read from Claude
/// Code 2.1.186 (`DE`), not guessed: every UTF-16 code unit outside
/// `[A-Za-z0-9]` becomes `-`; a name over 200 units keeps its first 200,
/// then `-` and the base-36 absolute value of the path's string hash.
fn project_dir_name(path: &str) -> String {
    let folded = fold_project_path(path);
    match folded.get(..PROJECT_NAME_MAX_UNITS) {
        Some(prefix) if folded.len() > PROJECT_NAME_MAX_UNITS => {
            format!("{prefix}-{}", base36(java_string_hash(path).unsigned_abs()))
        }
        _ => folded,
    }
}

/// One `-` per UTF-16 code unit outside `[A-Za-z0-9]`, as JavaScript's
/// `replace(/[^a-zA-Z0-9]/g, "-")` does: a non-ASCII BMP char is one `-`,
/// an astral char two. The result is ASCII, so bytes count code units.
fn fold_project_path(path: &str) -> String {
    path.encode_utf16()
        .map(|unit| match u8::try_from(unit) {
            Ok(byte) if byte.is_ascii_alphanumeric() => char::from(byte),
            _ => '-',
        })
        .collect()
}

/// Claude Code's `kAe`: `h = (h << 5) - h + unit | 0` over the path's
/// UTF-16 code units — the Java string hash with i32 wrapping.
fn java_string_hash(path: &str) -> i32 {
    path.encode_utf16().fold(0_i32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(i32::from(unit))
    })
}

/// `Number.prototype.toString(36)` for a non-negative integer: lowercase.
fn base36(mut value: u32) -> String {
    let mut digits = Vec::new();
    loop {
        digits.extend(char::from_digit(value % 36, 36));
        value /= 36;
        if value == 0 {
            break;
        }
    }
    digits.iter().rev().collect()
}

/// Where Claude Code keeps `.claude`: `HOME` where it is set (every Unix
/// shell, and the redirected home a test hands the binary), otherwise the
/// Windows profile directory, which is what Claude Code itself reads there.
fn claude_code_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .or_else(|| {
            if cfg!(windows) {
                std::env::var_os("USERPROFILE")
            } else {
                None
            }
        })
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule stated independently of the production fold: every char
    /// outside ASCII letters and digits becomes one `-` per UTF-16 unit.
    fn claude_code_fold(path: &str) -> String {
        let mut out = String::new();
        for c in path.chars() {
            if c.is_ascii_alphanumeric() {
                out.push(c);
            } else {
                out.push_str(&"-".repeat(c.len_utf16()));
            }
        }
        out
    }

    /// A project path with `_`, a space and `~` keeps its transcripts where
    /// Claude Code folds every one of them to `-`, and is found there.
    #[test]
    fn a_project_path_with_underscore_space_and_tilde_finds_its_transcript() {
        let tmp = tempfile::tempdir().unwrap();
        let work_tree = tmp.path().join("my_proj ~1").join("repo");
        std::fs::create_dir_all(&work_tree).unwrap();
        let projects = tmp.path().join("home").join(".claude").join("projects");
        let dir = projects.join(claude_code_fold(&work_tree.to_string_lossy()));
        let transcript = dir.join("oob-1.jsonl");
        assert!(write_transcript(&transcript, b"{}\n"));

        assert_eq!(
            transcript_path_under(&projects, &work_tree, "oob-1"),
            transcript
        );
        assert_eq!(transcript_dir_under(&projects, &work_tree), dir);
    }

    /// The default transcript location is stated only where the convention is
    /// known. `None` everywhere else is the honest answer, not a fallback.
    #[test]
    fn only_the_agent_whose_convention_is_known_gets_a_default_transcript_path() {
        let work_tree = Path::new("/repos/my.project/sub");
        for agent in ["codex", "gemini", "copilot", "opencode", ""] {
            assert!(
                default_transcript_path(agent, work_tree, "sid").is_none(),
                "{agent} has no known transcript convention"
            );
            assert!(
                default_transcript_dir(agent, work_tree).is_none(),
                "{agent}"
            );
        }

        // Claude Code keys its per-project dir by the worktree path with `/`
        // and `.` folded to `-`; HOME wins, with USERPROFILE on Windows.
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .or_else(|| {
                cfg!(windows)
                    .then(|| std::env::var_os("USERPROFILE"))
                    .flatten()
            })
            .map(PathBuf::from);
        let path = default_transcript_path("claude-code", work_tree, "sid");
        assert_eq!(
            path.is_some(),
            home.is_some(),
            "the path is knowable exactly when a supported home is"
        );
        if let (Some(home), Some(path)) = (home, path) {
            assert_eq!(
                path,
                home.join(".claude")
                    .join("projects")
                    .join("-repos-my-project-sub")
                    .join("sid.jsonl")
            );
            assert_eq!(
                default_transcript_dir("claude-code", work_tree),
                Some(
                    home.join(".claude")
                        .join("projects")
                        .join("-repos-my-project-sub")
                ),
                "the dir is the path's parent, from the same convention"
            );
        }
    }

    /// The Windows convention, observed on a machine running Claude Code:
    /// `\` and the drive's `:` fold to `-` like `/` and `.`, and the
    /// verbatim spelling the family plane resolves is folded in its plain
    /// form. Folding only `/` and `.` left an absolute path, and `join`
    /// discarded the projects directory: `session attach` never found a
    /// Windows transcript, and `resume` on a fresh clone would have written
    /// one beside the checkout in `%TEMP%`.
    #[cfg(windows)]
    #[test]
    fn a_windows_work_tree_stays_under_the_projects_directory() {
        let home = claude_code_home().expect("HOME or USERPROFILE is set on Windows");
        let expected = home
            .join(".claude")
            .join("projects")
            .join("C--repos-my-project-sub")
            .join("sid.jsonl");
        for spelling in [r"C:\repos\my.project\sub", r"\\?\C:\repos\my.project\sub"] {
            let path = default_transcript_path("claude-code", Path::new(spelling), "sid")
                .expect("the convention is known");
            assert_eq!(path, expected, "{spelling}");
            assert!(
                path.starts_with(home.join(".claude").join("projects")),
                "the folded name is relative: {}",
                path.display()
            );
        }
    }

    /// The folding table, pinned: ASCII punctuation and spaces are one `-`
    /// each, a non-ASCII BMP char is one, an astral char is two.
    #[test]
    fn every_unit_outside_ascii_letters_and_digits_folds_to_one_dash() {
        assert_eq!(project_dir_name("/Users/a.b/c_d e~f"), "-Users-a-b-c-d-e-f");
        assert_eq!(project_dir_name("/r/caf\u{e9}/x"), "-r-caf--x");
        assert_eq!(project_dir_name("/r/\u{1F600}/x"), "-r----x");
        assert_eq!(project_dir_name("C:\\a+b@c"), "C--a-b-c");
    }

    /// Shortening, pinned to values Claude Code 2.1.186's own `DE` produced
    /// for these paths (run under node and bun from the binary's source):
    /// a 200-unit name is kept whole; one unit more is cut to 200 and given
    /// the base-36 absolute string hash, which was negative for the first
    /// two and positive for the non-ASCII one.
    #[test]
    fn a_name_over_two_hundred_units_is_cut_and_given_the_path_hash() {
        let exact = format!("/{}", "a".repeat(199));
        assert_eq!(project_dir_name(&exact), format!("-{}", "a".repeat(199)));

        let over = format!("/{}", "a".repeat(200));
        assert_eq!(java_string_hash(&over), -676_821_585);
        assert_eq!(
            project_dir_name(&over),
            format!("-{}-b6ymvl", "a".repeat(199))
        );

        let deep = format!("/Users/dev/{}repo", "deep/".repeat(40));
        assert_eq!(java_string_hash(&deep), -1_723_695_902);
        let name = project_dir_name(&deep);
        assert_eq!(name.len(), 207);
        assert!(name.ends_with("-deep-si8s9q"), "{name}");

        let accented = format!("/home/\u{fc}ser/{}proj", "nested_dir ".repeat(20));
        assert_eq!(java_string_hash(&accented), 1_361_551_783);
        assert!(project_dir_name(&accented).ends_with("ir-ne-mims2v"));

        assert_eq!(base36(i32::MIN.unsigned_abs()), "zik0zk");
    }

    /// A long path whose folder another runtime named with a different hash
    /// suffix is still found by its 200-unit prefix, as Claude Code finds it.
    #[test]
    fn a_shortened_folder_with_another_hash_suffix_is_found_by_its_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let work_tree = tmp.path().join("d".repeat(120)).join("e".repeat(120));
        let projects = tmp.path().join("projects");
        let folded = claude_code_fold(&work_tree.to_string_lossy());
        let prefix = folded.get(..200).unwrap();
        let dir = projects.join(format!("{prefix}-otherhash"));
        let transcript = dir.join("sid.jsonl");
        assert!(write_transcript(&transcript, b"{}\n"));
        std::fs::create_dir_all(projects.join(format!("{prefix}x-not-ours"))).unwrap();

        assert_eq!(
            transcript_path_under(&projects, &work_tree, "sid"),
            transcript
        );
        assert_eq!(transcript_dir_under(&projects, &work_tree), dir);
    }

    /// Claude Code names the folder after the realpath of its cwd; a work
    /// tree reached through a symlink still finds the transcript there, and
    /// an empty transcript is passed over as Claude Code passes it over.
    #[cfg(unix)]
    #[test]
    fn a_transcript_under_the_realpath_is_found_through_a_symlinked_spelling() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(real.join("repo")).unwrap();
        let alias = tmp.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let work_tree = alias.join("repo");
        let canonical = std::fs::canonicalize(&work_tree).unwrap();
        let projects = tmp.path().join("projects");
        let plain_dir = projects.join(claude_code_fold(&work_tree.to_string_lossy()));
        let real_dir = projects.join(claude_code_fold(&canonical.to_string_lossy()));
        assert!(write_transcript(&plain_dir.join("sid.jsonl"), b""));
        assert!(write_transcript(&real_dir.join("sid.jsonl"), b"{}\n"));

        assert_eq!(
            transcript_path_under(&projects, &work_tree, "sid"),
            real_dir.join("sid.jsonl")
        );
        assert_eq!(
            transcript_path_under(&projects, &work_tree, "missing"),
            plain_dir.join("missing.jsonl"),
            "a miss names the plain-spelling candidate"
        );
    }

    /// A transcript write reports failure instead of pretending: an
    /// unwritable parent is `false`, and a fresh nested path is created.
    #[test]
    fn a_transcript_write_reports_its_own_failure() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("t.jsonl");
        assert!(write_transcript(&nested, b"line\n"));
        assert_eq!(std::fs::read(&nested).unwrap(), b"line\n");

        // The parent exists as a FILE, so `create_dir_all` fails and the
        // write is refused rather than clobbering it.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"not a dir").unwrap();
        assert!(!write_transcript(&blocker.join("t.jsonl"), b"x"));
        assert_eq!(std::fs::read(&blocker).unwrap(), b"not a dir");
    }
}
