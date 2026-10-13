//! The native project-hook file of the standard families: which token has one,
//! how its Rift groups are inspected, planned and proved owned, and the
//! read-only question adoption asks of it (which binary it invokes). Codex's
//! trust in the file is its own module, `codex_trust`.

use std::path::{Path, PathBuf};

use crate::{
    fingerprint_json, fragment_contains_rift_command, hook_fragment, hook_subset,
    merge_standard_hooks, read_json_object, remove_standard_hooks, rift_hook_binary, ClientFamily,
    InstallError,
};

use super::antigravity::{
    inspect_antigravity_hooks, prepare_antigravity_hook_disable, prepare_antigravity_hook_enable,
};
use super::descriptor::descriptor_rift_bin;
use super::legacy::legacy_owned_hook_fragment;
use super::manifest::{read_manifest, OwnershipManifest};
use super::{NativeHookWiring, NativeWiring};

/// One client's hook-file write, planned before anything is touched: the
/// path, the merged bytes and the fingerprint the ownership manifest records.
///
/// Nothing is written while planning. A refusal must leave the tree exactly
/// as it was, and `enable` writes the descriptor first, so the conflict has to
/// be known before either file is touched.
pub(super) struct PlannedHook {
    pub(super) family: ClientFamily,
    pub(super) path: PathBuf,
    pub(super) bytes: Vec<u8>,
    pub(super) fragment_hash: String,
}

/// The repository-relative native hook file Rift writes for `agent_token`, if
/// it writes one — so a caller can NAME it (an enable's output line, a
/// refusal's repair instruction) without keeping a second table of paths.
#[must_use]
pub fn hook_file(agent_token: &str) -> Option<&'static str> {
    hook_family(agent_token).map(ClientFamily::hook_path)
}

/// The absolute `rift` binary an agent's INSTALLED hook fragment invokes, read
/// without writing anything.
///
/// The question adoption asks about a file it may not repair: `.agents/hooks.json`
/// is tracked by the repository, so a member whose bundle names a binary that
/// is no longer the family's record is reported `stale` and left exactly as it
/// is. `None` means no file, no Rift command in it, or a command
/// this emitter did not write.
#[must_use]
pub fn installed_hook_binary(repo: &Path, agent_token: &str) -> Option<PathBuf> {
    let family = ClientFamily::from_token(agent_token)?;
    let root = read_json_object(&repo.join(family.hook_path())).ok()?;
    let fragment = match family {
        ClientFamily::Antigravity => root.get("rift-recall")?,
        _ => root.get("hooks")?,
    };
    rift_hook_binary(fragment, family)
}

/// The client families whose native project-hook file explicit [`enable`]
/// writes, keyed by registry token. Antigravity's `.agents/hooks.json` is
/// tracked, so automatic worktree adoption remains verify-only; the explicit
/// command is the sole native path allowed to fingerprint-merge its reserved
/// `rift-recall` bundle. Gemini is outside this alpha and [`enable`] refuses it
/// long before here.
///
/// Claude Code joined this table late. Until then the only emitter of its
/// fragment was the legacy whole-set installer, whose last product caller went
/// away in `fce2baea` (2026-08-11) — so `rift enable --agent claude-code`
/// wrote a descriptor advertising nine hooks and registered none of them, in every
/// fresh clone and every linked worktree, and nothing reddened because nothing
/// compared the two.
///
/// [`enable`]: super::enable
pub(super) fn hook_family(agent_token: &str) -> Option<ClientFamily> {
    match agent_token {
        "claude-code" => Some(ClientFamily::ClaudeCode),
        "codex" => Some(ClientFamily::Codex),
        "antigravity" => Some(ClientFamily::Antigravity),
        _ => None,
    }
}

/// [`hook_family`], minus Antigravity's tracked file when the caller is not
/// the explicit command.
pub(super) fn writable_hook_family(
    agent_token: &str,
    write_tracked_hook: bool,
) -> Option<ClientFamily> {
    hook_family(agent_token)
        .filter(|family| write_tracked_hook || *family != ClientFamily::Antigravity)
}

