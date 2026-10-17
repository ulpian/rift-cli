//! The outline cache: one small file per bound revision, named by the hash
//! of its bytes, that tells the read gate a file's outline is bound and what
//! its first-contact frame may say.
//!
//! Separate because it is the one thing the decision path reads to answer a
//! first read, and the one thing the detached worker writes for it. A hook
//! opens no store and starts no tokenizer, so the outline and the token
//! counts the worker bound into the recall projection are copied here, keyed
//! by the content hash and the outline's versions: a file whose bytes move
//! names another key and simply misses, so a stale entry cannot be read. The
//! cache is run state under the family's own directory, rebuilt from the
//! projection by the next bind, and never the only holder of a fact.
//!
//! The grammar is closed. A header line, the path's class, the shape line and
//! one line per definition; every field but a definition's name is a number
//! or a word from a fixed set, and the name is the outline's own (an
//! identifier, or a heading already cut to the frame alphabet). The renderer
//! still judges every name before a frame quotes it.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use rift_recall_serve::DefinitionKind;

use crate::hooks::family_state::family_state_dir;

/// The cache's subdirectory of the family's run-state directory.
const OUTLINE_DIR: &str = "outline";

/// The first line of every cache file: the grammar and its version. A file
/// that opens any other way is a miss, never a guess.
const HEADER: &str = "rift-outline 1";

/// The largest cache file the gate reads, and the most a writer produces.
///
/// A ceiling on what a hook spends, not on correctness: an outline with more
/// definitions than fit keeps those that do, and the shape line still states
/// the full count.
pub(in crate::hooks) const MAX_OUTLINE_CACHE_BYTES: usize = 64 * 1024;

/// One bound outline as the cache holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::hooks) struct CachedOutline {
    /// Whether the outline is by heading rather than by definition.
    pub(in crate::hooks) markdown: bool,
    /// The file's line count.
    pub(in crate::hooks) lines: u32,
    /// The file's byte count.
    pub(in crate::hooks) bytes: u64,
    /// How many definitions the outline lists, whether or not each one is
    /// below.
    pub(in crate::hooks) definitions: u32,
    /// The whole file's stored token count, where one was stored.
    pub(in crate::hooks) whole_tokens: Option<u32>,
    /// The definitions, in file order.
    pub(in crate::hooks) entries: Vec<CachedDefinition>,
}

/// A field the cache grammar does not admit.
struct OutsideGrammar;

/// One definition of a cached outline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::hooks) struct CachedDefinition {
    /// Its kind, from the closed set.
    pub(in crate::hooks) kind: DefinitionKind,
    /// Its name.
    pub(in crate::hooks) name: String,
    /// The first line of its span.
    pub(in crate::hooks) first_line: u32,
    /// The last line of its span, inclusive.
    pub(in crate::hooks) last_line: u32,
    /// Its stored token count, where one was stored.
    pub(in crate::hooks) tokens: Option<u32>,
}

/// The cache directory of the family whose git common dir is `common_dir`.
///
/// The common dir, never a checkout: every member of the family reads the
/// outlines any member's read bound.
pub(in crate::hooks) fn outline_cache_dir(common_dir: &Path) -> PathBuf {
    family_state_dir(common_dir).join(OUTLINE_DIR)
}

/// The file one revision's outline is cached under: the content hash and
/// the outline's extractor, policy and redaction versions.
fn outline_cache_name(content_sha256: &str) -> String {
    let versions = rift_recall_serve::outline_versions(rift_recall_policy::SERVE_POLICY_VERSION);
    format!(
        "{content_sha256}-e{}p{}r{}.outline",
        versions.extractor_version, versions.policy_version, versions.redaction_version
    )
}

/// The cached outline of `rel_path` at `hash`, or `None` — a miss — when no
/// file holds one, the file is past its ceiling, or a line of it is not in
/// the grammar.
///
/// The decision path's one read for a first contact: the common dir from
/// the git dir's own pointer file, then one bounded file read. No store, no
/// tokenizer, no walk of the work tree.
pub(in crate::hooks) fn load_outline_cache(
    git_dir: &Path,
    rel_path: &str,
    hash: rift_core::Sha256Id,
) -> Option<CachedOutline> {
    let common_dir = rift_git::read_common_dir(git_dir);
    let file = outline_cache_dir(&common_dir).join(outline_cache_name(&hash.to_hex()));
    let meta = std::fs::symlink_metadata(&file).ok()?;
    if !meta.is_file() || meta.len() > MAX_OUTLINE_CACHE_BYTES as u64 {
        return None;
    }
    let text = std::fs::read_to_string(&file).ok()?;
    parse_outline_cache(&text, rift_recall_serve::is_markdown_path(rel_path))
}

