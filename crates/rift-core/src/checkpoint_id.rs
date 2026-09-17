//! The checkpoint-id derivation **rules** — one per canonicalization era, as
//! pure byte functions.
//!
//! **This module is the single writer of each rule.** The rules were composed
//! inside `rift-shadow` next to the leaf spec (which artifacts feed a shard's
//! Merkle root — capture policy, not id arithmetic), and every other place that
//! needed an id had to either depend on the whole capture crate or grow a
//! second implementation of the formula. Two implementations of one id rule is
//! the defect this project has paid for repeatedly, so the *rules* live here —
//! `sha256` over caller-supplied bytes, no leaf spec, no capture types — and
//! `rift-shadow` composes them with the leaf spec it owns. Anything that needs
//! a fixture id mints it through [`from_capture`] for the same reason: a
//! fixture that computed its own arithmetic would be a second writer.
//!
//! The eras, oldest first (the era a shard declares selects the rule a checker
//! re-derives; `rift-shadow`'s `checkpoint_ids_for_shard` owns that gate):
//!
//! * [`from_capture_only`] — the earliest derivation: the id bound the redacted
//!   capture and nothing else.
//! * [`from_capture`] — the capture-pair derivation (epochs 1–2): the id bound
//!   the redacted capture and the read facts (`tool_calls.jsonl`), an absent
//!   fact artifact digesting as the empty byte string.
//! * [`from_root`] — the root-bound derivation (epoch 3+): the id binds the
//!   shard's Merkle root, so every content artifact — present and future — is
//!   bound at once and the rule never has to move again. A future artifact
//!   extends the leaf spec, never this formula.
//!
//! The two superseded rules are not dead code. They still address live shards:
//! of the 103 shards on the reference repository's own checkpoint branch (all
//! epoch 1), 102 re-derive under [`from_capture`] and one — an empty capture
//! with no read facts — only under [`from_capture_only`]. A checker written to
//! a single formula calls an honest shard tampered, which is the failure the
//! era set exists to prevent. That measurement is pinned in
//! `tests::the_superseded_rule_still_addresses_the_measured_live_shard` and,
//! at the composition level, in `rift-shadow`'s
//! `the_superseded_derivation_still_addresses_a_live_shard`.

use sha2::{Digest as _, Sha256};

use crate::Sha256Id;

/// Lowercase-hex SHA-256 over the concatenation of `parts` — the one hashing
/// shape every era's rule shares.
fn sha256_hex(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    let mut b = [0u8; 32];
    b.copy_from_slice(&h.finalize());
    Sha256Id::from_bytes(b).to_hex()
}

/// The **root-bound** rule (epoch 3+): `sha256(session ‖ \0 ‖
/// sha256_hex(shard_root))`, over the shard's Merkle root as derived from its
/// own content artifacts.
///
/// Binding the root rather than a growing list of per-artifact digests is what
/// ended the formula-per-artifact treadmill: the root already commits to every
/// content artifact in canonical order, so a byte change in any of them moves
/// the id — and therefore the shard's path — while anchor drift (a second
/// commit in one session) moves neither. What bytes feed that root is the leaf
/// spec, which stays in `rift-shadow`: this function neither knows nor decides
/// it.
#[must_use]
pub fn from_root(session: &str, shard_root: Sha256Id) -> String {
    let root = shard_root.to_hex();
    sha256_hex(&[session.as_bytes(), b"\0", root.as_bytes()])
}

