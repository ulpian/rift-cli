//! Tamper-evidence primitive: SHA-256 leaves, Merkle roots, and a checkpoint
//! chain.
//!
//! Each captured event hashes to a SHA-256 *leaf*; a checkpoint's leaves combine
//! into a Merkle *root*; checkpoints *chain* via `prev`. This is the math behind
//! `04-database-design` §4.3 rule 5 and the engine `rift verify` runs. The
//! construction is **SHA-256** for substrate byte-compatibility (the checkpoint
//! `content_hash.txt` and the tamper chain are SHA-256 — see the dossier §3.6);
//! `blake3` is deliberately *not* used here.
//!
//! Domain separation (`0x00` for leaves, `0x01` for internal nodes) prevents
//! second-preimage confusion between leaves and nodes. Odd levels duplicate the
//! last node; the empty tree has a fixed [`empty_root`]. The crate is pure: no
//! I/O, no clock, no RNG — trivially testable and callable from both the store
//! and the capture path.
//!
//! > **(verify)** the substrate's exact leaf/node prefixing and odd-leaf rule.
//! > If the Entire write path differs, *match it* — byte-compatibility with
//! > `content_hash.txt` is non-negotiable. A fixture vector pins this.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![allow(
    clippy::module_name_repetitions,
    clippy::doc_markdown,
    clippy::indexing_slicing
)]

use rift_core::{Result, RiftError, Sha256Id};
use sha2::{Digest, Sha256};

const LEAF_PREFIX: u8 = 0x00;
const NODE_PREFIX: u8 = 0x01;

fn finalize(hasher: Sha256) -> Sha256Id {
    let out = hasher.finalize();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&out);
    Sha256Id::from_bytes(bytes)
}

/// Canonical leaf hash of one event's canonical bytes: `SHA-256(0x00 ‖ bytes)`.
///
/// The *canonical encoding* of an event is the caller's responsibility (the
/// capture path), but its contract is documented here so leaf hashing stays
/// substrate-compatible.
#[must_use]
pub fn leaf_hash(canonical_event_bytes: &[u8]) -> Sha256Id {
    let mut h = Sha256::new();
    h.update([LEAF_PREFIX]);
    h.update(canonical_event_bytes);
    finalize(h)
}

/// Internal Merkle node over two children: `SHA-256(0x01 ‖ left ‖ right)`.
#[must_use]
pub fn node_hash(left: &Sha256Id, right: &Sha256Id) -> Sha256Id {
    let mut h = Sha256::new();
    h.update([NODE_PREFIX]);
    h.update(left.as_bytes());
    h.update(right.as_bytes());
    finalize(h)
}

/// The fixed root of an empty leaf set: `SHA-256(0x02 ‖ "rift-empty-merkle")`.
#[must_use]
pub fn empty_root() -> Sha256Id {
    let mut h = Sha256::new();
    h.update([0x02]);
    h.update(b"rift-empty-merkle");
    finalize(h)
}

/// Merkle root over an ordered list of leaves. Empty → [`empty_root`]; odd
/// levels duplicate the last node. Order is part of the commitment.
#[must_use]
pub fn merkle_root(leaves: &[Sha256Id]) -> Sha256Id {
    if leaves.is_empty() {
        return empty_root();
    }
    let mut level: Vec<Sha256Id> = leaves.to_vec();
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut i = 0;
        while i < level.len() {
            let left = level[i];
            let right = if i + 1 < level.len() {
                level[i + 1]
            } else {
                level[i]
            };
            next.push(node_hash(&left, &right));
            i += 2;
        }
        level = next;
    }
    level[0]
}

/// Verify a single checkpoint's claimed root against recomputed leaves.
///
/// # Errors
/// [`RiftError::Integrity`] if the recomputed root differs.
pub fn verify_checkpoint(leaves: &[Sha256Id], claimed_root: &Sha256Id) -> Result<()> {
    if &merkle_root(leaves) == claimed_root {
        Ok(())
    } else {
        Err(RiftError::integrity("checkpoint merkle root mismatch"))
    }
}