/// Write `outline` as the cached outline of the revision `content_sha256`,
/// unless a file already holds it. Says whether a file was written.
///
/// The one writer, called wherever an outline is bound: the detached
/// worker's job after a read, and the catch-up at condense. Written to a
/// temporary name and renamed, so a reader finds a whole file or none.
///
/// # Errors
/// An I/O failure creating the directory or writing the file.
pub(in crate::hooks) fn write_outline_cache(
    common_dir: &Path,
    content_sha256: &str,
    outline: &CachedOutline,
) -> std::io::Result<bool> {
    let dir = outline_cache_dir(common_dir);
    let file = dir.join(outline_cache_name(content_sha256));
    if file.is_file() {
        return Ok(false);
    }
    std::fs::create_dir_all(&dir)?;
    let staged = dir.join(format!(".{content_sha256}.{}.tmp", std::process::id()));
    std::fs::write(&staged, render_outline_cache(outline))?;
    std::fs::rename(&staged, &file)?;
    Ok(true)
}

/// Remove the cache files older than `ttl_ms` by `now_ms`, and say how many
/// went (or would, on a dry run).
///
/// Age is the bound, and it is safe because every file is a copy: named by
/// the bytes' hash, written once, and rebuilt from the projection by the next
/// read of those bytes, so a file collected while still useful costs one
/// first contact answered by the read instead. Only names this module writes
/// are touched — a cache file or one of its staged temporaries — and only in
/// the outline directory; a file whose age cannot be read is kept.
pub(crate) fn sweep_outline_cache(
    common_dir: &Path,
    now_ms: i64,
    ttl_ms: i64,
    dry_run: bool,
) -> usize {
    let cutoff = now_ms.saturating_sub(ttl_ms);
    let Ok(entries) = std::fs::read_dir(outline_cache_dir(common_dir)) else {
        return 0;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_cache_file_name(&name) {
            continue;
        }
        let path = entry.path();
        let modified_ms = std::fs::symlink_metadata(&path)
            .ok()
            .filter(std::fs::Metadata::is_file)
            .and_then(|meta| meta.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_millis()).ok());
        let Some(modified_ms) = modified_ms else {
            continue;
        };
        if modified_ms >= cutoff {
            continue;
        }
        if dry_run || std::fs::remove_file(&path).is_ok() {
            removed = removed.saturating_add(1);
        }
    }
    removed
}

/// Whether a name is one this module writes: `<sha256>-e<E>p<P>r<R>.outline`,
/// or a staged `.<sha256>.<pid>.tmp`.
fn is_cache_file_name(name: &str) -> bool {
    let is_hex = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
    if let Some(staged) = name.strip_prefix('.').and_then(|n| n.strip_suffix(".tmp")) {
        return staged
            .split_once('.')
            .is_some_and(|(sha, pid)| is_hex(sha) && number::<u32>(pid).is_some());
    }
    let Some((sha, versions)) = name
        .strip_suffix(".outline")
        .and_then(|n| n.split_once('-'))
    else {
        return false;
    };
    let digits = versions.replace(['e', 'p', 'r'], "");
    is_hex(sha) && versions.starts_with('e') && number::<u64>(&digits).is_some()
}

/// The cache file's text: header, class, shape, then as many definitions as
/// fit under [`MAX_OUTLINE_CACHE_BYTES`].
pub(in crate::hooks) fn render_outline_cache(outline: &CachedOutline) -> String {
    let class = if outline.markdown { "markdown" } else { "code" };
    let mut out = format!(
        "{HEADER}\nclass {class}\nshape {} {} {} {}\n",
        outline.lines,
        outline.bytes,
        outline.definitions,
        count_field(outline.whole_tokens)
    );
    for entry in &outline.entries {
        let line = format!(
            "def {} {} {} {} {}\n",
            entry.kind.as_str(),
            entry.first_line,
            entry.last_line,
            count_field(entry.tokens),
            entry.name
        );
        if out.len().saturating_add(line.len()) > MAX_OUTLINE_CACHE_BYTES {
            break;
        }
        let _ = write!(out, "{line}");
    }
    out
}

