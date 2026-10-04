//! Editing the serve-policy file in place, the one way Project Settings
//! changes it: load, refuse a link or an invalid source, check the revision
//! the caller read, validate the prospective text as a policy, and replace the
//! file atomically in its own directory. Its own module because it is the
//! crate's only writer.

use std::path::{Path, PathBuf};

use crate::serve_policy::{owned_policy_bytes, parse_policy, OwnedPolicy};
use crate::{load_serve_policy, Mode, ServePolicyRevision, ServePolicySnapshot, ServePolicySource};

/// A safe serve-policy write refusal.
#[derive(Debug, thiserror::Error)]
pub enum ServePolicyEditError {
    /// The path is a symlink/special file.
    #[error("serve policy is refused; repair .rift/policies/serve.toml and retry")]
    Refused,
    /// The existing policy is invalid.
    #[error("serve policy is invalid; repair .rift/policies/serve.toml and retry")]
    Invalid,
    /// Source changed since the view was loaded.
    #[error("serve policy changed on disk; refresh Project Settings and retry")]
    SourceConflict,
    /// Filesystem failure.
    #[error("{path}: {source}")]
    Io {
        /// Repository-owned path involved.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
}

/// The lever tables Project Settings switches as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeverTable {
    /// `[search]`: bounding and answering searches.
    Search,
    /// `[doc]`: reading documents as Markdown.
    Doc,
}

impl LeverTable {
    /// The table's name in the policy file.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Doc => "doc",
        }
    }
}

/// Patch only the binary Project Settings injection control. `0` is off and
/// `400` is on; callers should pass
/// [`DEFAULT_INJECTION_TURN_BUDGET`](crate::DEFAULT_INJECTION_TURN_BUDGET) for on.
/// Existing custom positive budgets remain untouched until the operator flips
/// the binary control.
///
/// # Errors
/// Refuses links/special files, invalid sources, revision conflicts, and I/O.
pub fn patch_injection_turn_budget(
    rift_dir: &Path,
    expected: &ServePolicyRevision,
    budget: u32,
) -> Result<ServePolicySnapshot, ServePolicyEditError> {
    edit_serve_policy(rift_dir, expected, |doc| {
        let budget = toml_edit::Value::from(i64::from(budget));
        set_keeping_decor(doc.as_table_mut(), "injection_turn_budget", budget);
    })
}

/// Patch only a lever table's `mode`, the binary Project Settings control
/// over it: `enforce` is on (the default the table ships with) and `off` is
/// off. Every other key in the table — the bounds, the channel, the caps —
/// stays exactly as written, an absent table is created carrying only its
/// `mode`, and an explicit `observe` an operator declared stays until the
/// operator flips the control.
///
/// # Errors
/// Refuses links/special files, invalid sources, revision conflicts, and I/O.
pub fn patch_lever_mode(
    rift_dir: &Path,
    expected: &ServePolicyRevision,
    table: LeverTable,
    mode: Mode,
) -> Result<ServePolicySnapshot, ServePolicyEditError> {
    edit_serve_policy(rift_dir, expected, |doc| {
        let Some(entry) = doc
            .entry(table.as_str())
            .or_insert_with(toml_edit::table)
            .as_table_like_mut()
        else {
            // A `[search]`/`[doc]` that is not a table did not load as a
            // policy in the first place, and the prospective parse below
            // refuses the edit rather than this arm inventing one.
            return;
        };
        set_keeping_decor(entry, "mode", toml_edit::Value::from(mode.as_str()));
    })
}

