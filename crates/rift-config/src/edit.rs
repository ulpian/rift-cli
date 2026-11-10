//! Comment-preserving, conflict-aware edits of `.rift/config.toml`.

use std::path::{Path, PathBuf};

use toml_edit::{value, DocumentMut, Item, Table};

use crate::{EffectiveConfig, PolicyRefusal, PolicySource};

/// The source identity observed when a project policy was loaded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PolicyRevision {
    source_hash: Option<String>,
}

impl PolicyRevision {
    /// The BLAKE3 hash of the declared source, or `None` when it was absent.
    #[must_use]
    pub fn source_hash(&self) -> Option<&str> {
        self.source_hash.as_deref()
    }
}

/// A loaded policy together with the source revision required for a safe edit.
#[derive(Clone, Debug)]
pub struct ProjectPolicySnapshot {
    /// Validated effective policy.
    pub effective: EffectiveConfig,
    /// Exact declared-source revision.
    pub revision: PolicyRevision,
}

/// Fields the Project Settings surface is allowed to change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProjectPolicyPatch {
    /// Total Recall desired state.
    pub total_recall: Option<bool>,
    /// Capture native tool-call events.
    pub tool_calls: Option<bool>,
    /// Capture provider-reported token usage.
    pub token_usage: Option<bool>,
    /// Raw-transcript retrieval desired state.
    pub raw_transcripts: Option<bool>,
}

/// A policy write refusal. Messages name only repository-owned paths and
/// neutral reasons; policy bytes are never included.
#[derive(Debug, thiserror::Error)]
pub enum PolicyEditError {
    /// The policy path is not a repository-owned regular file.
    #[error("policy refused: {0}")]
    Refused(&'static str),
    /// The source changed after the caller loaded it.
    #[error("policy changed on disk; refresh Project Settings and retry")]
    SourceConflict,
    /// The existing or prospective document is invalid.
    #[error("policy is invalid: {0}")]
    Invalid(String),
    /// A filesystem operation failed.
    #[error("{path}: {source}")]
    Io {
        /// Repository-owned path involved.
        path: PathBuf,
        /// Underlying filesystem error.
        source: std::io::Error,
    },
}

/// Load the effective config and its exact source revision.
///
/// # Errors
/// Returns a sanitized invalid/I/O error. A refused symlink or special file is
/// represented by the effective config's [`PolicySource`] and can be displayed
/// read-only; it is not followed.
pub fn load_project_policy(repo: &Path) -> Result<ProjectPolicySnapshot, PolicyEditError> {
    let rift_dir = repo.join(".rift");
    let path = rift_dir.join("config.toml");
    let effective = if let Ok(effective) = EffectiveConfig::load(&rift_dir) {
        effective
    } else {
        // Rift historically scaffolded a comments-only file. It is a valid
        // sparse starting point for the editor even though the strict runtime
        // loader correctly rejects it until filled in.
        let bytes = read(&path)?;
        let sparse = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| text.parse::<DocumentMut>().ok())
            .is_some_and(|doc| doc.is_empty());
        if !sparse {
            return Err(PolicyEditError::Invalid(
                ".rift/config.toml is not valid TOML or policy".to_owned(),
            ));
        }
        EffectiveConfig::secure_default().map_err(|_| {
            PolicyEditError::Invalid("secure policy defaults did not validate".to_owned())
        })?
    };
    let source_hash = match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() => Some(hash_bytes(&read(&path)?)),
        _ => None,
    };
    Ok(ProjectPolicySnapshot {
        effective,
        revision: PolicyRevision { source_hash },
    })
}

/// Apply a Project Settings patch with optimistic source-hash concurrency,
/// same-directory atomic replacement, full prospective validation, and a
/// post-write effective re-read.
///
/// # Errors
/// Refuses symlinks/special files, invalid policy, source conflicts, and I/O.
pub fn patch_project_policy(
    repo: &Path,
    expected: &PolicyRevision,
    changes: ProjectPolicyPatch,
) -> Result<ProjectPolicySnapshot, PolicyEditError> {
    edit_document(repo, expected, |doc| apply_settings_patch(doc, changes))
}

