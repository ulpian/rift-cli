//! Explicit project-local permission edits. Existing allow/deny rules stay intact.

use crate::InstallError;
use std::path::{Path, PathBuf};

/// One supported native permission choice and its exact effect.
#[derive(Clone, Debug)]
pub struct PermissionOption {
    /// Stable choice token.
    pub token: &'static str,
    /// User-facing title.
    pub label: &'static str,
    /// Native settings changed, including limits on what the choice promises.
    pub detail: &'static str,
}

/// Only documented project-local controls are offered. No global file is edited.
#[must_use]
pub fn permission_options(agent: &str) -> Vec<PermissionOption> {
    let details = match agent {
        "codex" => [
            "Keep all current Codex permission settings unchanged.",
            ".codex/config.toml: sandbox_mode=read-only; approval_policy=on-request. Requires a trusted project; enforced policies still apply.",
            ".codex/config.toml: sandbox_mode=workspace-write; approval_policy=on-request. Existing network and additional writable-root rules remain in force.",
        ],
        "claude-code" => [
            "Keep all current Claude Code permission settings unchanged.",
            ".claude/settings.local.json: permissions.defaultMode=plan. Existing rules and managed policy still apply; this is the harness's Plan mode.",
            ".claude/settings.local.json: permissions.defaultMode=acceptEdits. Automatically accepts supported edits in working/additional directories; other rules remain in force.",
        ],
        _ => return Vec::new(),
    };
    ["keep", "restricted", "workspace"]
        .into_iter()
        .zip(["Keep current", "Restricted / Plan", "Workspace edits"])
        .zip(details)
        .map(|((token, label), detail)| PermissionOption {
            token,
            label,
            detail,
        })
        .collect()
}

fn settings_path(repo: &Path, agent: &str) -> Result<PathBuf, InstallError> {
    match agent {
        "codex" => Ok(repo.join(".codex/config.toml")),
        "claude-code" => Ok(repo.join(".claude/settings.local.json")),
        _ => Err(failure(
            repo,
            "this harness has no supported project-local permission preset",
        )),
    }
}

/// Hash of the exact file the user reviewed; absent files have a distinct revision.
///
/// # Errors
/// Refuses links, malformed files, and incompatible Codex permission profiles.
pub fn permission_revision(repo: &Path, agent: &str) -> Result<String, InstallError> {
    let path = settings_path(repo, agent)?;
    let bytes = read(&path)?;
    prospective(&path, agent, "keep", bytes.as_deref())?;
    Ok(revision(bytes.as_deref()))
}

/// Apply a previously reviewed choice without rewriting unrelated keys.
///
/// # Errors
/// Refuses stale previews, unknown presets, links, and invalid native configuration.
pub fn apply_permission(
    repo: &Path,
    agent: &str,
    preset: &str,
    expected: &str,
) -> Result<(), InstallError> {
    if preset == "keep" {
        return Ok(());
    }
    let path = settings_path(repo, agent)?;
    validate_inherited_permissions(agent, &path)?;
    apply_project_permission(repo, agent, preset, expected)
}

fn apply_project_permission(
    repo: &Path,
    agent: &str,
    preset: &str,
    expected: &str,
) -> Result<(), InstallError> {
    let path = settings_path(repo, agent)?;
    let before = read(&path)?;
    if revision(before.as_deref()) != expected {
        return Err(failure(
            &path,
            "permission settings changed since preview; reload and review again",
        ));
    }
    let bytes = prospective(&path, agent, preset, before.as_deref())?;
    // A second check immediately before the shared same-directory replacement.
    if read(&path)? != before {
        return Err(failure(
            &path,
            "permission settings changed while preparing the edit",
        ));
    }
    crate::atomic_write(&path, &bytes)
}

/// Validate a reviewed change before any member of a setup batch writes.
///
/// # Errors
/// Refuses stale previews, unknown presets, links, and invalid configuration.
pub fn validate_permission(
    repo: &Path,
    agent: &str,
    preset: &str,
    expected: &str,
) -> Result<(), InstallError> {
    if preset == "keep" {
        return Ok(());
    }
    let path = settings_path(repo, agent)?;
    validate_inherited_permissions(agent, &path)?;
    let before = read(&path)?;
    if revision(before.as_deref()) != expected {
        return Err(failure(
            &path,
            "permission settings changed since preview; reload and review again",
        ));
    }
    prospective(&path, agent, preset, before.as_deref())?;
    Ok(())
}

fn prospective(
    path: &Path,
    agent: &str,
    preset: &str,
    source: Option<&[u8]>,
) -> Result<Vec<u8>, InstallError> {
    if !permission_options(agent)
        .iter()
        .any(|option| option.token == preset)
    {
        return Err(failure(path, "unknown native permission preset"));
    }
    let text = std::str::from_utf8(source.unwrap_or_default())
        .map_err(|_| failure(path, "native settings are not UTF-8"))?;
    match agent {
        "codex" => codex_settings(path, text, preset),
        "claude-code" => claude_settings(path, text, preset),
        _ => Err(failure(path, "unsupported native permission settings")),
    }
}

fn codex_settings(path: &Path, text: &str, preset: &str) -> Result<Vec<u8>, InstallError> {
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| failure(path, "Codex settings are not valid TOML"))?;
    if preset == "keep" {
        return Ok(text.as_bytes().to_vec());
    }
    if has_permission_profile(&doc) {
        return Err(failure(
            path,
            "Codex permission profiles are configured; keep them and change permissions in Codex",
        ));
    }
    let sandbox = if preset == "restricted" {
        "read-only"
    } else {
        "workspace-write"
    };
    let table = doc.as_table_mut();
    table.insert("sandbox_mode", toml_edit::value(sandbox));
    table.insert("approval_policy", toml_edit::value("on-request"));
    Ok(doc.to_string().into_bytes())
}

