//! The **stream cursor**: a resume point in a repository's append-only capture
//! stream.
//!
//! One type and one text spelling, so a consumer that stops persists a cursor
//! instead of inventing its own high-water mark.
//!
//! **TWO CONSUMERS, ONE RESUME PATH — and that is now the fact rather than the
//! intent.** [`crate::bundle`]'s export and recall's index warm
//! (`rift_recall_ledger::ingest`) both resume through [`suffix_after_by`] and
//! both take their head from [`head_by`]; the chain-typed [`suffix_after`] and
//! [`head_of`] are one-line delegations, so "shared" is a property of the call
//! graph and not of two bodies that agree. `rift_recall_ledger`'s
//! `the_index_warm_resumes_through_the_same_suffix_function_as_export` asserts
//! the two consumers hand back the same suffix for the same cursor.
//!
//! **What is still NOT on this path, stated because a docstring that overclaimed
//! is what this paragraph replaces:** the Canopy poller uses the projection
//! watermark described below (a change detector, not a position — see the last
//! section), and it is not migrated. And index warm's cursor is **reconstructed
//! from `ingest_watermark`'s `(branch, branch_tip)` against the current walk**
//! rather than persisted whole, because that relation has no column for a
//! position and canonical schema is not this path's to extend. State the
//! consequence exactly rather than let "shared" imply more than it buys: for
//! that consumer the id-at-position arm of [`suffix_after_by`] cannot fire,
//! because the position was derived from the id one line earlier. What catches
//! a rewritten stream there is **presence** — a recorded tip the walk no longer
//! holds resolves to no cursor at all, and the ingest falls back to a full pass
//! and says so in its report. Persisting the position would make the third arm
//! live for index warm too; that needs an additive column on a canonical
//! relation, which is a schema change, not a change this module may make
//! alone.
//!
//! # What orders the stream
//!
//! `chain_seq`: the **position of a checkpoint in the chain's own linkage**, as
//! [`crate::integrity::read_stored_links`] returns it — 1-based, with `0`
//! reserved for [`StreamCursor::origin`] ("nothing consumed"). It is not a
//! timestamp and not a row count.
//!
//! The two design notes that specified this disagreed on the field order
//! (`(chain_seq, ulid)` versus `(ulid, chain_seq)`); this module orders by
//! `chain_seq` first, because a ULID is wall-clock derived and two machines
//! with skewed clocks would reorder the stream under the other spelling — at
//! which point "everything after cursor C" stops being the set a consumer has
//! not seen. A checkpoint id is an **identity**, never the stream's order.
//!
//! # What the id is for
//!
//! `chain_seq` is already unique per repository (`read_stored_links` refuses a
//! forked, orphaned or multi-genesis chain), so the id never breaks a tie. It
//! **binds the cursor to one chain**: presenting a cursor whose id does not
//! match the checkpoint now sitting at that position means the chain the
//! consumer was reading is not the chain in front of it, and the resume fails
//! closed instead of silently handing back the wrong suffix.
//!
//! # Not the projection watermark
//!
//! [`crate::CanonicalStore::read_watermark`] + `read_rebuild_epoch`
//! are an advisory **change detector** — "has anything landed since I last
//! looked" — that resets backward on rebuild and answers no question about
//! *which* events. A stream cursor answers *which*, and for a given chain never
//! moves backward.

use core::fmt;

use rift_core::{Result, RiftError};
use rift_hashchain::ChainLink;

use crate::{integrity::read_stored_links, CanonicalStore};

/// The current cursor text schema version. Writers emit this; readers accept
/// `1..=CURSOR_SCHEMA_VERSION` and refuse anything else **whole**.
pub const CURSOR_SCHEMA_VERSION: u32 = 1;

/// The prefix every cursor spelling opens with.
const CURSOR_PREFIX: &str = "rift-cursor/";

/// The longest id a cursor will hold: a 64-hex `Sha256Id` today, comfortably
/// over a 26-character ULID should a per-checkpoint refs backend ever land.
const MAX_ID_LEN: usize = 64;

