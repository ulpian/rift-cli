//! Tamper-evidence primitive: SHA-256 leaves, Merkle roots, and a checkpoint
//! chain.
//!
//! Each captured event hashes to a SHA-256 *leaf*; a checkpoint's leaves combine
//! into a Merkle *root*; checkpoints *chain* via `prev`. This is the math behind
//! the database design's tamper-evidence rule and the engine `rift verify` runs. The
//! construction is **SHA-256** for substrate byte-compatibility (the checkpoint
//! `content_hash.txt` and the tamper chain are SHA-256 — see the dossier §3.6);
//! `blake3` is deliberately *not* used here.
//!
//! Domain separation (`0x00` leaves, `0x01` internal nodes, `0x02` empty root,
//! `0x03` checkpoint root, `0x04` commit hash) prevents second-preimage confusion
//! between the layers. Odd Merkle levels duplicate the last node; the empty tree
//! has a fixed [`empty_root`]. The crate is pure: no I/O, no clock, no RNG —
//! trivially testable and callable from both the store and the capture path.
//!
//! ## The chain layers
//!
//! 1. **leaf** — [`leaf_hash`] over an event's canonical bytes.
//! 2. **session root** — [`session_root`] (a Merkle tree over a session's ordered
//!    leaves).
//! 3. **checkpoint root** — [`checkpoint_root`] binds a checkpoint's session roots
//!    to its cost and attribution summary hashes.
//! 4. **commit hash** — [`checkpoint_commit_hash`] chains each checkpoint to its
//!    parent, so altering checkpoint *k* invalidates *k* and every successor.
//!
//! The `blake3` vs `sha2` role split the hashing rules mandate is already provided by
//! [`rift_core::Sha256Id`] (substrate/chain) vs [`rift_core::Blake3Id`] (internal
//! handles): they are distinct types with no conversion between them, so a blake3
//! handle can never masquerade as a chain hash (proven by the `compile_fail`
//! doctest on `rift_core::id`).
//!
//! **The construction is Rift-native**: checkpoint hashing has
//! no equivalent in the legacy backend to be byte-compatible with — the parity
//! harness compares native against native. The prefixes, odd-leaf rule, empty root, and
//! all four layers are therefore **pinned by hard-coded hex vectors** in this
//! crate's tests (`construction_is_pinned_by_hex_vectors`): any change to the
//! construction is a reviewable, breaking diff, never a silent drift.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use rift_core::{Result, RiftError, Sha256Id};
use sha2::{Digest, Sha256};

const LEAF_PREFIX: u8 = 0x00;
const NODE_PREFIX: u8 = 0x01;
const CHECKPOINT_PREFIX: u8 = 0x03;
const COMMIT_PREFIX: u8 = 0x04;

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

/// Extension trait deriving a [`CanonicalEvent`](rift_core::CanonicalEvent)'s
/// Merkle leaf from its canonical bytes.
///
/// The canonical event type lives in `rift-core` (the schema authority),
/// which deliberately does **not** depend on this hash layer; the leaf derivation
/// lives here instead so the dependency graph stays acyclic. Bring this trait
/// into scope (`use rift_hashchain::EventLeaf;`) to call `event.leaf()`.
pub trait EventLeaf {
    /// The SHA-256 Merkle leaf for this event (domain-separated, substrate-compatible).
    fn leaf(&self) -> Sha256Id;
}

