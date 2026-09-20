//! Antigravity's tracked `.agents/hooks.json`: a reserved `rift-recall` bundle
//! beside the user's own, fingerprinted on its own and written only by the
//! explicit command.

use std::path::{Path, PathBuf};

use crate::{
    fingerprint_json, hook_fragment, merge_antigravity_hooks, remove_antigravity_hooks,
    rift_hook_binary, ClientFamily, InstallError,
};

use super::descriptor::descriptor_rift_bin;
use super::hooks::{descriptor_emits, PlannedHook};
use super::manifest::{read_manifest, OwnershipManifest};
use super::{NativeHookWiring, NativeWiring};

/// The reserved bundle's state: the manifest or the managed descriptor's own
/// binary prove it Rift's; a bundle Rift would emit for some other binary is
/// refreshable; anything else under the reserved key is a conflict.
pub(super) fn inspect_antigravity_hooks(
    repo: &Path,
    root: &serde_json::Map<String, serde_json::Value>,
    descriptor_wiring: NativeWiring,
) -> NativeHookWiring {
    let family = ClientFamily::Antigravity;
    let Some(current) = root.get("rift-recall") else {
        return NativeHookWiring::Missing;
    };
    if read_manifest(repo).owns_hook(family.token(), &fingerprint_json(current))
        || descriptor_emits(repo, family, descriptor_wiring, current)
    {
        return NativeHookWiring::Managed;
    }
    if recognizable_antigravity_hook_fragment(current) {
        NativeHookWiring::Stale
    } else {
        NativeHookWiring::Conflict
    }
}

/// Plan the merge of the reserved bundle. Once the manifest holds a record for
/// Antigravity that record is authoritative; before the first record only the
/// exact bundle about to be written is safely Rift's.
pub(super) fn prepare_antigravity_hook_enable(
    path: PathBuf,
    root: serde_json::Map<String, serde_json::Value>,
    rift_bin: &Path,
    manifest: &OwnershipManifest,
) -> Result<PlannedHook, InstallError> {
    let family = ClientFamily::Antigravity;
    let expected = hook_fragment(family, rift_bin);
    let previous = match root.get("rift-recall") {
        None => None,
        Some(current) => {
            let current_hash = fingerprint_json(current);
            let safely_owned = if manifest.hooks.contains_key(family.token()) {
                manifest.owns_hook(family.token(), &current_hash)
            } else {
                current == &expected
            };
            if !safely_owned {
                return Err(reserved_bundle_mismatch(path));
            }
            Some(current_hash)
        }
    };
    let bytes = merge_antigravity_hooks(&path, &expected, previous.as_deref(), false)?;
    Ok(PlannedHook {
        family,
        path,
        bytes,
        fragment_hash: fingerprint_json(&expected),
    })
}

/// Plan the removal of the reserved bundle: `None` when there is none, the
/// bytes without it when the manifest — or, with no record at all, the
/// descriptor's own binary — proves it Rift's.
pub(super) fn prepare_antigravity_hook_disable(
    repo: &Path,
    path: PathBuf,
    root: serde_json::Map<String, serde_json::Value>,
    manifest: &OwnershipManifest,
) -> Result<Option<(PathBuf, Vec<u8>)>, InstallError> {
    let family = ClientFamily::Antigravity;
    let Some(current) = root.get("rift-recall") else {
        return Ok(None);
    };
    let current_hash = fingerprint_json(current);
    let manifest_owned = manifest.owns_hook(family.token(), &current_hash);
    let descriptor_owned = !manifest.hooks.contains_key(family.token())
        && descriptor_rift_bin(repo, family.token())
            .is_some_and(|rift_bin| current == &hook_fragment(family, &rift_bin));
    if !(manifest_owned || descriptor_owned) {
        return Err(reserved_bundle_mismatch(path));
    }
    let bytes = remove_antigravity_hooks(&path, Some(&current_hash))?;
    Ok(bytes.map(|bytes| (path, bytes)))
}

fn recognizable_antigravity_hook_fragment(value: &serde_json::Value) -> bool {
    let family = ClientFamily::Antigravity;
    rift_hook_binary(value, family)
        .is_some_and(|rift_bin| value == &hook_fragment(family, &rift_bin))
}