/// Set (or clear, with `None`) `[checkpoint] remote` through the same
/// conflict-checked, symlink-refusing, prospectively-validated,
/// same-directory-atomic path every other policy edit takes.
///
/// This exists because the checkpoint remote used to be written by *replacing*
/// `.rift/config.toml` with a two-line document — which discarded the
/// project's declared policy and produced a file the strict loader then
/// refused. An edit that can silently delete a repository's redaction policy is
/// not an acceptable way to record where checkpoints are shipped.
///
/// # Errors
/// Refuses symlinks/special files, invalid policy, source conflicts, and I/O.
pub fn set_checkpoint_remote(
    repo: &Path,
    expected: &PolicyRevision,
    remote: Option<&str>,
) -> Result<ProjectPolicySnapshot, PolicyEditError> {
    edit_document(repo, expected, |doc| {
        let table = table_mut(doc, "checkpoint")?;
        match remote {
            Some(name) => set_str(table, "remote", name),
            None => {
                table.remove("remote");
            }
        }
        Ok(())
    })
}

/// The shared edit path: refuse an unowned policy file, check the caller's
/// revision still matches what is on disk, parse, scaffold, apply `change`,
/// validate the **prospective** document, replace atomically, and re-read.
///
/// Every public edit goes through here so that no future one can accidentally
/// skip the conflict check or the prospective validation.
fn edit_document(
    repo: &Path,
    expected: &PolicyRevision,
    change: impl FnOnce(&mut DocumentMut) -> Result<(), PolicyEditError>,
) -> Result<ProjectPolicySnapshot, PolicyEditError> {
    let rift_dir = repo.join(".rift");
    let path = rift_dir.join("config.toml");
    refuse_unowned_path(&rift_dir, &path)?;

    let current = read_optional(&path)?;
    let current_hash = current.as_deref().map(hash_bytes);
    if current_hash != expected.source_hash {
        return Err(PolicyEditError::SourceConflict);
    }

    let text = current
        .as_deref()
        .map(std::str::from_utf8)
        .transpose()
        .map_err(|_| PolicyEditError::Invalid("config.toml is not UTF-8".to_owned()))?
        .unwrap_or("");
    let mut doc = if text.trim().is_empty() {
        DocumentMut::new()
    } else {
        text.parse::<DocumentMut>()
            .map_err(|_| PolicyEditError::Invalid("config.toml is not valid TOML".to_owned()))?
    };
    ensure_scaffold(&mut doc, repo)?;
    change(&mut doc)?;

    let prospective = doc.to_string();
    EffectiveConfig::from_toml_str(&prospective)
        .map_err(|e| PolicyEditError::Invalid(e.to_string()))?;
    atomic_replace(&path, prospective.as_bytes())?;
    load_project_policy(repo)
}

fn apply_settings_patch(
    doc: &mut DocumentMut,
    changes: ProjectPolicyPatch,
) -> Result<(), PolicyEditError> {
    if let Some(enabled) = changes.total_recall {
        set_bool(table_mut(doc, "features")?, "total_recall", enabled);
    }
    if let Some(enabled) = changes.tool_calls {
        set_bool(table_mut(doc, "capture")?, "tool_calls", enabled);
    }
    if let Some(enabled) = changes.token_usage {
        set_bool(table_mut(doc, "capture")?, "token_usage", enabled);
    }
    if let Some(enabled) = changes.raw_transcripts {
        let capture = table_mut(doc, "capture")?;
        set_bool(capture, "raw_transcripts", enabled);
        // The acknowledgement is one state with the dangerous toggle: it can
        // never stay true after raw retrieval is switched off.
        set_bool(capture, "acknowledge_risk", enabled);
    }
    Ok(())
}

fn ensure_scaffold(doc: &mut DocumentMut, repo: &Path) -> Result<(), PolicyEditError> {
    if !doc.contains_key("schema_version") {
        doc.insert("schema_version", value(1));
    }
    let project = table_mut(doc, "project")?;
    if !project.contains_key("name") {
        let name = repo
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("unnamed");
        project.insert("name", value(name));
    }
    Ok(())
}

