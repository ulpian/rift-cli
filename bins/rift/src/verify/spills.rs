//! The sealed spilled tool outputs of a shard, read back and re-checked.
//!
//! A spill's body is a Merkle leaf, so an edited one already fails the root and
//! the id. This pass is what names it: the `spills` manifest on the metadata
//! document beside each transcript records the digest and size of every body
//! it sealed, and a body that no longer matches is reported as its own file
//! rather than only as a checkpoint whose id no longer addresses it. The same
//! manifest is the reader's enumeration — the substrate has no tree listing —
//! so a body hidden from it is a leaf missing from the re-derivation.

#[cfg(test)]
use rift_recall_substrate::SubstrateReader;
#[cfg(test)]
use rift_shadow::backend::native_read::manifests::spill_claims as claims;
pub(super) use rift_shadow::backend::native_read::manifests::spill_leaves as leaves;
use rift_shadow::backend::native_read::manifests::SpillBytes;
#[cfg(test)]
use rift_shadow::backend::SpillLeafBytes;

use super::verdict::Verdict;

/// Each claimed body must be in the shard and match the digest and size its
/// manifest records; one that does not is named by its path.
pub(super) fn spill_digest_pass(id: &str, spills: &[SpillBytes]) -> Vec<Verdict> {
    spills
        .iter()
        .filter(|s| {
            let Some(body) = &s.body else {
                return true;
            };
            let digest = rift_shadow::sha256_hex(&[body]);
            s.claim.sha256.as_deref() != Some(digest.as_str())
                || s.claim.bytes != u64::try_from(body.len()).ok()
        })
        .map(|s| {
            Verdict::Tamper(format!(
                "{id}: {} does not match its recorded sha256",
                s.path()
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests;