/// The root-bound rule's **full form**: the id of a
/// checkpoint naming N ≥ 1 sessions is `sha256(s0 ‖ \0 ‖ s1 ‖ \0 ‖ … ‖
/// sN-1 ‖ \0 ‖ sha256_hex(shard_root))`, sessions in their stable 0-based
/// index order (the order the shard's `metadata.json` records them in).
///
/// **For N = 1 this is [`from_root`], byte for byte** — pinned in
/// `tests::one_session_is_the_shipped_rule_verbatim` — which is why defining
/// the multi-session arm needed **no era bump**: every existing epoch-3 shard
/// names one session and re-derives unchanged, and N ≥ 2 was previously
/// *refused* by the writer, so no shard exists for the new arm to re-read.
///
/// **`None` refuses the inputs the frame cannot carry**: an empty session
/// list (a checkpoint of nothing is not addressable), or any session id
/// containing NUL. NUL is the frame byte; with sessions NUL-free the encoding
/// is injective — no two session lists produce one preimage, and no crafted
/// single "session" can imitate a multi-session list, because the trailing
/// component is always exactly the 64-hex root and every other frame slot is
/// NUL-free. A session id with a NUL in it has never been minted by any agent
/// this program captures; refusing it is cheaper than carrying a
/// length-prefix scheme that would break the N = 1 identity.
#[must_use]
pub fn from_root_sessions(sessions: &[&str], shard_root: Sha256Id) -> Option<String> {
    if sessions.is_empty() || sessions.iter().any(|s| s.contains('\0')) {
        return None;
    }
    let root = shard_root.to_hex();
    let mut parts: Vec<&[u8]> = Vec::with_capacity(sessions.len() * 2 + 1);
    for s in sessions {
        parts.push(s.as_bytes());
        parts.push(b"\0");
    }
    parts.push(root.as_bytes());
    Some(sha256_hex(&parts))
}

/// The **capture-pair** rule (epochs 1–2): `sha256(session ‖ \0 ‖
/// sha256_hex(redacted) ‖ \0 ‖ sha256_hex(tool_calls_jsonl))`, an absent
/// `tool_calls.jsonl` digesting as the empty byte string.
///
/// It bound the capture and the read facts and nothing else — which is exactly
/// why it was superseded: any artifact added after it (`full.jsonl`, a
/// transcript) sat outside the id, so the shard tree was not a function of the
/// shard path. It remains a live rule for the eras that minted with it.
#[must_use]
pub fn from_capture(session: &str, redacted: &[u8], tool_calls_jsonl: Option<&[u8]>) -> String {
    let content = sha256_hex(&[redacted]);
    let facts_digest = sha256_hex(&[tool_calls_jsonl.unwrap_or_default()]);
    sha256_hex(&[
        session.as_bytes(),
        b"\0",
        content.as_bytes(),
        b"\0",
        facts_digest.as_bytes(),
    ])
}