fn reserved_bundle_mismatch(path: PathBuf) -> InstallError {
    InstallError::Config {
        path,
        reason: "reserved `rift-recall` hook bundle differs from Rift's ownership fingerprint"
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use crate::pretty_json;

    use super::super::descriptor::descriptor_path;
    use super::super::fixtures::{antigravity_hook_path, row_for};
    use super::super::hooks::hook_file;
    use super::super::lifecycle::{disable, disable_for_adoption, enable, enable_for_adoption};
    use super::super::manifest::manifest_path;
    use super::*;

    #[test]
    fn explicit_antigravity_enable_merges_and_disable_removes_only_the_reserved_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let path = antigravity_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let user_bundle = serde_json::json!({
            "team-lint": {
                "enabled": true,
                "PreToolUse": [{"matcher": "write_file", "hooks": []}]
            }
        });
        std::fs::write(&path, pretty_json(&user_bundle)).unwrap();

        let report = enable(dir.path(), Path::new("/opt/rift"), "antigravity").unwrap();
        let installed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(installed["team-lint"], user_bundle["team-lint"]);
        assert_eq!(
            installed["rift-recall"],
            hook_fragment(ClientFamily::Antigravity, Path::new("/opt/rift"),)
        );
        assert!(
            report.changed_files.contains(&path),
            "the explicit command reports the tracked hook file it changed"
        );
        assert_eq!(hook_file("antigravity"), Some(".agents/hooks.json"));
        assert_eq!(
            row_for(dir.path(), "antigravity").hook_wiring,
            Some(NativeHookWiring::Managed)
        );

        let second = enable(dir.path(), Path::new("/opt/rift"), "antigravity").unwrap();
        assert!(second.changed_files.is_empty(), "enable is idempotent");

        disable(dir.path(), "antigravity").unwrap();
        let remaining: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            remaining, user_bundle,
            "disable preserves unrelated bundles"
        );
    }

    #[test]
    fn antigravity_adoption_refreshes_the_descriptor_but_never_the_tracked_hook_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = antigravity_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let tracked = serde_json::json!({"team-lint": {"enabled": true}});
        let tracked_bytes = pretty_json(&tracked);
        std::fs::write(&path, &tracked_bytes).unwrap();

        let report =
            enable_for_adoption(dir.path(), Path::new("/opt/rift"), "antigravity").unwrap();
        assert!(descriptor_path(dir.path(), "antigravity").exists());
        assert!(!report.changed_files.contains(&path));
        assert_eq!(std::fs::read(&path).unwrap(), tracked_bytes);

        let before_disable = std::fs::read(&path).unwrap();
        disable_for_adoption(dir.path(), "antigravity").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before_disable);
    }

    #[test]
    fn malformed_or_edited_antigravity_hooks_are_refused_before_any_native_write() {
        for (label, bytes) in [
            ("malformed", b"{not-json".as_slice()),
            ("non-object", b"[]".as_slice()),
            (
                "edited-reserved",
                br#"{"rift-recall":{"enabled":false},"team-lint":{"enabled":true}}"#.as_slice(),
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = antigravity_hook_path(dir.path());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            let manifest = manifest_path(dir.path());
            std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
            let manifest_bytes = b"owner-state-must-not-change\n";
            std::fs::write(&manifest, manifest_bytes).unwrap();

            let error = enable(dir.path(), Path::new("/opt/rift"), "antigravity").unwrap_err();
            assert!(
                error.to_string().contains("invalid JSON object")
                    || error.to_string().contains("ownership fingerprint"),
                "{label}: {error}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes, "{label}: hook bytes");
            assert_eq!(
                std::fs::read(&manifest).unwrap(),
                manifest_bytes,
                "{label}: manifest bytes"
            );
            assert!(
                !descriptor_path(dir.path(), "antigravity").exists(),
                "{label}: descriptor was written before refusal"
            );
        }
    }

    #[test]
    fn an_edited_installed_antigravity_bundle_is_not_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = antigravity_hook_path(dir.path());
        enable(dir.path(), Path::new("/opt/rift"), "antigravity").unwrap();
        let mut installed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        installed["rift-recall"]["enabled"] = false.into();
        let edited = pretty_json(&installed);
        std::fs::write(&path, &edited).unwrap();

        let error = disable(dir.path(), "antigravity").unwrap_err();
        assert!(error.to_string().contains("ownership fingerprint"));
        assert_eq!(std::fs::read(&path).unwrap(), edited);
        assert!(
            descriptor_path(dir.path(), "antigravity").exists(),
            "disable must preflight the reserved bundle before removing the descriptor"
        );
    }

    #[test]
    fn changing_every_antigravity_command_path_does_not_bypass_the_fingerprint() {
        fn rewrite_strings(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::String(text) => {
                    *text = text.replace("/opt/rift", "/tmp/edited-rift");
                }
                serde_json::Value::Array(values) => {
                    values.iter_mut().for_each(rewrite_strings);
                }
                serde_json::Value::Object(values) => {
                    values.values_mut().for_each(rewrite_strings);
                }
                _ => {}
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let path = antigravity_hook_path(dir.path());
        enable(dir.path(), Path::new("/opt/rift"), "antigravity").unwrap();
        let mut installed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        rewrite_strings(&mut installed["rift-recall"]);
        let edited = pretty_json(&installed);
        std::fs::write(&path, &edited).unwrap();

        let enable_error = enable(dir.path(), Path::new("/tmp/edited-rift"), "antigravity")
            .expect_err("a consistently edited bundle is still not manifest-owned");
        assert!(enable_error.to_string().contains("ownership fingerprint"));
        assert_eq!(std::fs::read(&path).unwrap(), edited);

        let disable_error = disable(dir.path(), "antigravity")
            .expect_err("disable must enforce the same fingerprint");
        assert!(disable_error.to_string().contains("ownership fingerprint"));
        assert_eq!(std::fs::read(&path).unwrap(), edited);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_antigravity_hook_surfaces_are_refused_before_any_native_write() {
        use std::os::unix::fs::symlink;

        for symlink_directory in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let outside = dir.path().join("outside");
            std::fs::create_dir_all(&outside).unwrap();
            let outside_file = outside.join("hooks.json");
            let outside_bytes = b"{\"team-lint\":{\"enabled\":true}}\n";
            std::fs::write(&outside_file, outside_bytes).unwrap();
            if symlink_directory {
                symlink(&outside, dir.path().join(".agents")).unwrap();
            } else {
                std::fs::create_dir_all(dir.path().join(".agents")).unwrap();
                symlink(&outside_file, antigravity_hook_path(dir.path())).unwrap();
            }

            let error = enable(dir.path(), Path::new("/opt/rift"), "antigravity").unwrap_err();
            assert!(error.to_string().contains("symlinked Antigravity"));
            assert_eq!(std::fs::read(&outside_file).unwrap(), outside_bytes);
            assert!(!descriptor_path(dir.path(), "antigravity").exists());
            assert!(!manifest_path(dir.path()).exists());
        }
    }
}