/// The one way the serving policy is edited in place: load, refuse what is
/// not an owned valid file, check the revision the caller read, apply `edit`
/// to the parsed document, validate the prospective text as a policy, and
/// replace the file atomically. Every patch above is one closure over this.
fn edit_serve_policy(
    rift_dir: &Path,
    expected: &ServePolicyRevision,
    edit: impl FnOnce(&mut toml_edit::DocumentMut),
) -> Result<ServePolicySnapshot, ServePolicyEditError> {
    let path = rift_dir.join("policies").join("serve.toml");
    let current = load_serve_policy(rift_dir);
    match current.source {
        ServePolicySource::Refused => return Err(ServePolicyEditError::Refused),
        ServePolicySource::Invalid => return Err(ServePolicyEditError::Invalid),
        ServePolicySource::Declared | ServePolicySource::Default => {}
    }
    if &current.revision != expected {
        return Err(ServePolicyEditError::SourceConflict);
    }
    let text = match owned_policy_bytes(rift_dir, &path) {
        OwnedPolicy::Bytes(bytes) => {
            String::from_utf8(bytes).map_err(|_| ServePolicyEditError::Invalid)?
        }
        OwnedPolicy::Absent => String::new(),
        OwnedPolicy::Refused => return Err(ServePolicyEditError::Refused),
    };
    let mut doc = if text.trim().is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        text.parse::<toml_edit::DocumentMut>()
            .map_err(|_| ServePolicyEditError::Invalid)?
    };
    edit(&mut doc);
    let prospective = doc.to_string();
    parse_policy(&prospective).ok_or(ServePolicyEditError::Invalid)?;
    atomic_policy_replace(&path, prospective.as_bytes())?;
    Ok(load_serve_policy(rift_dir))
}