pub(super) fn client_hook_path(repo: &Path, family: ClientFamily) -> PathBuf {
    repo.join(family.hook_path())
}

/// Read a family's hook file to a verdict, or to the JSON object the
/// family-specific inspection reads.
pub(super) fn inspect_client_hooks(
    repo: &Path,
    family: ClientFamily,
    descriptor_wiring: NativeWiring,
) -> NativeHookWiring {
    let path = client_hook_path(repo, family);
    if !path.exists() {
        return NativeHookWiring::Missing;
    }
    if path.parent().is_some_and(path_is_symlink) || path_is_symlink(&path) {
        return NativeHookWiring::Conflict;
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return NativeHookWiring::Invalid;
    };
    let Ok(root) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return NativeHookWiring::Invalid;
    };
    let Some(root) = root.as_object() else {
        return NativeHookWiring::Invalid;
    };
    match family {
        ClientFamily::Antigravity => inspect_antigravity_hooks(repo, root, descriptor_wiring),
        _ => inspect_standard_hooks(repo, family, root, descriptor_wiring),
    }
}

/// The `hooks` map of a Claude Code or Codex file. Rift's groups are the
/// subset under the family's verb keys, and ownership is proved by the
/// manifest, by the managed descriptor's own binary, or by a legacy record —
/// in that order of cost.
fn inspect_standard_hooks(
    repo: &Path,
    family: ClientFamily,
    root: &serde_json::Map<String, serde_json::Value>,
    descriptor_wiring: NativeWiring,
) -> NativeHookWiring {
    let Some(hooks) = root.get("hooks") else {
        return NativeHookWiring::Missing;
    };
    let Some(hooks) = hooks.as_object() else {
        return NativeHookWiring::Invalid;
    };
    let current = hook_subset(hooks, family);
    if !fragment_contains_rift_command(&serde_json::Value::Object(hooks.clone()), family) {
        return NativeHookWiring::Missing;
    }
    if !fragment_contains_rift_command(&current, family) {
        return NativeHookWiring::Conflict;
    }
    if read_manifest(repo).owns_hook(family.token(), &fingerprint_json(&current))
        || descriptor_emits(repo, family, descriptor_wiring, &current)
    {
        return NativeHookWiring::Managed;
    }
    if legacy_owned_hook_fragment(repo, family, &current) {
        NativeHookWiring::Stale
    } else {
        NativeHookWiring::Conflict
    }
}

/// Whether `current` is exactly what Rift emits for the binary the MANAGED
/// descriptor names — the ownership route for a hook file whose manifest entry
/// never travelled with the clone.
pub(super) fn descriptor_emits(
    repo: &Path,
    family: ClientFamily,
    descriptor_wiring: NativeWiring,
    current: &serde_json::Value,
) -> bool {
    if descriptor_wiring != NativeWiring::Managed {
        return false;
    }
    descriptor_rift_bin(repo, family.token())
        .is_some_and(|rift_bin| current == &hook_fragment(family, &rift_bin))
}

/// Plan the write of one client's hook fragment; see [`PlannedHook`] for why
/// nothing is written here.
pub(super) fn prepare_hook_enable(
    repo: &Path,
    family: ClientFamily,
    rift_bin: &Path,
    manifest: &OwnershipManifest,
) -> Result<PlannedHook, InstallError> {
    let path = client_hook_path(repo, family);
    refuse_hook_symlinks(&path, family)?;
    let root = read_json_object(&path)?;
    if family == ClientFamily::Antigravity {
        return prepare_antigravity_hook_enable(path, root, rift_bin, manifest);
    }
    let hook_value = root
        .get("hooks")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let current = hook_value
        .as_object()
        .map_or_else(|| serde_json::json!({}), |hooks| hook_subset(hooks, family));
    let contains_rift = fragment_contains_rift_command(&hook_value, family);
    let current_hash = fingerprint_json(&current);
    if contains_rift
        && !(manifest.owns_hook(family.token(), &current_hash)
            || legacy_owned_hook_fragment(repo, family, &current))
    {
        return Err(hook_fingerprint_mismatch(path, family));
    }
    let expected = hook_fragment(family, rift_bin);
    let previous = if contains_rift {
        Some(current_hash)
    } else {
        None
    };
    let bytes = merge_standard_hooks(&path, family, &expected, previous.as_deref(), false)?;
    Ok(PlannedHook {
        family,
        path,
        bytes,
        fragment_hash: fingerprint_json(&expected),
    })
}