fn has_permission_profile(doc: &toml_edit::DocumentMut) -> bool {
    ["default_permissions", "permissions", "profile", "profiles"]
        .iter()
        .any(|key| doc.contains_key(key))
}

fn validate_inherited_permissions(agent: &str, project: &Path) -> Result<(), InstallError> {
    if agent != "codex" {
        return Ok(());
    }
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(|home| PathBuf::from(home).join(".codex"))
        });
    let Some(home) = home else {
        return Err(failure(
            project,
            "cannot inspect inherited Codex permissions; keep current",
        ));
    };
    inherited_codex_settings(&home.join("config.toml"))
}

fn inherited_codex_settings(path: &Path) -> Result<(), InstallError> {
    let Some(bytes) = read(path)? else {
        return Ok(());
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| failure(path, "inherited Codex settings are not UTF-8"))?;
    let doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| failure(path, "inherited Codex settings are invalid"))?;
    if has_permission_profile(&doc) {
        return Err(failure(
            path,
            "inherited Codex permission/profile configuration needs review in Codex; keep current",
        ));
    }
    Ok(())
}

fn claude_settings(path: &Path, text: &str, preset: &str) -> Result<Vec<u8>, InstallError> {
    let mut value: serde_json::Value = if text.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(text)
            .map_err(|_| failure(path, "Claude settings are not valid JSON"))?
    };
    let object = value
        .as_object_mut()
        .ok_or_else(|| failure(path, "Claude settings must be a JSON object"))?;
    if preset == "keep" {
        return Ok(text.as_bytes().to_vec());
    }
    let permissions = object
        .entry("permissions")
        .or_insert_with(|| serde_json::json!({}));
    let permissions = permissions
        .as_object_mut()
        .ok_or_else(|| failure(path, "Claude permissions must be a JSON object"))?;
    permissions.insert(
        "defaultMode".into(),
        serde_json::json!(if preset == "restricted" {
            "plan"
        } else {
            "acceptEdits"
        }),
    );
    serde_json::to_vec_pretty(&value)
        .map_err(|_| failure(path, "could not encode native permission settings"))
}

fn read(path: &Path) -> Result<Option<Vec<u8>>, InstallError> {
    for component in path.parent().into_iter().chain(std::iter::once(path)) {
        match std::fs::symlink_metadata(component) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(failure(component, "refusing linked native settings"))
            }
            Ok(meta) if component == path && (!meta.is_file() || meta.len() > 1_048_576) => {
                return Err(failure(
                    component,
                    "native settings must be a regular file no larger than 1 MiB",
                ))
            }
            Ok(meta) if component != path && !meta.is_dir() => {
                return Err(failure(
                    component,
                    "native settings parent must be a directory",
                ))
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(InstallError::Io {
                    path: component.into(),
                    source,
                })
            }
        }
    }
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(InstallError::Io {
            path: path.into(),
            source,
        }),
    }
}

fn revision(bytes: Option<&[u8]>) -> String {
    bytes.map_or_else(
        || "absent".to_owned(),
        |bytes| blake3::hash(bytes).to_hex().to_string(),
    )
}

fn failure(path: &Path, reason: &str) -> InstallError {
    InstallError::Config {
        path: path.into(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_permission_preview_never_overwrites_new_settings() {
        let repo = tempfile::tempdir().unwrap();
        let rev = permission_revision(repo.path(), "codex").unwrap();
        std::fs::create_dir(repo.path().join(".codex")).unwrap();
        let path = repo.path().join(".codex/config.toml");
        std::fs::write(&path, "model = 'keep-me'\n").unwrap();
        assert!(apply_project_permission(repo.path(), "codex", "restricted", &rev).is_err());
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "model = 'keep-me'\n"
        );
    }

    #[test]
    fn permission_edits_preserve_comments_hooks_and_deny_rules() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".codex")).unwrap();
        std::fs::write(
            repo.path().join(".codex/config.toml"),
            "# keep this\nmodel = 'keep-me'\n",
        )
        .unwrap();
        let rev = permission_revision(repo.path(), "codex").unwrap();
        apply_project_permission(repo.path(), "codex", "restricted", &rev).unwrap();
        let text = std::fs::read_to_string(repo.path().join(".codex/config.toml")).unwrap();
        assert!(
            text.contains("# keep this") && text.contains("keep-me") && text.contains("read-only")
        );
        std::fs::create_dir(repo.path().join(".claude")).unwrap();
        let path = repo.path().join(".claude/settings.local.json");
        std::fs::write(
            &path,
            r#"{"hooks":{},"permissions":{"deny":["Bash(rm *)"]}}"#,
        )
        .unwrap();
        let rev = permission_revision(repo.path(), "claude-code").unwrap();
        apply_project_permission(repo.path(), "claude-code", "workspace", &rev).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(
            value["permissions"]["deny"],
            serde_json::json!(["Bash(rm *)"])
        );
        assert_eq!(value["hooks"], serde_json::json!({}));
    }
    #[test]
    fn inherited_codex_profiles_require_review_without_changing_global_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let source = "default_permissions = 'restricted'\n[permissions.restricted]\n";
        std::fs::write(&path, source).unwrap();
        assert!(inherited_codex_settings(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        std::fs::write(&path, "model = 'existing'\n").unwrap();
        assert!(inherited_codex_settings(&path).is_ok());
    }
}
