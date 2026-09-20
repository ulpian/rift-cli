//! The ownership manifest under `.rift/state/`: the fingerprints that let
//! Rift tell its own descriptors, hook groups and skill regions from a user's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::InstallError;

use super::write_if_changed;

/// What Rift wrote, by fingerprint. A file whose bytes no longer hash to the
/// recorded value is somebody else's, and is never overwritten or removed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct OwnershipManifest {
    #[serde(default = "manifest_version")]
    pub(super) schema_version: u16,
    #[serde(default)]
    pub(super) descriptors: BTreeMap<String, String>,
    #[serde(default)]
    pub(super) hooks: BTreeMap<String, OwnedHook>,
    #[serde(default)]
    pub(super) skills: BTreeMap<String, OwnedSkill>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct OwnedSkill {
    pub(super) path: String,
    pub(super) region_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct OwnedHook {
    pub(super) path: String,
    pub(super) fragment_hash: String,
}

impl Default for OwnershipManifest {
    fn default() -> Self {
        Self {
            schema_version: manifest_version(),
            descriptors: BTreeMap::new(),
            hooks: BTreeMap::new(),
            skills: BTreeMap::new(),
        }
    }
}

impl OwnershipManifest {
    /// Whether the manifest records `hash` as the descriptor Rift wrote for
    /// `token`.
    pub(super) fn owns_descriptor(&self, token: &str, hash: &str) -> bool {
        self.descriptors
            .get(token)
            .is_some_and(|owned| owned == hash)
    }

    /// Whether the manifest records `hash` as the hook fragment Rift wrote for
    /// the family `token` names.
    pub(super) fn owns_hook(&self, token: &str, hash: &str) -> bool {
        self.hooks
            .get(token)
            .is_some_and(|owned| owned.fragment_hash == hash)
    }
}

const fn manifest_version() -> u16 {
    1
}

pub(super) fn manifest_path(repo: &Path) -> PathBuf {
    repo.join(".rift/state/native-integrations.json")
}

/// A manifest of another schema version reads as absent, so ownership is
/// re-proved by the descriptor and hook shapes rather than trusted.
pub(super) fn read_manifest(repo: &Path) -> OwnershipManifest {
    std::fs::read(manifest_path(repo))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|manifest: &OwnershipManifest| manifest.schema_version == manifest_version())
        .unwrap_or_default()
}

/// Write the manifest when its bytes changed; `true` when a write happened.
pub(super) fn write_manifest(
    repo: &Path,
    manifest: &OwnershipManifest,
) -> Result<bool, InstallError> {
    let mut bytes = serde_json::to_vec_pretty(manifest).unwrap_or_default();
    bytes.push(b'\n');
    write_if_changed(&manifest_path(repo), &bytes)
}

pub(super) fn fingerprint(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}