/// A count as the grammar spells it: the number, or `-` when none was stored.
fn count_field(count: Option<u32>) -> String {
    count.map_or_else(|| "-".to_owned(), |n| n.to_string())
}

/// Read a cache file back, or `None` when any line is outside the grammar or
/// its class is not the path's.
pub(in crate::hooks) fn parse_outline_cache(text: &str, markdown: bool) -> Option<CachedOutline> {
    let mut lines = text.lines();
    if lines.next()? != HEADER {
        return None;
    }
    let class = lines.next()?.strip_prefix("class ")?;
    if class != if markdown { "markdown" } else { "code" } {
        return None;
    }
    let shape: Vec<&str> = lines.next()?.strip_prefix("shape ")?.split(' ').collect();
    let [lines_field, bytes_field, definitions_field, whole_field] = shape.as_slice() else {
        return None;
    };
    let entries = lines.map(parse_definition).collect::<Option<Vec<_>>>()?;
    Some(CachedOutline {
        markdown,
        lines: number(lines_field)?,
        bytes: number(bytes_field)?,
        definitions: number(definitions_field)?,
        whole_tokens: optional_count(whole_field).ok()?,
        entries,
    })
}

/// One `def` line: kind, first line, last line, count, then the name.
fn parse_definition(line: &str) -> Option<CachedDefinition> {
    let mut fields = line.strip_prefix("def ")?.splitn(5, ' ');
    let kind = DefinitionKind::from_label(fields.next()?)?;
    let first_line = number(fields.next()?)?;
    let last_line = number(fields.next()?)?;
    let tokens = optional_count(fields.next()?).ok()?;
    let name = fields.next()?;
    if name.is_empty() || name.chars().any(char::is_control) || first_line > last_line {
        return None;
    }
    Some(CachedDefinition {
        kind,
        name: name.to_owned(),
        first_line,
        last_line,
        tokens,
    })
}

/// A field of ASCII digits, parsed; anything else is outside the grammar.
fn number<T: std::str::FromStr>(field: &str) -> Option<T> {
    if field.is_empty() || !field.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    field.parse().ok()
}

