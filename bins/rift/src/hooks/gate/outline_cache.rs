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

/// The family's own directory inside the git common dir.
const FAMILY_DIR: &str = "rift-family";

/// The cache's subdirectory under it.
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
    common_dir.join(FAMILY_DIR).join(OUTLINE_DIR)
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
}
