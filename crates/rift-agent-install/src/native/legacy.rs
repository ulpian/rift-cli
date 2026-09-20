//! Proofs that an installed hook fragment is Rift's own when the ownership
//! manifest cannot say so: its shape, the retired installer's manifest, and
//! the sidecar that installer left behind.

use std::path::Path;

use crate::{fingerprint_json, hook_subset, recognizable_rift_hook_fragment, ClientFamily};

/// Whether an installed hook fragment is Rift's OWN output, proved by something
/// other than today's ownership manifest — the shape the owner set on the
/// descriptor side, which the hooks side re-owed on 2026-08-24: a byte-recognisable legacy record is
/// RECOGNISED and refreshed, never reported as a conflict.
///
/// Three proofs, in cost order, any one of which is enough.
///
/// **1. The shape.** [`recognizable_rift_hook_fragment`] re-emits every
/// group from the candidate's OWN key set, so a fragment written from a
/// superseded verb table still recognizes while a single wrong byte still
/// refuses. The body this replaced compared against TODAY's
/// [`hook_fragment`], so the moment a verb table moved — Codex 9 → 10
/// when `SessionEnd` joined — a hook file Rift itself wrote stopped matching
/// every ownership route and `enable`, `disable` and Project Settings all
/// refused the repair they were asked to perform. Calibrated by
/// [`a_moved_verb_table_leaves_the_installed_hook_fragment_refreshable`]; the
/// deliberately unhandled shrink direction is planted by
/// [`a_rift_group_under_a_retired_verb_key_is_still_a_conflict`].
///
/// **2. The legacy installer's manifest**, `.rift/agents/recall-install.json`,
/// which fingerprints the fragment it wrote per client. The legacy installer
/// is the one that wrote the Claude Code fragment until `fce2baea`, and on a
/// machine that ran it the fingerprint is the only
/// record naming this file as Rift's — the native manifest has never held a
/// `hooks` entry for Claude Code.
///
/// **3. The sidecar** it left beside that manifest, `.rift/agents/
/// claude-code.claude-settings.json`: the fragment as written. Nothing has read
/// it since `fce2baea`. It is read here, and never written and never deleted —
/// a dead file that proves a fragment is ours outlives its tidiness, and this
/// crate does not remove artifacts it can still learn from.
pub(super) fn legacy_owned_hook_fragment(
    repo: &Path,
    family: ClientFamily,
    current: &serde_json::Value,
) -> bool {
    recognizable_rift_hook_fragment(current, family).is_some()
        || legacy_install_manifest_owns(repo, family, &fingerprint_json(current))
        || legacy_sidecar_owns(repo, family, current)
}

/// Proof 2: the legacy multi-client installer's manifest claims this family
/// with exactly the fragment fingerprint now on disk.
fn legacy_install_manifest_owns(repo: &Path, family: ClientFamily, hash: &str) -> bool {
    let Ok(bytes) = std::fs::read(repo.join(".rift/agents/recall-install.json")) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    manifest
        .get("clients")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|clients| {
            clients.iter().any(|client| {
                client.get("client").and_then(serde_json::Value::as_str) == Some(family.token())
                    && client
                        .get("hook_fingerprint")
                        .and_then(serde_json::Value::as_str)
                        == Some(hash)
            })
        })
}

/// Proof 3: the dead sidecar holds the same Rift groups the file now holds.
///
/// Claude Code only: it is the one family the legacy installer wrote a sidecar
/// copy for, and inventing the path for the others would be a second table of
/// filenames nothing ever wrote.
fn legacy_sidecar_owns(repo: &Path, family: ClientFamily, current: &serde_json::Value) -> bool {
    if family != ClientFamily::ClaudeCode {
        return false;
    }
    let path = repo.join(".rift/agents/claude-code.claude-settings.json");
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let Ok(sidecar) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    sidecar
        .get("hooks")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|hooks| &hook_subset(hooks, family) == current)
}

#[cfg(test)]
mod tests {
    use crate::{hook_fragment, pretty_json};