/// The **capture-only** rule, the earliest shipped: `sha256(session ‖ \0 ‖
/// sha256_hex(redacted))`.
///
/// Kept because it still addresses a live shard (see the module doc's 102/1
/// measurement); dropping it as dead code would condemn an honest checkpoint.
#[must_use]
pub fn from_capture_only(session: &str, redacted: &[u8]) -> String {
    sha256_hex(&[
        session.as_bytes(),
        b"\0",
        sha256_hex(&[redacted]).as_bytes(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session id from the reference repository's own checkpoint branch (not
    /// a secret — it is committed in that shard's `metadata.json`).
    const MEASURED_SESSION: &str = "ac5515af-4e58-426d-b45c-9e52b7ebd7da";

    /// The one live shard that only the capture-only rule addresses:
    /// `f62a4930…`, an empty capture with no read facts, measured on
    /// `refs/heads/rift/checkpoints/v1`. If the rule's bytes ever move, this
    /// reds with the id of the shard the change would condemn.
    #[test]
    fn the_superseded_rule_still_addresses_the_measured_live_shard() {
        assert_eq!(
            from_capture_only(MEASURED_SESSION, b""),
            "f62a493024792e8f4dc540579c3389e2b4f8826be45d3f51ebb336a4424cf960",
        );
    }

    /// The era-evidence property: the three rules produce pairwise different
    /// ids over the same bytes, so a shard's path is evidence of the era that
    /// minted it and a relabelled shard cannot satisfy another era's rule.
    #[test]
    fn the_three_rules_disagree_on_the_same_bytes() {
        let redacted = b"the redacted capture".as_slice();
        let root = Sha256Id::from_bytes([7u8; 32]);
        let capture_only = from_capture_only("s", redacted);
        let capture_pair = from_capture("s", redacted, None);
        let root_bound = from_root("s", root);
        assert_ne!(capture_only, capture_pair);
        assert_ne!(capture_only, root_bound);
        assert_ne!(capture_pair, root_bound);
    }

    /// The documented absence contract: an absent `tool_calls.jsonl` digests as
    /// the empty byte string. (The writer never freezes an *empty* artifact —
    /// `tool_calls_jsonl_bytes` returns `None` for no facts — so these are two
    /// spellings of the same shard, and they must derive one id.)
    #[test]
    fn an_absent_fact_artifact_digests_as_the_empty_byte_string() {
        assert_eq!(
            from_capture("s", b"capture", None),
            from_capture("s", b"capture", Some(b"")),
        );
    }

    /// **The N = 1 identity — the fact that makes the full form no era
    /// bump.** If these bytes ever drift apart, every existing epoch-3 shard
    /// would re-derive differently under the full form, and THAT would be an
    /// era bump wearing a refactor's name.
    #[test]
    fn one_session_is_the_shipped_rule_verbatim() {
        let root = Sha256Id::from_bytes([7u8; 32]);
        assert_eq!(
            from_root_sessions(&["s"], root).as_deref(),
            Some(from_root("s", root).as_str()),
        );
        // …and against the real measured constant's shape: a session id from
        // the reference repository, so the identity holds on real inputs too.
        assert_eq!(
            from_root_sessions(&[MEASURED_SESSION], root).as_deref(),
            Some(from_root(MEASURED_SESSION, root).as_str()),
        );
    }

    /// The full form's refusals and its injectivity levers: empty lists and
    /// NUL-carrying ids are `None`; order is load-bearing; a session list is
    /// not imitable by a crafted single session (the NUL it would need is the
    /// byte that gets it refused).
    #[test]
    fn the_full_form_refuses_nul_and_binds_order() {
        let root = Sha256Id::from_bytes([7u8; 32]);
        assert_eq!(
            from_root_sessions(&[], root),
            None,
            "a checkpoint of nothing"
        );
        assert_eq!(
            from_root_sessions(&["a\0b"], root),
            None,
            "NUL is the frame"
        );
        assert_eq!(from_root_sessions(&["a", "b\0"], root), None);
        assert_ne!(
            from_root_sessions(&["a", "b"], root),
            from_root_sessions(&["b", "a"], root),
            "index order is part of the identity"
        );
        assert_ne!(
            from_root_sessions(&["a", "b"], root),
            from_root_sessions(&["a"], root),
            "cardinality is part of the identity"
        );
        // The imitation a NUL-free domain forbids: the only string that would
        // make a single session collide with ["a","b"] is "a\0b" — refused.
        assert_eq!(from_root_sessions(&["a\0b"], root), None);
    }

    /// Every input is load-bearing: a different session or different capture
    /// bytes move every rule's id.
    #[test]
    fn session_and_content_both_move_every_rule() {
        let root = Sha256Id::from_bytes([7u8; 32]);
        let other_root = Sha256Id::from_bytes([8u8; 32]);
        assert_ne!(
            from_capture_only("a", b"capture"),
            from_capture_only("b", b"capture")
        );
        assert_ne!(
            from_capture_only("a", b"capture"),
            from_capture_only("a", b"other")
        );
        assert_ne!(
            from_capture("a", b"capture", Some(b"facts")),
            from_capture("a", b"capture", Some(b"other facts"))
        );
        assert_ne!(from_root("a", root), from_root("b", root));
        assert_ne!(from_root("a", root), from_root("a", other_root));
    }
}