/// A resume point in one repository's capture stream.
///
/// Spelled `rift-cursor/<schema_version>:<chain_seq>:<checkpoint_id>`;
/// [`StreamCursor::origin`] is `rift-cursor/1:0:`.
///
/// The fields are private and there is one fallible constructor, so an
/// ill-formed cursor is unrepresentable rather than merely undocumented: the
/// origin's `chain_seq == 0` and its empty id are the same fact, and a value
/// carrying one without the other could not be rendered and read back.
///
/// **Round-trip is a bijection.** [`StreamCursor::parse`] accepts exactly the
/// canonical spelling — no leading zeros, no sign, no padding — so
/// `parse(s)?.to_string() == s` for every accepted `s`, and a cursor written
/// into a manifest is a function of its value and not of who typed it.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct StreamCursor {
    /// Ordered first, and the only field that orders the stream.
    chain_seq: u64,
    /// Identity, not order: the id of the checkpoint at `chain_seq`.
    checkpoint_id: String,
}

impl StreamCursor {
    /// The cursor meaning **nothing consumed** — sorts before every real
    /// position, and resuming from it yields the whole stream.
    #[must_use]
    pub fn origin() -> Self {
        Self {
            chain_seq: 0,
            checkpoint_id: String::new(),
        }
    }

    /// A cursor at `chain_seq` (1-based) naming `checkpoint_id`.
    ///
    /// # Errors
    /// [`RiftError::Validity`] if `chain_seq` is 0 (that position belongs to
    /// [`StreamCursor::origin`] and has no checkpoint), if the id is empty or
    /// longer than 64 bytes, or if the id is not `[0-9A-Za-z]` — the charset
    /// both dialects share, and the one that keeps the `:`-delimited spelling
    /// unambiguous.
    pub fn at(chain_seq: u64, checkpoint_id: &str) -> Result<Self> {
        if chain_seq == 0 {
            return Err(RiftError::validity(
                "stream cursor: chain_seq 0 is the origin and names no checkpoint",
            ));
        }
        if checkpoint_id.is_empty() {
            return Err(RiftError::validity(
                "stream cursor: a non-origin cursor must name a checkpoint",
            ));
        }
        if checkpoint_id.len() > MAX_ID_LEN {
            return Err(RiftError::validity(format!(
                "stream cursor: checkpoint id longer than {MAX_ID_LEN} bytes"
            )));
        }
        if !checkpoint_id.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(RiftError::validity(
                "stream cursor: checkpoint id must be ASCII alphanumeric",
            ));
        }
        Ok(Self {
            chain_seq,
            checkpoint_id: checkpoint_id.to_owned(),
        })
    }

    /// Position in the chain's linkage; `0` is [`StreamCursor::origin`].
    #[must_use]
    pub fn chain_seq(&self) -> u64 {
        self.chain_seq
    }

    /// The checkpoint id this cursor is bound to; empty for the origin.
    #[must_use]
    pub fn checkpoint_id(&self) -> &str {
        &self.checkpoint_id
    }

    /// Whether this is [`StreamCursor::origin`].
    #[must_use]
    pub fn is_origin(&self) -> bool {
        self.chain_seq == 0
    }

    /// Read a cursor from its text spelling.
    ///
    /// # Errors
    /// [`RiftError::Validity`] — with the reason, never the input — if the
    /// prefix is absent, the field count is wrong, the version is unknown
    /// (refused **whole**, never partially read), the sequence is not a
    /// canonical base-10 `u64`, or the id fails [`StreamCursor::at`]'s rules.
    pub fn parse(text: &str) -> Result<Self> {
        let body = text.strip_prefix(CURSOR_PREFIX).ok_or_else(|| {
            RiftError::validity("stream cursor: text does not open with `rift-cursor/`")
        })?;
        let mut fields = body.split(':');
        let (Some(version), Some(seq), Some(id), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(RiftError::validity(
                "stream cursor: expected exactly `<version>:<chain_seq>:<checkpoint_id>`",
            ));
        };

        let version = parse_canonical_u64(version).ok_or_else(|| {
            RiftError::validity("stream cursor: schema version is not a canonical integer")
        })?;
        // Readers accept <= current and refuse the rest WHOLE.
        // With one version shipped that is exactly 1; the shape is what a v2
        // reader inherits.
        if version == 0 || version > u64::from(CURSOR_SCHEMA_VERSION) {
            return Err(RiftError::validity(format!(
                "stream cursor: unknown schema version {version} (this reader accepts \
                 1..={CURSOR_SCHEMA_VERSION})"
            )));
        }

        let chain_seq = parse_canonical_u64(seq).ok_or_else(|| {
            RiftError::validity("stream cursor: chain_seq is not a canonical base-10 integer")
        })?;
        if chain_seq == 0 {
            if id.is_empty() {
                return Ok(Self::origin());
            }
            return Err(RiftError::validity(
                "stream cursor: chain_seq 0 is the origin and must name no checkpoint",
            ));
        }
        Self::at(chain_seq, id)
    }
}