/// Plan the removal of one client's Rift groups: `None` when there is nothing
/// of Rift's to remove, the path and the bytes without them otherwise.
pub(super) fn prepare_hook_disable(
    repo: &Path,
    family: ClientFamily,
    manifest: &OwnershipManifest,
) -> Result<Option<(PathBuf, Vec<u8>)>, InstallError> {
    let path = client_hook_path(repo, family);
    refuse_hook_symlinks(&path, family)?;
    if !path.exists() {
        return Ok(None);
    }
    let root = read_json_object(&path)?;
    if family == ClientFamily::Antigravity {
        return prepare_antigravity_hook_disable(repo, path, root, manifest);
    }
    let Some(hooks) = root.get("hooks").and_then(serde_json::Value::as_object) else {
        if root.contains_key("hooks") {
            return Err(InstallError::Config {
                path,
                reason: "`hooks` is not an object".to_owned(),
            });
        }
        return Ok(None);
    };
    let current = hook_subset(hooks, family);
    if !fragment_contains_rift_command(&serde_json::Value::Object(hooks.clone()), family) {
        return Ok(None);
    }
    let current_hash = fingerprint_json(&current);
    if !(manifest.owns_hook(family.token(), &current_hash)
        || legacy_owned_hook_fragment(repo, family, &current))
    {
        return Err(hook_fingerprint_mismatch(path, family));
    }
    let bytes = remove_standard_hooks(&path, family, Some(&current_hash))?;
    Ok(bytes.map(|bytes| (path, bytes)))
}

fn hook_fingerprint_mismatch(path: PathBuf, family: ClientFamily) -> InstallError {
    InstallError::Config {
        path,
        reason: format!(
            "{} Rift hooks differ from Rift's ownership fingerprint",
            family.display_name()
        ),
    }
}

/// Refuse a symlinked hook file, and a symlinked project directory around it,
/// before anything is written through either.
fn refuse_hook_symlinks(path: &Path, family: ClientFamily) -> Result<(), InstallError> {
    if let Some(dir) = path.parent() {
        refuse_symlink(dir, &format!("{} project directory", family.display_name()))?;
    }
    refuse_symlink(path, &format!("{} hook file", family.display_name()))
}

fn refuse_symlink(path: &Path, label: &str) -> Result<(), InstallError> {
    if path_is_symlink(path) {
        return Err(InstallError::Config {
            path: path.to_path_buf(),
            reason: format!("refusing symlinked {label}"),
        });
    }
    Ok(())
}

fn path_is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
}

#[cfg(test)]
mod tests {
    use rift_core::AgentId;
    use rift_shadow_hooks::AdapterRegistry;

    use crate::pretty_json;

    use super::super::descriptor::descriptor_path;
    use super::super::fixtures::{claude_hook_path, codex_hook_path, row_for};
    use super::super::lifecycle::{disable, enable};
    use super::*;