/// One link in the checkpoint chain. Genesis has `prev = None`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ChainLink {
    /// The content-addressed checkpoint id.
    pub checkpoint_id: Sha256Id,
    /// The Merkle root over this checkpoint's event leaves.
    pub merkle_root: Sha256Id,
    /// The previous checkpoint's id (`None` for genesis).
    pub prev: Option<Sha256Id>,
}

/// Verify a whole chain: genesis has no predecessor, and every later link's
/// `prev` equals the prior link's `checkpoint_id`. Returns the first break.
///
/// # Errors
/// [`RiftError::Integrity`] naming the first divergent index (no payload bytes).
pub fn verify_chain(links: &[ChainLink]) -> Result<()> {
    for (i, link) in links.iter().enumerate() {
        if i == 0 {
            if link.prev.is_some() {
                return Err(RiftError::integrity("genesis checkpoint must have no prev"));
            }
        } else {
            let expected = links[i - 1].checkpoint_id;
            if link.prev != Some(expected) {
                return Err(RiftError::integrity(format!(
                    "chain break at index {i}: prev does not match prior checkpoint"
                )));
            }
        }
    }
    Ok(())
}

/// Which side a Merkle sibling sits on relative to the path node.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    /// Sibling is to the left; combine as `node(sibling, acc)`.
    Left,
    /// Sibling is to the right; combine as `node(acc, sibling)`.
    Right,
}

/// A Merkle inclusion proof: the sibling hashes from leaf to root.
#[derive(Clone, Debug)]
pub struct MerkleProof {
    /// The proven leaf's index.
    pub index: usize,
    /// `(side, hash)` siblings, leaf level → root.
    pub siblings: Vec<(Side, Sha256Id)>,
}

/// Build an inclusion proof for `leaves[index]`.
///
/// # Errors
/// [`RiftError::Validity`] if `index` is out of range.
pub fn inclusion_proof(leaves: &[Sha256Id], index: usize) -> Result<MerkleProof> {
    if index >= leaves.len() {
        return Err(RiftError::validity("inclusion index out of range"));
    }
    let mut idx = index;
    let mut level: Vec<Sha256Id> = leaves.to_vec();
    let mut siblings = Vec::new();
    while level.len() > 1 {
        let (side, sib) = if idx.is_multiple_of(2) {
            // even: sibling on the right (duplicate self if it is the last node)
            let s = if idx + 1 < level.len() {
                level[idx + 1]
            } else {
                level[idx]
            };
            (Side::Right, s)
        } else {
            (Side::Left, level[idx - 1])
        };
        siblings.push((side, sib));
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut i = 0;
        while i < level.len() {
            let left = level[i];
            let right = if i + 1 < level.len() {
                level[i + 1]
            } else {
                level[i]
            };
            next.push(node_hash(&left, &right));
            i += 2;
        }
        level = next;
        idx /= 2;
    }
    Ok(MerkleProof { index, siblings })
}