fn table_mut<'a>(doc: &'a mut DocumentMut, key: &str) -> Result<&'a mut Table, PolicyEditError> {
    if !doc.contains_key(key) {
        doc.insert(key, Item::Table(Table::new()));
    }
    doc.get_mut(key)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| PolicyEditError::Invalid(format!("[{key}] is not a table")))
}

fn set_str(table: &mut Table, key: &str, text: &str) {
    if let Some(current) = table.get_mut(key).and_then(Item::as_value_mut) {
        let decor = current.decor().clone();
        *current = toml_edit::Value::from(text);
        *current.decor_mut() = decor;
    } else {
        table.insert(key, value(text));
    }
}

fn set_bool(table: &mut Table, key: &str, enabled: bool) {
    if let Some(current) = table.get_mut(key).and_then(Item::as_value_mut) {
        let decor = current.decor().clone();
        *current = toml_edit::Value::from(enabled);
        *current.decor_mut() = decor;
    } else {
        table.insert(key, value(enabled));
    }
}

fn refuse_unowned_path(rift_dir: &Path, path: &Path) -> Result<(), PolicyEditError> {
    match std::fs::symlink_metadata(rift_dir) {
        Ok(meta) if !meta.is_dir() => {
            return Err(PolicyEditError::Refused(
                PolicySource::Refused(PolicyRefusal::RiftDirIsNotADirectory)
                    .diagnostic()
                    .unwrap_or(".rift is not a directory"),
            ));
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(PolicyEditError::Io {
                path: rift_dir.to_path_buf(),
                source: error,
            });
        }
        _ => {}
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_symlink() => Err(PolicyEditError::Refused(
            PolicySource::Refused(PolicyRefusal::ConfigIsASymlink)
                .diagnostic()
                .unwrap_or("config.toml is a symlink"),
        )),
        Ok(meta) if !meta.is_file() => Err(PolicyEditError::Refused(
            PolicySource::Refused(PolicyRefusal::ConfigIsNotARegularFile)
                .diagnostic()
                .unwrap_or("config.toml is not a regular file"),
        )),
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(PolicyEditError::Io {
            path: path.to_path_buf(),
            source: error,
        }),
        _ => Ok(()),
    }
}