impl EventLeaf for rift_core::CanonicalEvent {
    fn leaf(&self) -> Sha256Id {
        leaf_hash(&self.canonical_bytes())
    }
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
#[allow(clippy::indexing_slicing)] // loop indices bounds-guarded at every branch
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

/// A session's root: a Merkle tree over its **ordered** event leaves (chain
/// layer 2). Ordering — by `(turn_ordinal, event_ordinal)` at the call site — is
/// part of the commitment. This is [`merkle_root`] named for its chain layer.
#[must_use]
pub fn session_root(leaves: &[Sha256Id]) -> Sha256Id {
    merkle_root(leaves)
}

/// The checkpoint root (chain layer 3):
/// `SHA-256(0x03 ‖ session_root* ‖ cost_summary ‖ attribution_summary)`.
///
/// Binds *what happened* (the session roots, in `SessionInCheckpoint` ordinal
/// order) to *what we computed about it* (the canonical cost and attribution
/// rollup hashes) — so tampering with a cost number or an attribution count is as
/// detectable as tampering with an event. `session_roots` order is part of the
/// commitment.
#[must_use]
pub fn checkpoint_root(
    session_roots: &[Sha256Id],
    cost_summary: &Sha256Id,
    attribution_summary: &Sha256Id,
) -> Sha256Id {
    let mut h = Sha256::new();
    h.update([CHECKPOINT_PREFIX]);
    for r in session_roots {
        h.update(r.as_bytes());
    }
    h.update(cost_summary.as_bytes());
    h.update(attribution_summary.as_bytes());
    finalize(h)
}

/// The chained commit hash for a checkpoint (chain layer 4):
/// `SHA-256(0x04 ‖ parent_or_zero ‖ checkpoint_root ‖ meta)`.
///
/// Genesis uses a fixed all-zero parent. The chain is a hash-linked list
/// (`commit_hash_n = H(commit_hash_{n-1} ‖ checkpoint_root_n ‖ meta_n)`), so
/// mutating checkpoint *k* invalidates `commit_hash_k` and every successor.
#[must_use]
pub fn checkpoint_commit_hash(
    parent: Option<&Sha256Id>,
    checkpoint_root: &Sha256Id,
    meta: &[u8],
) -> Sha256Id {
    let mut h = Sha256::new();
    h.update([COMMIT_PREFIX]);
    h.update(parent.map_or([0u8; 32], |p| *p.as_bytes()));
    h.update(checkpoint_root.as_bytes());
    h.update(meta);
    finalize(h)
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
#[allow(clippy::indexing_slicing)] // loop indices bounds-guarded at every branch
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
#[allow(clippy::indexing_slicing)] // loop indices bounds-guarded at every branch
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

    /// The Rift-native construction, pinned byte-exact (see the module doc):
    /// the domain prefixes
    /// (0x00 leaf / 0x01 node / 0x02 empty / 0x03 checkpoint / 0x04 commit),
    /// the empty-root constant, the odd-leaf duplicate-last rule, and the layer
    /// compositions. Every other test in this crate is self-referential — this
    /// one is not: changing ANY part of the construction fails here first, as a
    /// reviewable diff of hex literals. Vectors cross-checked against an
    /// independent implementation (python hashlib).
    #[test]
    fn construction_is_pinned_by_hex_vectors() {
        assert_eq!(
            empty_root().to_hex(),
            "e2c3e6f55658a556f4403ed646adbc7326b09fcbb23f3df593e7c0de60e64794"
        );
        assert_eq!(
            leaf_hash(b"x").to_hex(),
            "3c7e9bc930dc93f01fa69985ef242d9f9e861f3c5355aa24ce5ef4b4b8a70ccb"
        );
        assert_eq!(
            merkle_root(&[leaf(1), leaf(2)]).to_hex(),
            "6bcf0e2e93e0a18e22789aee965e6553f4fbe93f0acfc4a705d691c8311c4965"
        );
        // three leaves: the odd level duplicates the LAST node (never carries)
        assert_eq!(
            merkle_root(&[leaf(1), leaf(2), leaf(3)]).to_hex(),
            "d10d803ddb112e8bab6820729655f564e52117c0b1aa14fb264b81282d36f9a4"
        );
        assert_eq!(
            checkpoint_root(&[leaf(1)], &leaf(2), &leaf(3)).to_hex(),
            "a33a16ff6a705953626ddd1fbf7f528f3d05c45fa8a4a941638e080c5d1b2b66"
        );
        // genesis (all-zero parent) and a chained commit
        assert_eq!(
            checkpoint_commit_hash(None, &leaf(1), b"meta").to_hex(),
            "2a8fb0627b6f5b0296e7aca845ef706210cea38835a21c2038f2ba49947cd9b7"
        );
        assert_eq!(
            checkpoint_commit_hash(Some(&leaf(9)), &leaf(1), b"meta").to_hex(),
            "b17e94f842f7f30132e4dddf7b2da20c1830dd166e46297931c1c1e3aa5826e9"
        );
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
    fn session_root_is_merkle_root() {
        let leaves = [leaf(1), leaf(2), leaf(3)];
        assert_eq!(session_root(&leaves), merkle_root(&leaves));
    }

    #[test]
    fn checkpoint_root_binds_summaries_and_order() {
        let s1 = session_root(&[leaf(1), leaf(2)]);
        let s2 = session_root(&[leaf(3)]);
        let cost = leaf(10);
        let attr = leaf(11);
        let base = checkpoint_root(&[s1, s2], &cost, &attr);
        // changing either summary changes the root
        assert_ne!(base, checkpoint_root(&[s1, s2], &leaf(99), &attr));
        assert_ne!(base, checkpoint_root(&[s1, s2], &cost, &leaf(99)));
        // session-root order is part of the commitment
        assert_ne!(base, checkpoint_root(&[s2, s1], &cost, &attr));
        // domain separation: a checkpoint root is not a bare merkle root
        assert_ne!(base, merkle_root(&[s1, s2, cost, attr]));
    }

    #[test]
    fn commit_hash_chains_to_parent_root_and_meta() {
        let root = leaf(5);
        let genesis = checkpoint_commit_hash(None, &root, b"meta");
        let parented = checkpoint_commit_hash(Some(&genesis), &root, b"meta");
        // genesis (zero parent) differs from a parented commit of the same root
        assert_ne!(genesis, parented);
        // depends on the checkpoint root
        assert_ne!(genesis, checkpoint_commit_hash(None, &leaf(6), b"meta"));
        // depends on the meta bytes
        assert_ne!(genesis, checkpoint_commit_hash(None, &root, b"meta2"));
        // deterministic
        assert_eq!(genesis, checkpoint_commit_hash(None, &root, b"meta"));
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

        /// Any generated valid chain verifies; ANY single structural mutation —
        /// relink, drop, swap-adjacent, or a corrupted genesis — at ANY position
        /// breaks it. (Root tampering with intact linkage is detected one layer
        /// up: `rift-store::verify_against_source` compares full `ChainLink`s,
        /// roots included, against the authoritative source.)
        #[test]
        fn chain_continuity(len in 3usize..16, pos in 1usize..15, mutation in 0u8..4) {
            let links: Vec<ChainLink> = (0..len)
                .map(|i| {
                    let prev = if i == 0 { None } else { Some(u8::try_from(i).unwrap_or(0)) };
                    link(u8::try_from(i + 1).unwrap_or(0), prev)
                })
                .collect();
            proptest::prop_assert!(verify_chain(&links).is_ok());

            let pos = 1 + (pos % (len - 1)); // a random non-genesis index
            let mut tampered = links.clone();
            match mutation {
                0 => tampered[pos].prev = Some(Sha256Id::from_bytes([200; 32])), // relink
                1 => {
                    let _ = tampered.remove(pos.min(len - 2)); // drop an interior link
                }
                2 => tampered.swap(pos - 1, pos), // reorder adjacent links
                _ => tampered[0].prev = Some(Sha256Id::from_bytes([7; 32])), // corrupt genesis
            }
            proptest::prop_assert!(
                verify_chain(&tampered).is_err(),
                "mutation {} at {} must break the chain", mutation, pos
            );
        }
    }
}