impl fmt::Display for StreamCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{CURSOR_PREFIX}{CURSOR_SCHEMA_VERSION}:{}:{}",
            self.chain_seq, self.checkpoint_id
        )
    }
}

/// The cursor sitting at the head of `links` — the position a consumer that has
/// seen the whole chain would persist. An empty chain's head is
/// [`StreamCursor::origin`].
///
/// `links` must be the linkage-ordered chain
/// [`crate::integrity::read_stored_links`] returns; that function is what
/// refuses a forked, orphaned or multi-genesis chain, so this one may take
/// position as identity.
#[must_use]
pub fn head_of(links: &[ChainLink]) -> StreamCursor {
    head_by(links, |l| l.checkpoint_id.to_hex())
}

/// [`head_of`] for **any** deterministically ordered stream of checkpoints,
/// given the function that names each item.
///
/// This is the generic half of the resume path, and it exists because there are
/// two streams of the same checkpoints: the projection's stored chain (what
/// [`crate::bundle`] exports) and the substrate walk (what recall's index warm
/// ingests). They are read from different places and neither is a re-encoding of
/// the other, but the *resume algebra* over them is one algebra, and it was
/// written twice before this function existed.
#[must_use]
pub fn head_by<T>(items: &[T], id_of: impl Fn(&T) -> String) -> StreamCursor {
    match (u64::try_from(items.len()), items.last()) {
        (Ok(seq), Some(last)) => StreamCursor {
            chain_seq: seq,
            checkpoint_id: id_of(last),
        },
        _ => StreamCursor::origin(),
    }
}

/// The cursor naming `id`'s position in `items`, or `None` when this stream does
/// not hold `id` at all.
///
/// `None` is the interesting answer: a consumer holding a checkpoint id the
/// stream no longer contains is a consumer whose stream was rewritten under it,
/// and the only safe move is a full pass. It is deliberately not an `Err`,
/// because "resume from the beginning" is a legitimate outcome and not a
/// failure of anything.
///
/// The **first** match wins, so a stream that names one checkpoint twice
/// resolves to the earlier position — the conservative one, which re-reads
/// rather than skips.
#[must_use]
pub fn cursor_of<T>(items: &[T], id: &str, id_of: impl Fn(&T) -> String) -> Option<StreamCursor> {
    let index = items.iter().position(|it| id_of(it) == id)?;
    let seq = u64::try_from(index).ok()?.checked_add(1)?;
    Some(StreamCursor {
        chain_seq: seq,
        checkpoint_id: id.to_owned(),
    })
}

/// The links **strictly after** `cursor` — the resume path, taken today by
/// [`crate::bundle`] (see the module docs for what has not migrated onto it).
///
/// The cursor is checked against the chain before any suffix is handed back, so
/// the two ways a stale cursor could silently mis-resume both **fail closed**:
/// a cursor beyond the chain's end (the chain was rebuilt shorter, or the
/// cursor came from a different repository), and a cursor whose position now
/// holds a different checkpoint (the chain diverged under it).
///
/// # Errors
/// [`RiftError::Integrity`] naming the position — never any payload — when the
/// cursor is ahead of the chain or does not match the checkpoint at its
/// position.
pub fn suffix_after<'a>(links: &'a [ChainLink], cursor: &StreamCursor) -> Result<&'a [ChainLink]> {
    suffix_after_by(links, cursor, |l| l.checkpoint_id.to_hex())
}