fn read(path: &Path) -> Result<Vec<u8>, PolicyEditError> {
    std::fs::read(path).map_err(|source| PolicyEditError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, PolicyEditError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(PolicyEditError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), PolicyEditError> {
    let parent = path
        .parent()
        .ok_or_else(|| PolicyEditError::Invalid("config path has no parent".to_owned()))?;
    std::fs::create_dir_all(parent).map_err(|source| PolicyEditError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let tmp = parent.join(format!(".config.toml.rift-tmp-{}", std::process::id()));
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| PolicyEditError::Io { path, source }
    };
    // `create_new` refuses a file or link already at the temporary name, so
    // the bytes never follow a planted link, and the committed file is left
    // untouched whatever fails.
    if let Err(error) = write_new(&tmp, bytes) {
        // Something already at the temporary name is not this edit's to
        // remove; anything this edit created and could not finish is.
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            let _ = std::fs::remove_file(&tmp);
        }
        return Err(io(&tmp)(error));
    }
    // `rename` replaces the target in one step on every platform (Windows
    // included, through MOVEFILE_REPLACE_EXISTING), so a reader never finds
    // the file missing and falls back to secure defaults. A Windows reader
    // holding the file open can refuse it for a moment: try a few times.
    let mut last = None;
    for attempt in 0..RENAME_ATTEMPTS {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(error) => last = Some(error),
        }
        if attempt + 1 < RENAME_ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(io(path)(
        last.unwrap_or_else(|| std::io::Error::other("rename failed")),
    ))
}

/// How many times a refused replace is tried before the edit gives up.
const RENAME_ATTEMPTS: u32 = 4;

/// Write `bytes` to a file that must not already exist, flushed to disk.
fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".rift")).unwrap();
        dir
    }

    #[test]
    fn patch_preserves_comments_order_and_unrelated_keys() {
        let dir = repo();
        let before = "# lead\nschema_version = 1\n\n[project]\nname = \"demo\" # keep\n\n[capture]\ntoken_usage = true\n# tail\ntool_calls = true\n";
        std::fs::write(dir.path().join(".rift/config.toml"), before).unwrap();
        let loaded = load_project_policy(dir.path()).unwrap();
        patch_project_policy(
            dir.path(),
            &loaded.revision,
            ProjectPolicyPatch {
                tool_calls: Some(false),
                ..ProjectPolicyPatch::default()
            },
        )
        .unwrap();
        let after = std::fs::read_to_string(dir.path().join(".rift/config.toml")).unwrap();
        assert!(after.starts_with("# lead\n"));
        assert!(after.contains("name = \"demo\" # keep"));
        assert!(after.contains("# tail"));
        assert!(after.contains("tool_calls = false"));
        assert!(after.find("token_usage").unwrap() < after.find("tool_calls").unwrap());
    }

    #[test]
    fn stale_source_hash_refuses_without_writing() {
        let dir = repo();
        let path = dir.path().join(".rift/config.toml");
        std::fs::write(&path, "schema_version=1\n[project]\nname='a'\n").unwrap();
        let loaded = load_project_policy(dir.path()).unwrap();
        let external = "schema_version=1\n[project]\nname='external'\n";
        std::fs::write(&path, external).unwrap();
        let err = patch_project_policy(
            dir.path(),
            &loaded.revision,
            ProjectPolicyPatch {
                token_usage: Some(false),
                ..ProjectPolicyPatch::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, PolicyEditError::SourceConflict));
        assert_eq!(std::fs::read_to_string(path).unwrap(), external);
    }

    #[test]
    fn raw_transcripts_and_acknowledgement_move_together() {
        let dir = repo();
        let loaded = load_project_policy(dir.path()).unwrap();
        let on = patch_project_policy(
            dir.path(),
            &loaded.revision,
            ProjectPolicyPatch {
                raw_transcripts: Some(true),
                ..ProjectPolicyPatch::default()
            },
        )
        .unwrap();
        assert!(on.effective.raw_transcripts_enabled());
        let off = patch_project_policy(
            dir.path(),
            &on.revision,
            ProjectPolicyPatch {
                raw_transcripts: Some(false),
                ..ProjectPolicyPatch::default()
            },
        )
        .unwrap();
        assert!(!off.effective.raw_transcripts_enabled());
        let source = std::fs::read_to_string(dir.path().join(".rift/config.toml")).unwrap();
        assert!(source.contains("acknowledge_risk = false"));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_policy_is_refused_and_target_is_untouched() {
        use std::os::unix::fs::symlink;
        let dir = repo();
        let target = dir.path().join("outside.toml");
        let bytes = "schema_version=1\n[project]\nname='outside'\n";
        std::fs::write(&target, bytes).unwrap();
        symlink(&target, dir.path().join(".rift/config.toml")).unwrap();
        let err = patch_project_policy(
            dir.path(),
            &PolicyRevision::default(),
            ProjectPolicyPatch {
                total_recall: Some(true),
                ..ProjectPolicyPatch::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, PolicyEditError::Refused(_)));
        assert_eq!(std::fs::read_to_string(target).unwrap(), bytes);
    }

    #[test]
    fn a_replace_overwrites_in_one_step_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "old").unwrap();
        atomic_replace(&path, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["config.toml"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_planted_at_the_temporary_name_is_refused_and_the_config_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "kept").unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, "untouched").unwrap();
        let tmp = dir
            .path()
            .join(format!(".config.toml.rift-tmp-{}", std::process::id()));
        std::os::unix::fs::symlink(&outside, &tmp).unwrap();
        assert!(atomic_replace(&path, b"new").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "kept");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "untouched");
        assert!(
            std::fs::symlink_metadata(&tmp).is_ok(),
            "what this edit did not create is not this edit's to remove"
        );
    }
}