/// A count field: `Ok(None)` for `-`, `Ok(Some(n))` for a number, and
/// `Err` for anything else.
fn optional_count(field: &str) -> Result<Option<u32>, OutsideGrammar> {
    if field == "-" {
        return Ok(None);
    }
    number(field).map(Some).ok_or(OutsideGrammar)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outline() -> CachedOutline {
        CachedOutline {
            markdown: false,
            lines: 637,
            bytes: 23_449,
            definitions: 3,
            whole_tokens: Some(5_704),
            entries: vec![
                CachedDefinition {
                    kind: DefinitionKind::Impl,
                    name: "fmt::Display for Thing".to_owned(),
                    first_line: 32,
                    last_line: 58,
                    tokens: Some(222),
                },
                CachedDefinition {
                    kind: DefinitionKind::Function,
                    name: "role_of".to_owned(),
                    first_line: 129,
                    last_line: 141,
                    tokens: None,
                },
            ],
        }
    }

    /// ★ One file per revision, named by its content hash and the outline's
    /// versions, in the family's common directory, written once.
    #[test]
    fn a_bound_outline_is_cached_once_under_its_content_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let common_dir = tmp.path();
        let sha = rift_agent::content_sha256(b"the bytes").to_hex();
        assert!(write_outline_cache(common_dir, &sha, &outline()).unwrap());
        assert!(!write_outline_cache(common_dir, &sha, &outline()).unwrap());
        let file = outline_cache_dir(common_dir).join(format!("{sha}-e3p1r1.outline"));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "rift-outline 1\nclass code\nshape 637 23449 3 5704\n\
             def impl 32 58 222 fmt::Display for Thing\ndef fn 129 141 - role_of\n"
        );
        let names: Vec<_> = std::fs::read_dir(outline_cache_dir(common_dir))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "no staged file is left behind: {names:?}");
    }

    /// A Markdown outline is never read for a code path, nor the reverse.
    #[test]
    fn a_cached_outline_of_the_other_class_is_a_miss() {
        let text = render_outline_cache(&outline());
        assert!(parse_outline_cache(&text, false).is_some());
        assert!(parse_outline_cache(&text, true).is_none());
    }

    /// Any line outside the grammar refuses the whole file.
    #[test]
    fn a_cache_file_outside_the_grammar_is_a_miss() {
        let good = render_outline_cache(&outline());
        for bad in [
            good.replace(HEADER, "rift-outline 2"),
            good.replace("shape 637", "shape six"),
            good.replace("def impl", "def ignore"),
            good.replace(" 129 141 ", " 141 129 "),
            format!("{good}def fn 1 2 3\n"),
            format!("{good}say fn 1 2 3 x\n"),
            good.replace("5704", "+5704"),
        ] {
            assert!(parse_outline_cache(&bad, false).is_none(), "{bad}");
        }
    }

    /// ★ What the worker writes is what the gate reads, under the content
    /// hash and the outline's versions, through the git dir's own pointer to
    /// the family's common directory.
    #[test]
    fn a_cached_outline_reads_back_exactly_under_its_content_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let hash = rift_agent::content_sha256(b"the bytes");
        write_outline_cache(&git_dir, &hash.to_hex(), &outline()).unwrap();
        assert_eq!(
            load_outline_cache(&git_dir, "src/a.rs", hash),
            Some(outline())
        );
        let other = rift_agent::content_sha256(b"other bytes");
        assert_eq!(load_outline_cache(&git_dir, "src/a.rs", other), None);
        assert_eq!(load_outline_cache(&git_dir, "docs/a.md", hash), None);
    }

    /// ★ The sweep collects cache files past their age by the injected clock,
    /// counts without removing on a dry run, and leaves a fresh file and a
    /// name it did not write alone.
    #[test]
    fn the_sweep_collects_old_cache_files_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let common_dir = tmp.path();
        let old_sha = "a".repeat(64);
        let fresh_sha = "b".repeat(64);
        write_outline_cache(common_dir, &old_sha, &outline()).unwrap();
        write_outline_cache(common_dir, &fresh_sha, &outline()).unwrap();
        let dir = outline_cache_dir(common_dir);
        let staged = dir.join(format!(".{}.123.tmp", "c".repeat(64)));
        let stranger = dir.join("notes.txt");
        let outside = common_dir.join(format!("{old_sha}-e3p1r1.outline"));
        for path in [&staged, &stranger, &outside] {
            std::fs::write(path, b"x").unwrap();
        }
        let (now, ttl) = (1_800_000_000_000_i64, 30 * 86_400_000_i64);
        let at = |path: &Path, ms: i64| {
            let when = std::time::UNIX_EPOCH
                + std::time::Duration::from_millis(u64::try_from(ms).unwrap());
            std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(when)
                .unwrap();
        };
        let old = dir.join(format!("{old_sha}-e3p1r1.outline"));
        let fresh = dir.join(format!("{fresh_sha}-e3p1r1.outline"));
        for path in [&old, &staged, &stranger, &outside] {
            at(path, now - ttl - 1);
        }
        at(&fresh, now - 1);
        assert_eq!(sweep_outline_cache(common_dir, now, ttl, true), 2);
        assert!(old.exists() && staged.exists(), "a dry run removes nothing");
        assert_eq!(sweep_outline_cache(common_dir, now, ttl, false), 2);
        assert!(!old.exists() && !staged.exists());
        assert!(fresh.exists(), "a fresh file stays");
        assert!(stranger.exists(), "a name this module did not write stays");
        assert!(
            outside.exists(),
            "nothing outside the outline directory is touched"
        );
    }

    /// A writer stops adding definitions at the ceiling, and the shape line
    /// still states the full count.
    #[test]
    fn a_cache_file_never_passes_its_ceiling() {
        let mut big = outline();
        big.entries = (1..=2_000)
            .map(|at| CachedDefinition {
                kind: DefinitionKind::Function,
                name: "x".repeat(90),
                first_line: at,
                last_line: at,
                tokens: Some(at),
            })
            .collect();
        big.definitions = 2_000;
        let text = render_outline_cache(&big);
        assert!(text.len() <= MAX_OUTLINE_CACHE_BYTES);
        assert!(text.contains("shape 637 23449 2000 5704\n"));
        let kept = text.lines().filter(|line| line.starts_with("def ")).count();
        assert!(kept > 0 && kept < 2_000, "{kept}");
    }

    /// The directory is the family's, under the common dir: the spelling the
    /// worker, the gate and the sweep have always met at.
    #[test]
    fn the_cache_lives_under_the_family_directory() {
        assert_eq!(
            outline_cache_dir(Path::new("/repo/.git")),
            Path::new("/repo/.git/rift-family/outline")
        );
    }
}