    use super::super::fixtures::{claude_hook_path, row_for};
    use super::super::lifecycle::enable;
    use super::super::NativeHookWiring;
    use super::*;

    /// Today's fragment with one field edited: still unmistakably Rift's
    /// command, but no longer re-emittable from its own keys — so only a
    /// RECORD of the install can prove it is ours.
    fn unrecognisable_rift_fragment(rift_bin: &Path) -> serde_json::Value {
        let mut fragment = hook_fragment(ClientFamily::ClaudeCode, rift_bin);
        for (_, groups) in fragment.as_object_mut().unwrap() {
            groups[0]["hooks"][0]["timeout"] = serde_json::json!(5);
        }
        assert!(
            recognizable_rift_hook_fragment(&fragment, ClientFamily::ClaudeCode).is_none(),
            "the fixture must defeat the shape route, or it proves nothing"
        );
        fragment
    }

    /// **The recognise-and-refresh shape, for the manifest the retired
    /// installer left behind**: a byte-recognisable legacy fingerprint is RECOGNISED and
    /// refreshed, never called a conflict.
    ///
    /// This is the state of every machine that ran `rift recall init` before
    /// `fce2baea`: `.claude/settings.local.json` holds a fragment the legacy
    /// installer wrote, `.rift/agents/recall-install.json` fingerprints it,
    /// and the native manifest has never held a `hooks` entry
    /// for Claude Code. Without this arm the writer's first act on a real
    /// machine would be to refuse to repair its own output.
    #[test]
    fn the_legacy_installers_manifest_still_proves_the_claude_fragment_is_ours() {
        let dir = tempfile::tempdir().unwrap();
        let path = claude_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let fragment = unrecognisable_rift_fragment(Path::new("/old/rift"));
        std::fs::write(
            &path,
            pretty_json(&serde_json::json!({ "hooks": fragment.clone() })),
        )
        .unwrap();
        let legacy = dir.path().join(".rift/agents/recall-install.json");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(
            &legacy,
            pretty_json(&serde_json::json!({
                "schema_version": 1,
                "clients": [{
                    "client": "claude-code",
                    "hook_path": ".claude/settings.local.json",
                    "hook_fingerprint": fingerprint_json(&fragment),
                }],
            })),
        )
        .unwrap();

        assert_eq!(
            row_for(dir.path(), "claude-code").hook_wiring,
            Some(NativeHookWiring::Stale),
            "recognised, not conflicted"
        );

        enable(dir.path(), Path::new("/opt/rift"), "claude-code").unwrap();
        let installed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            installed["hooks"],
            hook_fragment(ClientFamily::ClaudeCode, Path::new("/opt/rift")),
            "refreshed to today's table and today's binary"
        );
    }

    /// The other legacy record: `.rift/agents/claude-code.claude-settings.json`,
    /// the sidecar copy of what the retired installer wrote. Nothing has read
    /// it since `fce2baea`; it is read here as ownership evidence, and left
    /// exactly where it is — this crate does not delete a file it can still
    /// learn from.
    #[test]
    fn the_dead_claude_settings_sidecar_still_proves_the_fragment_is_ours() {
        let dir = tempfile::tempdir().unwrap();
        let path = claude_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let fragment = unrecognisable_rift_fragment(Path::new("/old/rift"));
        std::fs::write(
            &path,
            pretty_json(&serde_json::json!({ "hooks": fragment.clone() })),
        )
        .unwrap();
        let sidecar = dir
            .path()
            .join(".rift/agents/claude-code.claude-settings.json");
        std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        let sidecar_bytes = pretty_json(&serde_json::json!({ "hooks": fragment.clone() }));
        std::fs::write(&sidecar, &sidecar_bytes).unwrap();

        enable(dir.path(), Path::new("/opt/rift"), "claude-code").unwrap();
        let installed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            installed["hooks"],
            hook_fragment(ClientFamily::ClaudeCode, Path::new("/opt/rift"))
        );
        assert_eq!(
            std::fs::read(&sidecar).unwrap(),
            sidecar_bytes,
            "the sidecar is evidence, not a file to tidy away"
        );
    }
}