/// Verify an inclusion proof reproduces `root` from `leaf`.
#[must_use]
pub fn verify_inclusion(proof: &MerkleProof, leaf: &Sha256Id, root: &Sha256Id) -> bool {
    let mut acc = *leaf;
    for (side, sib) in &proof.siblings {
        acc = match side {
            Side::Right => node_hash(&acc, sib),
            Side::Left => node_hash(sib, &acc),
        };
    }
    &acc == root
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(n: u8) -> Sha256Id {
        leaf_hash(&[n])
    }

    #[test]
    fn leaf_hash_is_domain_separated() {
        let l = leaf_hash(b"x");
        let n = node_hash(&l, &l);
        assert_ne!(l, n);
    }

    #[test]
    fn merkle_root_small_vectors() {
        assert_eq!(merkle_root(&[]), empty_root());
        let one = leaf(1);
        assert_eq!(merkle_root(&[one]), one);
        let two = merkle_root(&[leaf(1), leaf(2)]);
        assert_eq!(two, node_hash(&leaf(1), &leaf(2)));
        let three = merkle_root(&[leaf(1), leaf(2), leaf(3)]);
        let expected = node_hash(
            &node_hash(&leaf(1), &leaf(2)),
            &node_hash(&leaf(3), &leaf(3)),
        );
        assert_eq!(three, expected);
    }

    #[test]
    fn verify_checkpoint_accepts_correct_rejects_wrong() {
        let leaves = [leaf(1), leaf(2), leaf(3)];
        let root = merkle_root(&leaves);
        assert!(verify_checkpoint(&leaves, &root).is_ok());
        assert!(matches!(
            verify_checkpoint(&leaves, &leaf(9)),
            Err(RiftError::Integrity(_))
        ));
    }

    fn link(id: u8, prev: Option<u8>) -> ChainLink {
        ChainLink {
            checkpoint_id: Sha256Id::from_bytes([id; 32]),
            merkle_root: Sha256Id::from_bytes([id.wrapping_add(100); 32]),
            prev: prev.map(|p| Sha256Id::from_bytes([p; 32])),
        }
    }

    #[test]
    fn verify_chain_accepts_continuous_rejects_broken() {
        let good = [link(1, None), link(2, Some(1)), link(3, Some(2))];
        assert!(verify_chain(&good).is_ok());

        let broken = [link(1, None), link(2, Some(9)), link(3, Some(2))];
        assert!(matches!(
            verify_chain(&broken),
            Err(RiftError::Integrity(_))
        ));

        let bad_genesis = [link(1, Some(0)), link(2, Some(1))];
        assert!(matches!(
            verify_chain(&bad_genesis),
            Err(RiftError::Integrity(_))
        ));
    }

    #[test]
    fn inclusion_proof_roundtrip_and_forgery_rejected() {
        let leaves: Vec<Sha256Id> = (0u8..5).map(leaf).collect();
        let root = merkle_root(&leaves);
        for (i, l) in leaves.iter().enumerate() {
            let proof = inclusion_proof(&leaves, i).unwrap();
            assert!(verify_inclusion(&proof, l, &root), "leaf {i} must verify");
            assert!(!verify_inclusion(&proof, &leaf(99), &root));
        }
        assert!(inclusion_proof(&leaves, 5).is_err());
    }

    proptest::proptest! {
        /// Tamper-evidence: mutating any single leaf changes the root.
        #[test]
        fn any_leaf_change_changes_root(
            mut leaves in proptest::collection::vec(proptest::num::u8::ANY, 1..32),
            idx in 0usize..32,
        ) {
            let original: Vec<Sha256Id> = leaves.iter().map(|b| leaf_hash(&[*b])).collect();
            let root = merkle_root(&original);
            let i = idx % leaves.len();
            leaves[i] = leaves[i].wrapping_add(1);
            let mutated: Vec<Sha256Id> = leaves.iter().map(|b| leaf_hash(&[*b])).collect();
            proptest::prop_assert_ne!(root, merkle_root(&mutated));
        }

        /// Ordering is part of the commitment.
        #[test]
        fn root_is_order_sensitive(a in 0u8..255, b in 0u8..255) {
            proptest::prop_assume!(a != b);
            let x = merkle_root(&[leaf(a), leaf(b)]);
            let y = merkle_root(&[leaf(b), leaf(a)]);
            proptest::prop_assert_ne!(x, y);
        }

        /// Any generated valid chain verifies; relinking a non-genesis link breaks it.
        #[test]
        fn chain_continuity(len in 2usize..16) {
            let links: Vec<ChainLink> = (0..len)
                .map(|i| {
                    let prev = if i == 0 { None } else { Some(u8::try_from(i).unwrap_or(0)) };
                    link(u8::try_from(i + 1).unwrap_or(0), prev)
                })
                .collect();
            proptest::prop_assert!(verify_chain(&links).is_ok());
            let mut tampered = links.clone();
            tampered[1].prev = Some(Sha256Id::from_bytes([200; 32]));
            proptest::prop_assert!(verify_chain(&tampered).is_err());
        }
    }
}