/// [`suffix_after`] for **any** deterministically ordered stream of checkpoints,
/// given the function that names each item — the one resume path, which both
/// [`crate::bundle`]'s export and recall's index warm take.
///
/// Sharing this rather than agreeing with it is the point: two implementations
/// that happen to agree today are two implementations, and the design calls
/// for one. Everything the chain version documented is a property of *this*
/// body, so both consumers inherit the same monotonicity and the same two
/// fail-closed arms.
///
/// # Errors
/// [`RiftError::Integrity`] naming the position — never any payload — when the
/// cursor is ahead of the stream or does not match the item at its position.
pub fn suffix_after_by<'a, T>(
    items: &'a [T],
    cursor: &StreamCursor,
    id_of: impl Fn(&T) -> String,
) -> Result<&'a [T]> {
    if cursor.is_origin() {
        return Ok(items);
    }
    let seq = cursor.chain_seq();
    let index = usize::try_from(seq.saturating_sub(1)).map_err(|_| {
        RiftError::integrity(format!("stream cursor: chain_seq {seq} is unreadable"))
    })?;
    let Some(at) = items.get(index) else {
        return Err(RiftError::integrity(format!(
            "stream cursor: chain_seq {seq} is ahead of a chain of {} checkpoint(s)",
            items.len()
        )));
    };
    if id_of(at) != cursor.checkpoint_id() {
        return Err(RiftError::integrity(format!(
            "stream cursor: the chain holds a different checkpoint at chain_seq {seq}"
        )));
    }
    items.get(index.saturating_add(1)..).ok_or_else(|| {
        RiftError::integrity(format!(
            "stream cursor: chain_seq {seq} has no readable suffix"
        ))
    })
}

/// The head cursor of the projection's stored chain.
///
/// # Errors
/// Whatever [`crate::integrity::read_stored_links`] reports — a malformed chain
/// topology is tamper evidence and fails closed rather than yielding a cursor.
pub async fn head_cursor(store: &impl CanonicalStore) -> Result<StreamCursor> {
    Ok(head_of(&read_stored_links(store).await?))
}

/// The chain links a consumer holding `cursor` has not seen.
///
/// # Errors
/// [`RiftError::Integrity`] per [`suffix_after`], or a read/topology error from
/// [`crate::integrity::read_stored_links`].
pub async fn links_since(
    store: &impl CanonicalStore,
    cursor: &StreamCursor,
) -> Result<Vec<ChainLink>> {
    let links = read_stored_links(store).await?;
    Ok(suffix_after(&links, cursor)?.to_vec())
}