fn atomic_policy_replace(path: &Path, bytes: &[u8]) -> Result<(), ServePolicyEditError> {
    let Some(parent) = path.parent() else {
        return Err(ServePolicyEditError::Invalid);
    };
    std::fs::create_dir_all(parent).map_err(|source| ServePolicyEditError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let tmp = parent.join(format!(".serve.toml.rift-tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|source| ServePolicyEditError::Io {
        path: tmp.clone(),
        source,
    })?;
    #[cfg(windows)]
    if path.exists() {
        std::fs::remove_file(path).map_err(|source| ServePolicyEditError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }
    std::fs::rename(&tmp, path).map_err(|source| ServePolicyEditError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Write `value` under `key`, keeping the comment and spacing written around
/// the value it replaces: a switched control must not strip the operator's
/// notes from the file. An absent key is inserted plain.
fn set_keeping_decor(table: &mut dyn toml_edit::TableLike, key: &str, value: toml_edit::Value) {
    match table.get_mut(key).and_then(toml_edit::Item::as_value_mut) {
        Some(current) => {
            let decor = current.decor().clone();
            *current = value;
            *current.decor_mut() = decor;
        }
        None => {
            table.insert(key, toml_edit::Item::Value(value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{resolve, FamilyCapability, DEFAULT_INJECTION_TURN_BUDGET, DEFAULT_TABLE_ROWS};

    #[test]
    fn binary_injection_patch_preserves_comments_and_resets_to_400() {
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        let path = pol.join("serve.toml");
        std::fs::write(
            &path,
            "# keep\ndenied_paths = [\"private/**\"]\ninjection_turn_budget = 777 # custom\n",
        )
        .unwrap();
        let loaded = load_serve_policy(dir.path());
        assert_eq!(loaded.policy.injection_turn_budget, 777);
        let off = patch_injection_turn_budget(dir.path(), &loaded.revision, 0).unwrap();
        assert_eq!(off.policy.injection_turn_budget, 0);
        let on =
            patch_injection_turn_budget(dir.path(), &off.revision, DEFAULT_INJECTION_TURN_BUDGET)
                .unwrap();
        assert_eq!(on.policy.injection_turn_budget, 400);
        let source = std::fs::read_to_string(path).unwrap();
        assert!(source.starts_with("# keep\n"));
        assert!(source.contains("denied_paths = [\"private/**\"]"));
        assert!(source.contains("injection_turn_budget = 400 # custom"));
    }

    #[test]
    fn injection_patch_refuses_a_stale_revision() {
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        let path = pol.join("serve.toml");
        std::fs::write(&path, "injection_turn_budget = 400\n").unwrap();
        let loaded = load_serve_policy(dir.path());
        std::fs::write(&path, "injection_turn_budget = 12\n").unwrap();
        let error = patch_injection_turn_budget(dir.path(), &loaded.revision, 0).unwrap_err();
        assert!(matches!(error, ServePolicyEditError::SourceConflict));
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "injection_turn_budget = 12\n"
        );
    }

    /// The binary control over a lever table touches its `mode` and nothing
    /// else: the bounds beside it and the comments around it survive, an
    /// absent table is created with only its `mode`, and switching back on
    /// writes the default the table ships with.
    #[test]
    fn lever_mode_patch_creates_an_absent_table_and_keeps_the_rest_of_a_present_one() {
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        let path = pol.join("serve.toml");
        std::fs::write(
            &path,
            "# keep\ninjection_turn_budget = 400\n\n[search]\nmode = \"enforce\" # lever\n\
             grep_head_limit = 12\n",
        )
        .unwrap();
        let loaded = load_serve_policy(dir.path());
        assert_eq!(loaded.policy.search.grep_head_limit, 12);

        let search_off =
            patch_lever_mode(dir.path(), &loaded.revision, LeverTable::Search, Mode::Off).unwrap();
        assert_eq!(search_off.policy.search.mode, Mode::Off);
        assert_eq!(search_off.policy.search.grep_head_limit, 12);
        assert_eq!(
            search_off.policy.doc.mode,
            Mode::Enforce,
            "the other table is untouched"
        );

        let doc_off =
            patch_lever_mode(dir.path(), &search_off.revision, LeverTable::Doc, Mode::Off).unwrap();
        assert_eq!(doc_off.policy.doc.mode, Mode::Off);
        assert_eq!(doc_off.policy.doc.table_rows, DEFAULT_TABLE_ROWS);
        assert_eq!(doc_off.policy.injection_turn_budget, 400);

        let doc_on = patch_lever_mode(
            dir.path(),
            &doc_off.revision,
            LeverTable::Doc,
            Mode::Enforce,
        )
        .unwrap();
        assert_eq!(doc_on.policy.doc.mode, Mode::Enforce);

        let source = std::fs::read_to_string(path).unwrap();
        assert!(source.starts_with("# keep\n"), "{source}");
        assert!(source.contains("mode = \"off\" # lever"), "{source}");
        assert!(source.contains("grep_head_limit = 12"), "{source}");
        assert!(source.contains("[doc]\nmode = \"enforce\""), "{source}");
    }

    /// `off` written by the control is `off` for every family: the levers a
    /// repository switched off resolve to nothing, whatever the family's
    /// channels could carry.
    #[test]
    fn a_lever_switched_off_resolves_off_for_every_family() {
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        let loaded = load_serve_policy(dir.path());
        let snapshot =
            patch_lever_mode(dir.path(), &loaded.revision, LeverTable::Doc, Mode::Off).unwrap();
        let snapshot = patch_lever_mode(
            dir.path(),
            &snapshot.revision,
            LeverTable::Search,
            Mode::Off,
        )
        .unwrap();
        let everything = FamilyCapability {
            confirmed_reason_channel: true,
            document_sidecar_rewrite: true,
            grep_head_limit_rewrite: true,
            ..FamilyCapability::default()
        };
        let effective = resolve(&snapshot.policy, "claude-code", everything);
        assert_eq!(effective.doc_rewrite, Mode::Off);
        assert_eq!(effective.doc_redirect, Mode::Off);
        assert_eq!(effective.search_answer, Mode::Off);
        assert_eq!(effective.grep_cap, Mode::Off);
    }

    #[test]
    fn lever_mode_patch_refuses_a_stale_revision() {
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        let path = pol.join("serve.toml");
        std::fs::write(&path, "[doc]\nmode = \"enforce\"\n").unwrap();
        let loaded = load_serve_policy(dir.path());
        std::fs::write(&path, "[doc]\nmode = \"observe\"\n").unwrap();
        let error =
            patch_lever_mode(dir.path(), &loaded.revision, LeverTable::Doc, Mode::Off).unwrap_err();
        assert!(matches!(error, ServePolicyEditError::SourceConflict));
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "[doc]\nmode = \"observe\"\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_serve_policy_is_refused() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        let target = dir.path().join("outside.toml");
        std::fs::write(&target, "injection_turn_budget = 9\n").unwrap();
        symlink(&target, pol.join("serve.toml")).unwrap();
        let loaded = load_serve_policy(dir.path());
        assert_eq!(loaded.source, ServePolicySource::Refused);
        assert!(loaded.policy.fail_closed);
        let error = patch_injection_turn_budget(dir.path(), &loaded.revision, 0).unwrap_err();
        assert!(matches!(error, ServePolicyEditError::Refused));
        assert_eq!(
            std::fs::read_to_string(target).unwrap(),
            "injection_turn_budget = 9\n"
        );
    }

    /// The binary injection control cannot smuggle a policy past the range
    /// check: an unreadable file is repaired, never patched.
    #[test]
    fn the_injection_patch_refuses_a_policy_outside_its_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        let path = pol.join("serve.toml");
        std::fs::write(&path, "[doc]\nttl_days = 0\n").unwrap();
        let loaded = load_serve_policy(dir.path());
        assert_eq!(loaded.source, ServePolicySource::Invalid);
        let error = patch_injection_turn_budget(dir.path(), &loaded.revision, 0).unwrap_err();
        assert!(matches!(error, ServePolicyEditError::Invalid));
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "[doc]\nttl_days = 0\n",
            "a refused patch writes nothing"
        );
    }

    /// A staging file that cannot be written fails the edit with the path it
    /// tried, and the policy on disk is left as it was.
    #[test]
    fn an_edit_whose_staging_file_cannot_be_written_reports_it_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        let staging = pol.join(format!(".serve.toml.rift-tmp-{}", std::process::id()));
        std::fs::create_dir_all(&staging).unwrap();
        let loaded = load_serve_policy(dir.path());
        assert_eq!(loaded.source, ServePolicySource::Default);
        let error = patch_injection_turn_budget(dir.path(), &loaded.revision, 0).unwrap_err();
        match error {
            ServePolicyEditError::Io { path, .. } => assert_eq!(path, staging),
            other => panic!("expected an I/O refusal, got {other:?}"),
        }
        assert!(!pol.join("serve.toml").exists());
    }

    #[test]
    fn the_atomic_replace_reports_the_target_when_the_rename_cannot_land() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("policies").join("serve.toml");
        std::fs::create_dir_all(target.join("occupied")).unwrap();
        let error = atomic_policy_replace(&target, b"injection_turn_budget = 0\n").unwrap_err();
        match error {
            ServePolicyEditError::Io { path, .. } => assert_eq!(path, target),
            other => panic!("expected an I/O refusal, got {other:?}"),
        }
        assert!(target.join("occupied").is_dir(), "the target is untouched");
    }

    #[test]
    fn the_atomic_replace_reports_the_directory_it_could_not_create() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("checkout");
        std::fs::write(&file, "a file, not a directory").unwrap();
        let parent = file.join("policies");
        let error = atomic_policy_replace(&parent.join("serve.toml"), b"").unwrap_err();
        match error {
            ServePolicyEditError::Io { path, .. } => assert_eq!(path, parent),
            other => panic!("expected an I/O refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_atomic_replace_refuses_a_path_with_no_directory() {
        let error = atomic_policy_replace(Path::new(""), b"").unwrap_err();
        assert!(matches!(error, ServePolicyEditError::Invalid));
    }
}