    #[test]
    fn codex_hooks_preserve_user_entries_and_disable_only_owned_groups() {
        let dir = tempfile::tempdir().unwrap();
        let path = codex_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let user_group = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "hooks": [{"type": "command", "command": "user-start"}]
                }],
                "PreToolUse": [{
                    "matcher": "*",
                    "hooks": [{"type": "command", "command": "user-pre-tool"}]
                }]
            }
        });
        std::fs::write(&path, pretty_json(&user_group)).unwrap();

        enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap();
        let installed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(installed["hooks"]["SessionStart"]
            .as_array()
            .is_some_and(|groups| groups.len() == 2));
        let pre_tool = installed["hooks"]["PreToolUse"]
            .as_array()
            .expect("the tool event remains an array");
        assert_eq!(pre_tool.len(), 2);
        assert!(pre_tool[0].to_string().contains(" hooks codex PreToolUse"));
        assert_eq!(pre_tool[1], user_group["hooks"]["PreToolUse"][0]);
        enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap();
        let reinstalled: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            reinstalled, installed,
            "re-enabling preserves both precedence and unrelated ordering byte-for-byte"
        );
        let codex = row_for(dir.path(), "codex");
        assert_eq!(codex.hook_wiring, Some(NativeHookWiring::Managed));
        assert!(codex.enabled());

        disable(dir.path(), "codex").unwrap();
        let remaining: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(remaining, user_group);
    }

    /// The settings file a user already has: an unrelated key and one hook of
    /// their own, both of which `enable` has to leave in place.
    fn user_claude_settings() -> serde_json::Value {
        serde_json::json!({
            "permissions": { "allow": ["Bash(git status:*)"] },
            "hooks": {
                "SessionStart": [{
                    "hooks": [{"type": "command", "command": "their-own-hook"}]
                }]
            }
        })
    }

    /// Write the user's settings, enable Claude Code over them, and read back
    /// the file as installed.
    fn enable_claude_code_over_user_settings(dir: &Path) -> (PathBuf, serde_json::Value) {
        let path = claude_hook_path(dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, pretty_json(&user_claude_settings())).unwrap();
        let report = enable(dir, Path::new("/opt/rift"), "claude-code").unwrap();
        assert!(report.changed_files.contains(&path), "{report:?}");
        let installed = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        (path, installed)
    }

    /// **The Claude Code hook fragment once had no live writer.**
    ///
    /// The legacy whole-set installer was the only emitter of
    /// `hook_fragment(ClaudeCode, …)`, and its last product caller went away in
    /// `fce2baea` (2026-08-11). From then on `rift enable --agent claude-code`
    /// wrote a descriptor advertising nine verbs, recorded it in the manifest,
    /// reported success — and registered no hook anywhere, in every fresh clone
    /// and every linked worktree. Nothing reddened because nothing compared the
    /// descriptor with the settings file.
    ///
    /// The fragment is written by the same emitter Codex's is, under the same
    /// manifest ownership, so the two cannot drift; the assertions here are the
    /// facts that were missing: the file exists, it carries exactly the
    /// adapter's declared verbs, and the user's own keys survive. The manifest's
    /// half is `the_claude_code_fragment_is_manifest_owned_and_the_row_reads_enabled`.
    #[test]
    fn claude_code_enable_registers_the_hooks_its_descriptor_advertises() {
        let dir = tempfile::tempdir().unwrap();
        let (_, installed) = enable_claude_code_over_user_settings(dir.path());
        let user_settings = user_claude_settings();
        assert_eq!(
            installed["permissions"], user_settings["permissions"],
            "an unrelated key is preserved verbatim"
        );
        assert!(
            installed["hooks"]["SessionStart"]
                .as_array()
                .is_some_and(|groups| groups
                    .iter()
                    .any(|group| group["hooks"][0]["command"] == "their-own-hook")),
            "the user's own hook survives beside Rift's: {installed}"
        );
        let registered: Vec<&str> = installed["hooks"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(_, groups)| fragment_contains_rift_command(groups, ClientFamily::ClaudeCode))
            .map(|(event, _)| event.as_str())
            .collect();
        let declared = AdapterRegistry::with_builtin()
            .get(&AgentId::new("claude-code"))
            .unwrap()
            .verbs();
        assert_eq!(
            registered.len(),
            declared.len(),
            "a hook per declared verb, not a second table: {registered:?} vs {declared:?}"
        );
    }

    /// The manifest says the settings file is Rift's, and the row reads
    /// enabled only once descriptor and fragment both stand.
    #[test]
    fn the_claude_code_fragment_is_manifest_owned_and_the_row_reads_enabled() {
        let dir = tempfile::tempdir().unwrap();
        enable_claude_code_over_user_settings(dir.path());
        let manifest = read_manifest(dir.path());
        let owned = manifest
            .hooks
            .get("claude-code")
            .expect("manifest ownership");
        assert_eq!(owned.path, ".claude/settings.local.json");
        assert_eq!(
            hook_file("claude-code"),
            Some(".claude/settings.local.json")
        );
        let row = row_for(dir.path(), "claude-code");
        assert_eq!(row.hook_wiring, Some(NativeHookWiring::Managed));
        assert!(
            row.enabled(),
            "descriptor plus fragment is what enabled means"
        );
    }

    /// Idempotent, then a clean round trip that leaves the user's file as
    /// Rift found it.
    #[test]
    fn claude_code_enable_is_idempotent_and_disable_restores_the_users_file() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = enable_claude_code_over_user_settings(dir.path());
        assert!(enable(dir.path(), Path::new("/opt/rift"), "claude-code")
            .unwrap()
            .changed_files
            .is_empty());
        disable(dir.path(), "claude-code").unwrap();
        let remaining: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(remaining, user_claude_settings());
    }

    /// The never-clobber rule reaches the new file on its first day: a fragment
    /// carrying a Rift command Rift did not write is a conflict, and the
    /// refusal lands before the descriptor is touched.
    #[test]
    fn a_user_modified_claude_fragment_is_refused_as_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let path = claude_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let theirs = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "hooks": [{
                        "type": "command",
                        "command": "\"/their/rift\" hooks claude-code SessionStart",
                        "timeout": 90
                    }]
                }]
            }
        });
        let bytes = pretty_json(&theirs);
        std::fs::write(&path, &bytes).unwrap();

        let error = enable(dir.path(), Path::new("/opt/rift"), "claude-code").unwrap_err();
        assert!(
            error.to_string().contains("ownership fingerprint"),
            "{error}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "left byte-identical");
        assert!(!descriptor_path(dir.path(), "claude-code").exists());
        assert_eq!(
            row_for(dir.path(), "claude-code").hook_wiring,
            Some(NativeHookWiring::Conflict)
        );
    }

    /// The verify-only read behind adoption's `stale` report: which `rift` does
    /// this installed fragment actually invoke? Antigravity's bundle has its own
    /// shape and its own file — which the repository TRACKS — so it is the one
    /// that has to work through this route rather than through an install.
    #[test]
    fn installed_hook_binary_reads_every_family_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let tracked = dir.path().join(".agents/hooks.json");
        std::fs::create_dir_all(tracked.parent().unwrap()).unwrap();
        let bundle = serde_json::json!({
            "rift-recall": hook_fragment(
                ClientFamily::Antigravity,
                Path::new("/old/rift"),
            ),
            "someone-else": { "enabled": true },
        });
        let bytes = pretty_json(&bundle);
        std::fs::write(&tracked, &bytes).unwrap();

        assert_eq!(
            installed_hook_binary(dir.path(), "antigravity"),
            Some(PathBuf::from("/old/rift"))
        );
        assert_eq!(std::fs::read(&tracked).unwrap(), bytes, "read, not written");
        assert_eq!(
            installed_hook_binary(dir.path(), "claude-code"),
            None,
            "no file, no claim"
        );
        assert_eq!(
            installed_hook_binary(dir.path(), "not-a-family"),
            None,
            "an unknown token names no hook file"
        );

        enable(dir.path(), Path::new("/opt/rift"), "claude-code").unwrap();
        assert_eq!(
            installed_hook_binary(dir.path(), "claude-code"),
            Some(PathBuf::from("/opt/rift"))
        );
    }

    #[test]
    fn codex_hook_conflicts_are_refused_before_descriptor_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = codex_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conflicting = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "hooks": [{
                        "type": "command",
                        "command": "\"/user/rift\" hooks codex SessionStart"
                    }]
                }]
            }
        });
        let bytes = pretty_json(&conflicting);
        std::fs::write(&path, &bytes).unwrap();

        let error = enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap_err();
        assert!(error.to_string().contains("ownership fingerprint"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(!descriptor_path(dir.path(), "codex").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_codex_hook_file_is_refused_without_writes() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.json");
        let outside_bytes = b"{\"hooks\":{}}";
        std::fs::write(&outside, outside_bytes).unwrap();
        let path = codex_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        symlink(&outside, &path).unwrap();

        let error = enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap_err();
        assert!(error.to_string().contains("symlinked Codex hook file"));
        assert_eq!(std::fs::read(outside).unwrap(), outside_bytes);
        assert!(!descriptor_path(dir.path(), "codex").exists());
    }

    #[test]
    fn exact_legacy_codex_hooks_are_adopted_and_refreshed() {
        let dir = tempfile::tempdir().unwrap();
        let path = codex_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let legacy = serde_json::json!({
            "hooks": hook_fragment(
                ClientFamily::Codex,
                Path::new("/old/rift"),
            )
        });
        std::fs::write(&path, pretty_json(&legacy)).unwrap();
        assert_eq!(
            row_for(dir.path(), "codex").hook_wiring,
            Some(NativeHookWiring::Stale)
        );

        enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap();
        let installed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(
            installed["hooks"],
            hook_fragment(ClientFamily::Codex, Path::new("/opt/rift"),)
        );
    }

    /// The superseded Codex emission: today's fragment minus `SessionEnd`,
    /// which is exactly what the uncorrected table produced, with a user group
    /// ahead of Rift's under `SessionStart`. Returned with that group so a test
    /// can assert it survives.
    fn superseded_codex_hooks(old_bin: &Path) -> (serde_json::Value, serde_json::Value) {
        let mut superseded = hook_fragment(ClientFamily::Codex, old_bin);
        assert!(
            superseded
                .as_object_mut()
                .unwrap()
                .remove("SessionEnd")
                .is_some(),
            "SessionEnd must be in today's table for this plant to mean anything"
        );
        // THE CALIBRATION CLAUSE: the superseded fragment no longer equals
        // today's, so a recognizer built on today-fragment equality — the
        // previous body of `recognizable_codex_hook_fragment` — cannot pass
        // this test. Deleting this line leaves the migration with nothing to
        // be a migration FROM.
        assert_ne!(
            superseded,
            hook_fragment(ClientFamily::Codex, old_bin),
            "THE BREAKAGE: a moved verb table unmatches the installed fragment"
        );
        let user_group =
            serde_json::json!({"hooks": [{"type": "command", "command": "user-start"}]});
        superseded["SessionStart"]
            .as_array_mut()
            .unwrap()
            .insert(0, user_group.clone());
        (superseded, user_group)
    }

    /// Write the superseded emission as the Codex hook file, with no manifest
    /// anywhere — the stranded population has none.
    fn write_superseded_codex_hooks(dir: &Path) -> (PathBuf, serde_json::Value) {
        let path = codex_hook_path(dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let (superseded, user_group) = superseded_codex_hooks(Path::new("/old/rift"));
        std::fs::write(
            &path,
            pretty_json(&serde_json::json!({"hooks": superseded})),
        )
        .unwrap();
        (path, user_group)
    }

    /// **Hooks side: correcting a verb table must leave a hook file Rift itself
    /// wrote REFRESHABLE, never conflicted.**
    ///
    /// The descriptor half of the same rule landed with
    /// [`a_moved_verb_table_conflicts_the_pre_manifest_install_without_arm_two`];
    /// this is the half it was missing, measured live on the main
    /// checkout: `.codex/hooks.json` held the byte-exact 9-verb emission from
    /// before `SessionEnd` joined `CodexAdapter::verbs()`, the manifest
    /// predated its `hooks` map altogether, and every ownership route compared
    /// against TODAY's 10-key fragment — so inspect read `Conflict`, and
    /// enable, disable, and Project Settings all refused the repair they were
    /// asked to perform.
    #[test]
    fn a_moved_verb_table_leaves_the_installed_hook_fragment_refreshable() {
        let dir = tempfile::tempdir().unwrap();
        let (path, user_group) = write_superseded_codex_hooks(dir.path());
        assert_eq!(
            row_for(dir.path(), "codex").hook_wiring,
            Some(NativeHookWiring::Stale),
            "a fragment Rift wrote from an older table is refreshable, not a conflict"
        );

        let new_bin = Path::new("/opt/rift");
        enable(dir.path(), new_bin, "codex").unwrap();
        let installed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let expected = hook_fragment(ClientFamily::Codex, new_bin);
        for (event, groups) in expected.as_object().unwrap() {
            if event == "SessionStart" {
                continue;
            }
            assert_eq!(&installed["hooks"][event], groups, "{event} not refreshed");
        }
        assert_eq!(
            installed["hooks"]["SessionStart"].as_array().map(Vec::len),
            Some(2),
            "the user group must survive the refresh"
        );
        assert_eq!(
            installed["hooks"]["SessionStart"][0], user_group,
            "user entries are preserved, never rewritten"
        );
    }

    /// The refresh takes manifest ownership of the new fragment, so the row
    /// reads managed — and enabled — afterwards.
    #[test]
    fn refreshing_a_superseded_fragment_takes_manifest_ownership() {
        let dir = tempfile::tempdir().unwrap();
        write_superseded_codex_hooks(dir.path());
        let new_bin = Path::new("/opt/rift");
        enable(dir.path(), new_bin, "codex").unwrap();
        let expected = hook_fragment(ClientFamily::Codex, new_bin);
        assert_eq!(
            read_manifest(dir.path())
                .hooks
                .get("codex")
                .map(|owned| owned.fragment_hash.clone()),
            Some(fingerprint_json(&expected)),
            "the refresh takes manifest ownership of the new fragment"
        );
        let row = row_for(dir.path(), "codex");
        assert_eq!(row.hook_wiring, Some(NativeHookWiring::Managed));
        assert!(row.enabled());
    }

    /// Disable symmetry for the same stranded population: the superseded
    /// fragment is recognized, Rift's groups leave, the user's stays, and a
    /// second disable is a no-op.
    #[test]
    fn a_moved_verb_table_still_disables_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let path = codex_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut superseded = hook_fragment(ClientFamily::Codex, Path::new("/old/rift"));
        assert!(superseded
            .as_object_mut()
            .unwrap()
            .remove("SessionEnd")
            .is_some());
        let user_group =
            serde_json::json!({"hooks": [{"type": "command", "command": "user-start"}]});
        superseded["SessionStart"]
            .as_array_mut()
            .unwrap()
            .insert(0, user_group.clone());
        std::fs::write(
            &path,
            pretty_json(&serde_json::json!({"hooks": superseded})),
        )
        .unwrap();

        disable(dir.path(), "codex").unwrap();
        let remaining: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            remaining,
            serde_json::json!({"hooks": {"SessionStart": [user_group]}}),
            "only Rift's groups leave; the user's stays"
        );
        assert!(disable(dir.path(), "codex")
            .unwrap()
            .changed_files
            .is_empty());
    }

    /// A Codex handler in Rift's own spelling, for building near-misses.
    fn codex_handler(verb: &str, bin: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "command",
            "command": format!("\"{bin}\" hooks codex {verb}"),
            "timeout": 10,
        })
    }

    /// Each candidate differs from Rift's emission by exactly one byte-level
    /// fact.
    fn hook_files_that_merely_resemble_rifts() -> Vec<(&'static str, serde_json::Value)> {
        vec![
            (
                "an edited timeout",
                serde_json::json!({
                    "SessionStart": [{"hooks": [{
                        "type": "command",
                        "command": "\"/opt/rift\" hooks codex SessionStart",
                        "timeout": 11,
                    }]}]
                }),
            ),
            (
                "a second handler in the group",
                serde_json::json!({
                    "SessionStart": [{"hooks": [
                        codex_handler("SessionStart", "/opt/rift"),
                        codex_handler("SessionStart", "/opt/rift"),
                    ]}]
                }),
            ),
            (
                "a verb that does not match its key",
                serde_json::json!({
                    "Stop": [{"hooks": [codex_handler("SessionStart", "/opt/rift")]}]
                }),
            ),
            (
                "a relative binary",
                serde_json::json!({
                    "SessionStart": [{"hooks": [codex_handler("SessionStart", "rift")]}]
                }),
            ),
            (
                "two binaries across groups",
                serde_json::json!({
                    "SessionStart": [{"hooks": [codex_handler("SessionStart", "/a/rift")]}],
                    "Stop": [{"hooks": [codex_handler("Stop", "/b/rift")]}]
                }),
            ),
            (
                "a matcher on a non-tool event",
                serde_json::json!({
                    "SessionStart": [{
                        "matcher": "*",
                        "hooks": [codex_handler("SessionStart", "/opt/rift")],
                    }]
                }),
            ),
        ]
    }

    /// The negative arms that stop the shape migration being a hole: each
    /// candidate stays a refused `Conflict` in BOTH repair directions with the
    /// file left byte-identical.
    #[test]
    fn a_hook_file_that_merely_resembles_rifts_stays_a_conflict() {
        for (why, hooks) in hook_files_that_merely_resemble_rifts() {
            let dir = tempfile::tempdir().unwrap();
            let path = codex_hook_path(dir.path());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let bytes = pretty_json(&serde_json::json!({"hooks": hooks}));
            std::fs::write(&path, &bytes).unwrap();
            assert_eq!(
                row_for(dir.path(), "codex").hook_wiring,
                Some(NativeHookWiring::Conflict),
                "{why}: must stay a conflict"
            );
            let enable_err = enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap_err();
            assert!(
                enable_err.to_string().contains("ownership fingerprint"),
                "{why}: {enable_err}"
            );
            let disable_err = disable(dir.path(), "codex").unwrap_err();
            assert!(
                disable_err.to_string().contains("ownership fingerprint"),
                "{why}: {disable_err}"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                bytes,
                "{why}: bytes must be untouched"
            );
            assert!(
                !descriptor_path(dir.path(), "codex").exists(),
                "{why}: a refusal wrote a descriptor"
            );
        }
    }

    /// **The deliberately unhandled direction: a verb table that SHRINKS.**
    ///
    /// A Rift-shaped group under a key today's table no longer declares never
    /// enters `hook_subset`, so ownership cannot be established over it and
    /// the file reads `Conflict`. Handling it means widening the subset's key
    /// domain — fingerprint semantics for every standard family — against a
    /// measured population of zero: no adapter's table has ever shrunk. So the
    /// case is PLANTED, not handled, exactly the way arm 1's breakage was
    /// planted before the verb-table correction falsified its premise: when a table first
    /// shrinks, this test is what that migration is FROM.
    #[test]
    fn a_rift_group_under_a_retired_verb_key_is_still_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let path = codex_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // "PreTask" is a verb no Codex table ever declared — standing in for a
        // retired one, which is indistinguishable to the subset.
        let bytes = pretty_json(&serde_json::json!({
            "hooks": {
                "PreTask": [{"hooks": [{
                    "type": "command",
                    "command": "\"/opt/rift\" hooks codex PreTask",
                    "timeout": 10,
                }]}]
            }
        }));
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(
            row_for(dir.path(), "codex").hook_wiring,
            Some(NativeHookWiring::Conflict)
        );
        assert!(enable(dir.path(), Path::new("/opt/rift"), "codex").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}