/// Base-10 `u64` in its **one** canonical spelling: ASCII digits only, and no
/// leading zero unless the value *is* `0`. `+7`, `007`, `7_000`, ` 7`, `0x7`
/// and an empty string are all refused, so two texts never name one cursor.
///
/// Shared rather than copied, and now by **three** texts: the cursor, the chain
/// segment, and the transfer container's frame (`bins/rift`'s `rift-bundle/1`).
/// Each pair of them has drifted once — the segment's header took a bare
/// `u32::from_str`, so `rift-chain-segment/01` and `/+1` were accepted where
/// `rift-cursor/01` and `/+1` were refused; the container repeated the identical
/// mistake for its version *and* its part lengths, accepting `rift-bundle/01`,
/// `/+1`, `manifest 02` and `manifest +2`. This is why the function is `pub`
/// rather than crate-private: a serialized-artifact reader outside this crate
/// that needs the rule must be able to **call** it, because the one that had to
/// re-derive it got it wrong. One rule, one place, or the texts drift again.
#[must_use]
pub fn parse_canonical_u64(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if s.len() > 1 && s.starts_with('0') {
        return None;
    }
    s.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use rift_core::Sha256Id;

    use super::*;
    use crate::{
        integrity::{record_checkpoint, CheckpointRecord},
        SqliteStore,
    };

    /// The id of the `i`-th (0-based) checkpoint of the chain tagged `tag`.
    /// `tag` distinguishes two chains that agree on length but not on content.
    fn id_of(i: u8, tag: u8) -> Sha256Id {
        let mut b = [tag; 32];
        b[0] = i.wrapping_add(1);
        Sha256Id::from_bytes(b)
    }

    /// A chain of `n` links, `chain[i].prev == chain[i-1].checkpoint_id`.
    fn chain(n: u8, tag: u8) -> Vec<ChainLink> {
        (0..n)
            .map(|i| ChainLink {
                checkpoint_id: id_of(i, tag),
                merkle_root: Sha256Id::from_bytes([i.wrapping_add(200); 32]),
                prev: i.checked_sub(1).map(|p| id_of(p, tag)),
            })
            .collect()
    }

    fn block<T>(f: impl core::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn origin_is_the_least_cursor_and_names_nothing() {
        let o = StreamCursor::origin();
        assert!(o.is_origin());
        assert_eq!(o.chain_seq(), 0);
        assert_eq!(o.checkpoint_id(), "");
        assert_eq!(o.to_string(), "rift-cursor/1:0:");
        // Every real position sorts after it.
        for seq in [1_u64, 2, u64::MAX] {
            assert!(o < StreamCursor::at(seq, "aa").unwrap());
        }
    }

    #[test]
    fn chain_seq_orders_before_the_id() {
        // The id is identity, not order: a later seq wins whatever the ids are.
        let early = StreamCursor::at(1, "zz").unwrap();
        let late = StreamCursor::at(2, "aa").unwrap();
        assert!(early < late);
        // Same seq (which a real chain cannot produce) still totalises.
        let a = StreamCursor::at(1, "aa").unwrap();
        assert!(a < early);
    }

    #[test]
    fn round_trip_is_a_bijection_on_the_examples() {
        for c in [
            StreamCursor::origin(),
            StreamCursor::at(1, "a").unwrap(),
            StreamCursor::at(u64::MAX, &"f".repeat(MAX_ID_LEN)).unwrap(),
        ] {
            let text = c.to_string();
            assert_eq!(StreamCursor::parse(&text).unwrap(), c);
            assert_eq!(StreamCursor::parse(&text).unwrap().to_string(), text);
        }
    }

    #[test]
    fn an_unknown_version_is_refused_whole() {
        // The failure that matters for a bundle crossing machines: a NEWER
        // writer's cursor is refused, never partially read as if the fields
        // still meant what this reader thinks.
        let newer = format!("rift-cursor/{}:3:abc", CURSOR_SCHEMA_VERSION + 1);
        let err = StreamCursor::parse(&newer).unwrap_err().to_string();
        assert!(err.contains("unknown schema version"), "{err}");
        // Version 0 never existed.
        assert!(StreamCursor::parse("rift-cursor/0:3:abc").is_err());
    }

    #[test]
    fn non_canonical_spellings_are_refused_so_one_value_has_one_text() {
        for text in [
            "rift-cursor/1:007:abc", // padded seq
            "rift-cursor/01:7:abc",  // padded version
            "rift-cursor/1:+7:abc",  // signed
            "rift-cursor/1:-7:abc",  // negative
            "rift-cursor/1: 7:abc",  // space
            "rift-cursor/1:0x7:abc", // radix prefix
            "rift-cursor/1::abc",    // empty seq
            "rift-cursor/1:7:",      // non-origin with no id
            "rift-cursor/1:0:abc",   // origin naming a checkpoint
            "rift-cursor/1:7:ab-cd", // id outside the shared charset
            "rift-cursor/1:7:ab:cd", // an extra field
            "rift-cursor/1:7",       // a missing field
            "rift-cursor/1:7:abc\n", // trailing newline
            "rift-cursor:1:7:abc",   // wrong prefix
            "1:7:abc",               // no prefix
            "",                      // empty
        ] {
            assert!(
                StreamCursor::parse(text).is_err(),
                "accepted a non-canonical spelling: {text:?}"
            );
        }
    }

    #[test]
    fn at_refuses_what_the_text_form_could_not_carry() {
        assert!(StreamCursor::at(0, "abc").is_err(), "0 is the origin");
        assert!(StreamCursor::at(1, "").is_err(), "empty id");
        assert!(StreamCursor::at(1, &"a".repeat(MAX_ID_LEN + 1)).is_err());
        for bad in ["ab:cd", "ab cd", "ab-cd", "ab\nc", "abé"] {
            assert!(StreamCursor::at(1, bad).is_err(), "accepted {bad:?}");
        }
        assert!(StreamCursor::at(1, &"a".repeat(MAX_ID_LEN)).is_ok());
    }

    // --- resume path ---------------------------------------------------------

    #[test]
    fn an_empty_chains_head_is_the_origin_and_resuming_from_it_yields_everything() {
        assert!(head_of(&[]).is_origin());
        assert_eq!(suffix_after(&[], &StreamCursor::origin()).unwrap(), &[]);
        let c = chain(3, 7);
        assert_eq!(suffix_after(&c, &StreamCursor::origin()).unwrap(), &c[..]);
    }

    #[test]
    fn the_head_cursor_names_the_last_checkpoint_and_resumes_to_nothing() {
        let c = chain(3, 7);
        let head = head_of(&c);
        assert_eq!(head.chain_seq(), 3);
        assert_eq!(head.checkpoint_id(), id_of(2, 7).to_hex());
        assert_eq!(suffix_after(&c, &head).unwrap(), &[]);
    }

    #[test]
    fn a_cursor_ahead_of_the_chain_fails_closed() {
        // The chain was rebuilt shorter, or the cursor is another repo's.
        let c = chain(2, 7);
        let ahead = StreamCursor::at(3, &id_of(2, 7).to_hex()).unwrap();
        let err = suffix_after(&c, &ahead).unwrap_err().to_string();
        assert!(err.contains("ahead of a chain of 2"), "{err}");
    }

    /// **Blind bundle review, defect (c)'s exact boundary, previously unpinned:**
    /// every existing "ahead of the chain" case used a NON-empty chain (a
    /// shorter one, or another repository's). A chain of exactly ZERO — a
    /// truly fresh projection receiving an INCREMENTAL segment — is the case
    /// `crate::bundle::import_segment` feeds through this same function
    /// (`suffix_after`, called on the *local* chain against the segment's
    /// `from_cursor`), and it is the case the review's measured consequence
    /// names: "the incremental bundle imported cleanly into a repo holding no
    /// chain at all". This pins that the origin-only escape (`is_origin` ->
    /// return the whole slice) never fires for a NON-origin cursor, so an
    /// empty local chain refuses a resuming cursor exactly like a short one.
    #[test]
    fn a_cursor_ahead_of_an_empty_chain_fails_closed() {
        let ahead = StreamCursor::at(1, &id_of(0, 7).to_hex()).unwrap();
        let err = suffix_after(&[], &ahead).unwrap_err().to_string();
        assert!(err.contains("ahead of a chain of 0"), "{err}");
    }

    #[test]
    fn a_cursor_from_a_diverged_chain_fails_closed_rather_than_resuming() {
        // Same length, same position, different history: the id is what catches
        // it. Without the identity check this hands back a suffix of a chain the
        // consumer has never read.
        let mine = chain(4, 7);
        let theirs = chain(4, 9);
        let cursor = StreamCursor::at(2, &id_of(1, 9).to_hex()).unwrap();
        assert_eq!(suffix_after(&theirs, &cursor).unwrap().len(), 2);
        let err = suffix_after(&mine, &cursor).unwrap_err().to_string();
        assert!(err.contains("different checkpoint at chain_seq 2"), "{err}");
    }

    #[test]
    fn head_cursor_and_links_since_read_the_store_through_the_same_path() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.create_canonical_schema().unwrap();
        assert!(block(head_cursor(&store)).unwrap().is_origin());

        let c = chain(3, 7);
        for (i, link) in c.iter().enumerate() {
            block(record_checkpoint(
                &store,
                &CheckpointRecord {
                    checkpoint_id: link.checkpoint_id,
                    strategy: "manual-commit".into(),
                    created_at: i64::try_from(i).unwrap(),
                    merkle_root: link.merkle_root,
                    prev: link.prev,
                    schema_version: 1,
                },
            ))
            .unwrap();
        }
        let head = block(head_cursor(&store)).unwrap();
        assert_eq!(head, head_of(&c));
        assert_eq!(block(links_since(&store, &head)).unwrap(), Vec::new());
        assert_eq!(
            block(links_since(&store, &StreamCursor::origin())).unwrap(),
            c
        );
        // And the fail-closed direction survives the async wrapper.
        let bogus = StreamCursor::at(2, &id_of(1, 9).to_hex()).unwrap();
        assert!(block(links_since(&store, &bogus)).is_err());
    }

    proptest::proptest! {
        /// Monotone under append: growing the chain strictly advances the head,
        /// and reading it twice gives the same answer. This is the property that
        /// lets a consumer persist a cursor and compare it later.
        #[test]
        fn the_head_cursor_is_monotone_under_append(n in 0_u8..12, extra in 1_u8..8) {
            let before = chain(n, 7);
            let after = chain(n.saturating_add(extra), 7);
            proptest::prop_assert!(head_of(&before) < head_of(&after));
            proptest::prop_assert_eq!(head_of(&before), head_of(&before));
        }

        /// Resume never skips and never repeats: consuming a chain in two chunks
        /// sees exactly the links consuming it in one pass sees, in the same
        /// order — whatever the split point.
        #[test]
        fn chunked_resume_equals_one_pass(n in 0_u8..16, cut in 0_usize..16) {
            let c = chain(n, 7);
            let cut = cut.min(c.len());
            let whole = suffix_after(&c, &StreamCursor::origin()).unwrap().to_vec();
            let first: Vec<ChainLink> = c.get(..cut).unwrap_or_default().to_vec();
            let mid = head_of(&first);
            let rest = suffix_after(&c, &mid).unwrap().to_vec();

            let mut chunked = first;
            chunked.extend(rest);
            proptest::prop_assert_eq!(chunked, whole);
        }

        /// Every cursor taken from a chain resumes to exactly the remainder, so
        /// the seen and unseen halves partition the chain with no overlap.
        #[test]
        fn a_cursor_partitions_its_own_chain(n in 1_u8..16, at in 1_usize..16) {
            let c = chain(n, 7);
            let at = at.min(c.len());
            let cursor = head_of(c.get(..at).unwrap_or_default());
            let rest = suffix_after(&c, &cursor).unwrap();
            proptest::prop_assert_eq!(rest.len(), c.len() - at);
            proptest::prop_assert_eq!(rest, c.get(at..).unwrap_or_default());
            proptest::prop_assert_eq!(cursor.chain_seq(), u64::try_from(at).unwrap());
        }

        /// **The property that makes one path serve two consumers**: the
        /// resume algebra does not depend on what the stream is
        /// made of. Export resumes over `ChainLink`s read from the projection's
        /// chain; index warm resumes over checkpoint ids read from the substrate
        /// walk. Same cursor, same stream, same answer — because it is the same
        /// function, and this fails the moment `suffix_after` stops delegating.
        #[test]
        fn the_generic_resume_agrees_with_the_chain_typed_one(n in 0_u8..16, at in 0_usize..16) {
            let c = chain(n, 7);
            let at = at.min(c.len());
            let cursor = head_of(c.get(..at).unwrap_or_default());
            let by_link: Vec<String> = suffix_after(&c, &cursor)
                .unwrap()
                .iter()
                .map(|l| l.checkpoint_id.to_hex())
                .collect();
            let ids: Vec<String> = c.iter().map(|l| l.checkpoint_id.to_hex()).collect();
            let by_id: Vec<String> = suffix_after_by(&ids, &cursor, Clone::clone)
                .unwrap()
                .to_vec();
            proptest::prop_assert_eq!(by_link, by_id);
            proptest::prop_assert_eq!(head_of(&c), head_by(&ids, Clone::clone));
        }

        /// Index warm's resume, on the shared body: locate a recorded tip, and
        /// the remainder is exactly what has not been seen.
        #[test]
        fn cursor_of_then_suffix_partitions_an_id_stream(n in 1_u8..16, at in 1_usize..16) {
            let ids: Vec<String> = (0..n).map(|i| format!("id{i}")).collect();
            let at = at.min(ids.len());
            let anchor = ids.get(at - 1).unwrap().clone();
            let cursor = cursor_of(&ids, &anchor, Clone::clone).unwrap();
            proptest::prop_assert_eq!(cursor.chain_seq(), u64::try_from(at).unwrap());
            let rest = suffix_after_by(&ids, &cursor, Clone::clone).unwrap();
            proptest::prop_assert_eq!(rest, ids.get(at..).unwrap_or_default());
        }

        /// The arm index warm actually depends on: an id the stream no longer
        /// holds resolves to **no cursor**, so the consumer falls back to a full
        /// pass instead of resuming from a position that means nothing.
        #[test]
        fn an_absent_id_has_no_cursor(n in 0_u8..16) {
            let ids: Vec<String> = (0..n).map(|i| format!("id{i}")).collect();
            proptest::prop_assert!(cursor_of(&ids, "not-in-this-stream", Clone::clone).is_none());
        }
    }

    /// The exact alternation sets
    /// [`every_accepted_text_is_its_own_canonical_spelling`] draws from,
    /// enumerated as its control. Lessons 1 and 2 of
    /// `ci/proptest-newline-baseline.txt` — reach is not coverage, and spelling
    /// is not assembly — apply to acceptance too: a round-trip property whose
    /// generator never *assembles* an accepted cursor asserts nothing and
    /// passes. This pins that the alphabet straddles acceptance **and** that it
    /// contains the padded-sequence class the round-trip property exists to
    /// catch. If the property's regexes are edited, edit these lists with them.
    const PREFIXES: [&str; 3] = ["rift-cursor/", "rift-cursor:", ""];
    const VERSIONS: [&str; 4] = ["1", "0", "2", "01"];
    const SEQS: [&str; 9] = ["0", "1", "7", "12", "07", "00", "007", "+7", ""];
    const IDS: [&str; 8] = ["", "a", "ab", "A0", "abc", "a:b", "a-b", "a b"];

    #[test]
    fn the_round_trip_alphabet_assembles_both_verdicts_and_the_padded_class() {
        let mut accepted = 0_u32;
        let mut total = 0_u32;
        for prefix in PREFIXES {
            for version in VERSIONS {
                for seq in SEQS {
                    for id in IDS {
                        total += 1;
                        if StreamCursor::parse(&format!("{prefix}{version}:{seq}:{id}")).is_ok() {
                            accepted += 1;
                        }
                    }
                }
            }
        }
        assert!(accepted >= 2, "alphabet never assembles an accepted cursor");
        assert!(accepted < total, "alphabet never assembles a refusal");
        // The class the property is FOR: a padded sequence sitting inside an
        // otherwise perfectly canonical cursor. Under a reader that accepted
        // leading zeros this text would parse and render back differently.
        assert!(SEQS.contains(&"007") && PREFIXES.contains(&"rift-cursor/"));
        assert!(VERSIONS.contains(&"1") && IDS.contains(&"abc"));
        assert!(StreamCursor::parse("rift-cursor/1:007:abc").is_err());
    }

    proptest::proptest! {
        /// Determinism + injectivity in one statement: whatever `parse` accepts
        /// renders back to the exact bytes it was given. If two texts ever named
        /// one cursor, one of them would fail this.
        ///
        /// THE DRAW IS ASSEMBLED, NOT NOISE, and it was rebuilt once for cause.
        /// The first version drew each field from a character class; it passed
        /// against the restored "leading zeros accepted" defect, because
        /// reaching that defect needs `rift-cursor/` **and** version `1` **and**
        /// a padded numeric seq **and** an alphanumeric id all in one draw —
        /// about 5e-5 per case, so ~1% over the default 256. Each field is now
        /// an alternation weighted toward the accepted spelling, with the
        /// non-canonical forms named explicitly.
        #[test]
        fn every_accepted_text_is_its_own_canonical_spelling(
            prefix in "(rift-cursor/|rift-cursor/|rift-cursor/|rift-cursor:|)",
            version in "(1|1|1|1|0|2|01| )",
            seq in "(0|1|7|12|07|00|007|4294967296|\\+7| 7|7 |)",
            id in "(|a|ab|A0|zz|abc|a:b|a-b|a b|a\\n)",
        ) {
            let s = format!("{prefix}{version}:{seq}:{id}");
            if let Ok(c) = StreamCursor::parse(&s) {
                proptest::prop_assert_eq!(c.to_string(), s);
            }
        }

        /// Parse is total: no input panics, and the prefixed shapes an adversary
        /// would actually try are exercised rather than only random noise.
        #[test]
        fn parse_never_panics(s in "(?s).{0,80}") {
            let _ = StreamCursor::parse(&s);
            let _ = StreamCursor::parse(&format!("{CURSOR_PREFIX}{s}"));
            let _ = StreamCursor::parse(&format!("{CURSOR_PREFIX}1:{s}"));
            let _ = StreamCursor::parse(&format!("{CURSOR_PREFIX}1:1:{s}"));
        }

        /// Every constructible cursor round-trips through its text form.
        #[test]
        fn constructed_cursors_round_trip(seq in 1_u64.., id in "[0-9A-Za-z]{1,64}") {
            let c = StreamCursor::at(seq, &id).unwrap();
            let text = c.to_string();
            proptest::prop_assert_eq!(StreamCursor::parse(&text).unwrap(), c);
        }

        /// `chain_seq` alone decides the order — the id never overturns it.
        #[test]
        fn order_follows_chain_seq(
            a in 1_u64..1_000_000,
            b in 1_u64..1_000_000,
            ia in "[0-9A-Za-z]{1,8}",
            ib in "[0-9A-Za-z]{1,8}",
        ) {
            let ca = StreamCursor::at(a, &ia).unwrap();
            let cb = StreamCursor::at(b, &ib).unwrap();
            if a < b {
                proptest::prop_assert!(ca < cb);
            } else if a > b {
                proptest::prop_assert!(ca > cb);
            }
        }
    }
}
