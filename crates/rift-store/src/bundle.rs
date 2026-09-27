//! The **chain segment**: the verifiable half of an offline bundle.
//!
//! A bundle is a git bundle of checkpoint refs plus a chain segment plus a
//! manifest. This module is the chain segment and its export/import path — the
//! part that decides whether a bundle may be trusted, and the part that does
//! not need a per-checkpoint refs backend to exist. The git-bundle payload
//! and the ref list ride the manifest and are not built here; the handoff line
//! in the ledger says so rather than implying this is the whole artifact.
//!
//! # What a segment proves, and what it does not
//!
//! A segment carries `[from_cursor, to_cursor]` and the links between them. Its
//! verification is about **order and membership**:
//!
//! * every link's `prev` is the previous link's id, and the first link attaches
//!   to `from_cursor` (or is genesis, when `from_cursor` is the origin);
//! * `to_cursor` names the last link, at the position the arithmetic demands
//!   (`from.chain_seq + links.len()`), so a dropped or inserted link is a
//!   refusal and not a shorter stream;
//! * an unknown `schema_version` is refused **whole**.
//!
//! It does **not** prove a link's `merkle_root` is the root of anything. A root
//! is authenticated by re-deriving it from the checkpoint's own contents, which
//! live in the git side of the bundle; that check is
//! [`crate::integrity::verify_against_source`], against git as the authority.
//! Flipping a root inside a segment is therefore NOT detected here — stated
//! plainly because a reader who assumed otherwise would trust the wrong half.
//!
//! # All-or-nothing
//!
//! [`import_segment`] verifies the segment, then verifies it attaches to the
//! importing repository's own chain, and only then writes — the write loop
//! inside one transaction, so a failure at any link leaves zero rows. Nothing
//! is advanced on any failure. Applying is set-union: a link already present is
//! a no-op, so re-importing the same bundle converges.

use core::fmt::Write as _;

use rift_core::{Result, RiftError, Sha256Id};
use rift_hashchain::ChainLink;

use crate::{
    cursor::{head_of, parse_canonical_u64, suffix_after, suffix_after_by, StreamCursor},
    integrity::{read_stored_links, record_checkpoint_if_new, CheckpointRecord},
    CanonicalStore, Store, TxMode,
};

/// The chain segment's wire schema version. Readers accept
/// `1..=CHAIN_SEGMENT_SCHEMA_VERSION` and refuse the rest whole.
pub const CHAIN_SEGMENT_SCHEMA_VERSION: u32 = 1;

/// The header every encoded segment opens with.
const SEGMENT_HEADER: &str = "rift-chain-segment/";

/// The links between two cursors, with the cursors that bound them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ChainSegment {
    from: StreamCursor,
    to: StreamCursor,
    links: Vec<ChainLink>,
}

/// What an import did. Both numbers are facts about this import, not about the
/// bundle: `applied + already_present == links`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ImportReport {
    /// Links written by this import.
    pub applied: usize,
    /// Links the importing repository already held (set-union no-ops).
    pub already_present: usize,
    /// The importing repository's head cursor after the import.
    pub head: u64,
}

impl ChainSegment {
    /// The segment of `links` (a whole linkage-ordered chain) after `from`.
    ///
    /// # Errors
    /// Whatever [`suffix_after`] reports: `from` ahead of the chain, or naming
    /// a checkpoint the chain does not hold at that position.
    pub fn between(links: &[ChainLink], from: &StreamCursor) -> Result<Self> {
        let tail = suffix_after(links, from)?;
        Ok(Self {
            from: from.clone(),
            to: head_of(links),
            links: tail.to_vec(),
        })
    }

    /// The cursor this segment resumes from.
    #[must_use]
    pub fn from_cursor(&self) -> &StreamCursor {
        &self.from
    }

    /// The cursor a consumer holds after applying this segment.
    #[must_use]
    pub fn to_cursor(&self) -> &StreamCursor {
        &self.to
    }

    /// The links, in chain order.
    #[must_use]
    pub fn links(&self) -> &[ChainLink] {
        &self.links
    }

    /// Order and membership, per the module docs. Pure — no store, no git.
    ///
    /// # Errors
    /// [`RiftError::Integrity`] naming the position of the first break, never
    /// any payload.
    pub fn verify(&self) -> Result<()> {
        // (1) The count the cursors declare is the count carried.
        let expected_to = self
            .from
            .chain_seq()
            .checked_add(self.links.len() as u64)
            .ok_or_else(|| RiftError::integrity("chain segment: cursor arithmetic overflows"))?;
        if self.to.chain_seq() != expected_to {
            return Err(RiftError::integrity(format!(
                "chain segment: to_cursor is at {} but {} link(s) after {} lands at {expected_to}",
                self.to.chain_seq(),
                self.links.len(),
                self.from.chain_seq()
            )));
        }

        // (2) The first link attaches to from_cursor (or is genesis).
        let Some(first) = self.links.first() else {
            // An empty segment must not move the cursor.
            return if self.from == self.to {
                Ok(())
            } else {
                Err(RiftError::integrity(
                    "chain segment: an empty segment moved the cursor",
                ))
            };
        };
        match (self.from.is_origin(), first.prev) {
            (true, None) => {}
            (true, Some(_)) => {
                return Err(RiftError::integrity(
                    "chain segment: starts from the origin but its first link has a predecessor",
                ))
            }
            (false, None) => {
                return Err(RiftError::integrity(
                    "chain segment: resumes from a cursor but its first link is a genesis link",
                ))
            }
            (false, Some(prev)) => {
                if prev.to_hex() != self.from.checkpoint_id() {
                    return Err(RiftError::integrity(
                        "chain segment: the first link does not follow from_cursor",
                    ));
                }
            }
        }

        // (3) Internal linkage.
        for (i, pair) in self.links.windows(2).enumerate() {
            let (Some(a), Some(b)) = (pair.first(), pair.get(1)) else {
                continue;
            };
            if b.prev != Some(a.checkpoint_id) {
                return Err(RiftError::integrity(format!(
                    "chain segment: break at link {}: prev does not match the prior checkpoint",
                    i + 1
                )));
            }
        }

        // (4) to_cursor names the last link. Reached only with a non-empty
        // segment — the empty case returned above — so there is no default arm
        // here for a reader to mistake for a real branch.
        if let Some(last) = self.links.last() {
            if last.checkpoint_id.to_hex() != self.to.checkpoint_id() {
                return Err(RiftError::integrity(
                    "chain segment: to_cursor does not name the last link",
                ));
            }
        }
        Ok(())
    }

    /// The canonical text encoding. Identical segments encode byte-identically:
    /// fixed field order, lowercase hex, `-` for a genesis link's absent
    /// predecessor, one record per line, `\n` terminated.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{SEGMENT_HEADER}{CHAIN_SEGMENT_SCHEMA_VERSION}");
        let _ = writeln!(out, "from {}", self.from);
        let _ = writeln!(out, "to {}", self.to);
        for l in &self.links {
            let _ = writeln!(
                out,
                "link {} {} {}",
                l.checkpoint_id.to_hex(),
                l.merkle_root.to_hex(),
                l.prev.map_or_else(|| "-".to_string(), |p| p.to_hex())
            );
        }
        out
    }

    /// Read a segment from its text encoding. **Does not verify** — decoding and
    /// trusting are two steps on purpose, so a caller cannot get the second by
    /// accident.
    ///
    /// # Errors
    /// [`RiftError::Validity`] — reason only, never the input — for a missing or
    /// unknown header, a malformed record, or a trailing field.
    pub fn decode(text: &str) -> Result<Self> {
        // Canonicity is a READER rule here, exactly as it is for the hex case
        // and the header integer below: the encoder writes `\n`-terminated
        // lines, so a text that is not exactly that is a SECOND SPELLING of one
        // artifact. `str::lines()` accepts a missing final terminator and
        // silently strips `\r`, so a CRLF-transcoded segment decoded clean and
        // then re-encoded with LF — different bytes out than in, which is the
        // drift this format's canonicity rules exist to prevent, and it was
        // already happening between two implementations of one paragraph.
        if !text.ends_with('\n') || text.contains('\r') {
            return Err(RiftError::validity(
                "chain segment: text is not `\\n`-terminated line-oriented UTF-8",
            ));
        }
        let mut records = text.strip_suffix('\n').unwrap_or(text).split('\n');
        let header = records
            .next()
            .and_then(|h| h.strip_prefix(SEGMENT_HEADER))
            .ok_or_else(|| {
                RiftError::validity("chain segment: text does not open with `rift-chain-segment/`")
            })?;
        // ONE spelling per version, the same rule the cursor text uses. A bare
        // `u32::from_str` accepted `01` and `+1` here while `rift-cursor/01`
        // and `/+1` were refused two modules away — two texts naming one
        // artifact, disagreeing about what its name is.
        let version = parse_canonical_u64(header)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| {
                RiftError::validity(
                    "chain segment: header version is not a canonical base-10 integer",
                )
            })?;
        if version == 0 || version > CHAIN_SEGMENT_SCHEMA_VERSION {
            return Err(RiftError::validity(format!(
                "chain segment: unknown schema version {version} (this reader accepts \
                 1..={CHAIN_SEGMENT_SCHEMA_VERSION})"
            )));
        }

        let from = field(records.next(), "from")?;
        let to = field(records.next(), "to")?;
        let mut links = Vec::new();
        for line in records {
            let rest = line
                .strip_prefix("link ")
                .ok_or_else(|| RiftError::validity("chain segment: expected a `link ` record"))?;
            let mut parts = rest.split(' ');
            let (Some(id), Some(root), Some(prev), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return Err(RiftError::validity(
                    "chain segment: a link record is not `link <id> <root> <prev|->`",
                ));
            };
            links.push(ChainLink {
                checkpoint_id: lower_hex_id(id)?,
                merkle_root: lower_hex_id(root)?,
                prev: if prev == "-" {
                    None
                } else {
                    Some(lower_hex_id(prev)?)
                },
            });
        }
        Ok(Self { from, to, links })
    }
}

/// A 64-char **lowercase** hex id, as [`ChainSegment::encode`] writes it.
///
/// `Sha256Id::parse_hex` is deliberately case-insensitive — it is the shared
/// id parser and other formats do carry uppercase — but `to_hex` is lowercase,
/// so an uppercase spelling here was accepted and then silently re-encoded.
/// That is one artifact with two names, and the re-encoding is what makes it
/// dangerous rather than merely untidy: a byte-compared segment stops matching
/// the bytes it was read from.
///
/// The refusal names the reason and never the input, like every refusal in this
/// module: a decoder that echoes what it refused is a decoder that logs
/// attacker-chosen bytes.
fn lower_hex_id(s: &str) -> Result<Sha256Id> {
    if s.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err(RiftError::validity(
            "chain segment: link ids must be lowercase hex",
        ));
    }
    Sha256Id::parse_hex(s)
}

/// One `<tag> <cursor>` header line.
fn field(line: Option<&str>, tag: &str) -> Result<StreamCursor> {
    let text = line
        .and_then(|l| l.strip_prefix(tag))
        .and_then(|l| l.strip_prefix(' '))
        .ok_or_else(|| RiftError::validity(format!("chain segment: missing `{tag}` line")))?;
    StreamCursor::parse(text)
}

/// The segment of this projection's chain after `from`.
///
/// # Errors
/// Read/topology errors from [`read_stored_links`], or a cursor that does not
/// belong to this chain.
pub async fn export_segment(
    store: &impl CanonicalStore,
    from: &StreamCursor,
) -> Result<ChainSegment> {
    ChainSegment::between(&read_stored_links(store).await?, from)
}

/// **The one attach rule** (closed 2026-08-29): does `segment` attach to the
/// chain `local` names, in order?
///
/// Pure and generic over the local chain's representation, so the projection's
/// stored links ([`import_segment`]) and the substrate's chain ref
/// (`rift bundle import`, which is refs-only and reads
/// `rift_git::chain::read_order`) answer the question with ONE body instead of
/// two that agree today. Three arms, every one fail-closed:
///
/// 1. **An incremental segment cannot bootstrap.** A segment that resumes from
///    a non-origin cursor is refused by a repository holding NO chain at all —
///    this is the case the blind bundle review measured ("the incremental bundle
///    imported cleanly into a repo holding no chain") and the ref side had no
///    check for until now. Export from origin to seed a fresh repository.
/// 2. **`from_cursor` names a held position** ([`suffix_after_by`]'s two
///    fail-closed arms: ahead of the chain, or a different checkpoint at that
///    position).
/// 3. **Where the two overlap they are the same chain** — the origin cursor
///    carries no identity, so without this a segment exported from origin
///    would attach to every repository and write a second genesis.
///
/// `id_of` names a local item by its checkpoint id (lowercase hex).
///
/// # Errors
/// [`RiftError::Integrity`] naming the position or the arm — never any payload.
pub fn attaches_by<T>(
    local: &[T],
    segment: &ChainSegment,
    id_of: impl Fn(&T) -> String,
) -> Result<()> {
    if local.is_empty() && !segment.from_cursor().is_origin() {
        return Err(RiftError::integrity(format!(
            "chain segment: an incremental segment cannot bootstrap a repository with no chain \
             (the cursor is ahead of a chain of 0 checkpoint(s), chain_seq {}); export from origin",
            segment.from_cursor().chain_seq()
        )));
    }
    let local_tail = suffix_after_by(local, segment.from_cursor(), &id_of)?;
    for (i, (mine, theirs)) in local_tail.iter().zip(segment.links()).enumerate() {
        if id_of(mine) != theirs.checkpoint_id.to_hex() {
            return Err(RiftError::integrity(format!(
                "chain segment: diverges from this repository's chain at link {}",
                i + 1
            )));
        }
    }
    Ok(())
}

/// Apply a segment to this projection, **all or nothing**.
///
/// Verifies the segment, then verifies it attaches to the local chain, and only
/// then writes — the whole write loop inside ONE transaction, so a failure at
/// link k leaves zero rows, never k−1 of them
/// (`a_mid_loop_write_failure_leaves_zero_rows` is the plant that holds this).
/// Re-importing the same segment converges (set-union).
///
/// The bound is [`Store`] rather than [`CanonicalStore`] because the write loop
/// needs a transaction, which lives on the connection; every [`Store`] is a
/// [`CanonicalStore`] through the blanket impl, so callers lose nothing. The
/// reference engine serializes all statements on one shared connection
/// ([`crate::StoreTx`]'s own contract), which is what lets the loop's inserts
/// ride the open transaction.
///
/// # Why `bins/rift`'s `cmd_import` does not call this (blind bundle review,
/// defects (a)/(b), a considered decision rather than an oversight)
///
/// This is the crate's tested, spec-compliant reference implementation of
/// the chain-segment attach rule (`docs/sync-format.md`'s chain-segment
/// entry), and every case above exercises it. It is deliberately NOT the
/// bundle importer's first writer of checkpoint rows, because a
/// [`ChainLink`] carries no strategy — only identity and linkage — so a
/// caller here must supply one label for a WHOLE segment, and
/// `record_checkpoint_if_new` is insert-or-ignore: a second writer with the
/// checkpoint's real strategy (`rift rebuild`'s substrate walk, the only
/// place that reads a checkpoint's own `metadata.json`) would find the id
/// already present and be silently dropped — canonical tables have no
/// `UPDATE` to correct it afterwards (§1 invariant 1).
/// `a_second_writer_with_the_true_strategy_is_silently_dropped_after_the_first`
/// measures the MECHANISM, directly against this function (it cannot see
/// whether `cmd_import` performs the wiring, since it never calls
/// `cmd_import`) — a verifier caught that gap. The claim that `cmd_import`
/// itself stays unwired is pinned separately, through the real binary:
/// `bins/rift/tests/bundle_e2e.rs`'s
/// `a_bundle_round_trips_into_a_fresh_repository_and_re_imports_idempotently`
/// reads the imported checkpoint's `strategy` back after `rebuild` and
/// asserts it is `rebuild`'s real value, never a placeholder — verified BY
/// MUTATION to red under the literal "call `import_segment` from
/// `cmd_import`" fix (the fixing commit's message quotes the panic).
/// `bins/rift`'s bundle import therefore stays refs-only and `rift rebuild`
/// remains the sole writer of checkpoint rows; this function stays ready for
/// a future caller that either supplies real per-link metadata or accepts a
/// placeholder strategy on purpose.
///
/// # Errors
/// [`RiftError::Integrity`] if the segment is inconsistent or does not attach,
/// [`RiftError::Storage`] if a write fails; nothing is written in any case.
pub async fn import_segment(
    store: &impl Store,
    segment: &ChainSegment,
    strategy: &str,
    at: i64,
) -> Result<ImportReport> {
    segment.verify()?;
    let local = read_stored_links(store).await?;
    // The one attach rule (identity): the segment's start is a position this
    // repository holds, and where the two overlap they name the same
    // checkpoints.
    attaches_by(&local, segment, |l| l.checkpoint_id.to_hex())?;
    // The store additionally requires the WHOLE link to agree — root and
    // predecessor, not just the id — because a stored link is what `verify`
    // re-derives against.
    let local_tail = suffix_after_by(&local, segment.from_cursor(), |l| l.checkpoint_id.to_hex())?;
    for (i, (mine, theirs)) in local_tail.iter().zip(segment.links()).enumerate() {
        if mine != theirs {
            return Err(RiftError::integrity(format!(
                "chain segment: diverges from this repository's chain at link {}",
                i + 1
            )));
        }
    }

    let mut applied = 0_usize;
    let mut already_present = 0_usize;
    if !segment.links().is_empty() {
        // ONE transaction around the whole loop. Without it, `?` on a failed
        // insert returned with every earlier link already durable — a
        // half-applied segment under a docstring that says all-or-nothing.
        // (An empty segment writes nothing, so it opens no transaction and an
        // empty import stays read-only.)
        let conn = store.conn().await?;
        let tx = conn.begin(TxMode::Deferred).await?;
        let mut failed: Option<RiftError> = None;
        for link in segment.links() {
            match record_checkpoint_if_new(
                store,
                &CheckpointRecord {
                    checkpoint_id: link.checkpoint_id,
                    strategy: strategy.to_owned(),
                    created_at: at,
                    merkle_root: link.merkle_root,
                    prev: link.prev,
                    schema_version: 1,
                },
            )
            .await
            {
                Ok(true) => applied += 1,
                Ok(false) => already_present += 1,
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            }
        }
        match failed {
            None => tx.commit().await?,
            Some(e) => {
                // The rollback is what makes the refusal all-or-nothing; its
                // own (already-reported-as-storage) failure must not mask the
                // write error being returned. A dropped-unfinished transaction
                // rolls back best-effort anyway.
                let _ = tx.rollback().await;
                return Err(e);
            }
        }
    }
    let head = head_of(&read_stored_links(store).await?).chain_seq();
    Ok(ImportReport {
        applied,
        already_present,
        head,
    })
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;
    use crate::store::{Store, StoreConn, StoreDescriptor, StoreTx};
    use crate::{Row, SqliteStore, TxMode, Value};

    fn id_of(i: u8, tag: u8) -> Sha256Id {
        let mut b = [tag; 32];
        b[0] = i.wrapping_add(1);
        Sha256Id::from_bytes(b)
    }

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

    fn seeded(links: &[ChainLink]) -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        for (i, l) in links.iter().enumerate() {
            block(record_checkpoint_if_new(
                &s,
                &CheckpointRecord {
                    checkpoint_id: l.checkpoint_id,
                    strategy: "manual-commit".into(),
                    created_at: i64::try_from(i).unwrap(),
                    merkle_root: l.merkle_root,
                    prev: l.prev,
                    schema_version: 1,
                },
            ))
            .unwrap();
        }
        s
    }

    #[test]
    fn a_round_trip_lands_the_same_chain_in_a_fresh_projection() {
        // The reviewed shape, minus the git-bundle payload: export → import into an
        // empty projection ⇒ identical links, identical head cursor.
        let source = seeded(&chain(5, 7));
        let seg = block(export_segment(&source, &StreamCursor::origin())).unwrap();
        seg.verify().unwrap();

        let fresh = SqliteStore::open_in_memory().unwrap();
        fresh.create_canonical_schema().unwrap();
        let report = block(import_segment(&fresh, &seg, "bundle-import", 1)).unwrap();
        assert_eq!(report.applied, 5);
        assert_eq!(report.already_present, 0);
        assert_eq!(report.head, 5);
        assert_eq!(
            block(read_stored_links(&fresh)).unwrap(),
            block(read_stored_links(&source)).unwrap()
        );
    }

    #[test]
    fn re_importing_converges_instead_of_duplicating() {
        let source = seeded(&chain(4, 7));
        let seg = block(export_segment(&source, &StreamCursor::origin())).unwrap();
        let fresh = SqliteStore::open_in_memory().unwrap();
        fresh.create_canonical_schema().unwrap();
        let first = block(import_segment(&fresh, &seg, "bundle-import", 1)).unwrap();
        let second = block(import_segment(&fresh, &seg, "bundle-import", 2)).unwrap();
        assert_eq!((first.applied, first.already_present), (4, 0));
        assert_eq!((second.applied, second.already_present), (0, 4));
        assert_eq!(second.head, 4);
        assert_eq!(block(read_stored_links(&fresh)).unwrap().len(), 4);
    }

    /// **Blind bundle review, defects (a)/(b): why `import_segment` stays
    /// unwired in `cmd_import`, MEASURED rather than assumed.** A `ChainLink`
    /// carries no strategy field — only `checkpoint_id`/`merkle_root`/`prev`
    /// — so any `import_segment` caller supplies exactly ONE label for the
    /// whole segment. `record_checkpoint_if_new` is `INSERT OR IGNORE`: a
    /// second call for an id that already exists is silently dropped, never
    /// merged, and canonical tables have no `UPDATE` to correct it after the
    /// fact (§1 invariant 1). `rift rebuild`'s substrate walk
    /// (`rift_shadow::artifact_bridge`) is the ONLY place that reads a
    /// checkpoint's own `metadata.json` to learn its real strategy — so if a
    /// bundle-import writer landed a placeholder label FIRST, rebuild's true
    /// value would arrive second and be silently dropped, permanently. That
    /// is the measured reason `cmd_import`'s own doc comment keeps it
    /// refs-only ("`rift rebuild` projects what a bundle brought in") rather
    /// than also calling `import_segment` here: this crate's reference
    /// implementation of the attach rule is exercised and correct (every
    /// test above), but it is not the right FIRST writer for a checkpoint's
    /// metadata, and nothing today gives it the real value to write.
    #[test]
    fn a_second_writer_with_the_true_strategy_is_silently_dropped_after_the_first() {
        use crate::Value;

        let source = seeded(&chain(2, 7));
        let seg = block(export_segment(&source, &StreamCursor::origin())).unwrap();

        let target = SqliteStore::open_in_memory().unwrap();
        target.create_canonical_schema().unwrap();
        let report = block(import_segment(&target, &seg, "bundle-import", 1)).unwrap();
        assert_eq!(report.applied, 2);

        // The substrate walk's own true strategy for the SAME checkpoint,
        // tried second -- exactly what `rift rebuild` would do if
        // `import_segment` had already run inside `cmd_import`.
        let link = seg.links().first().unwrap();
        let wrote_again = block(record_checkpoint_if_new(
            &target,
            &CheckpointRecord {
                checkpoint_id: link.checkpoint_id,
                strategy: "manual-commit".to_owned(),
                created_at: 2,
                merkle_root: link.merkle_root,
                prev: link.prev,
                schema_version: 1,
            },
        ))
        .unwrap();
        assert!(
            !wrote_again,
            "insert-or-ignore must report the row already present"
        );

        let rows = block(target.query(
            "SELECT strategy FROM checkpoint WHERE checkpoint_id = ?",
            &[Value::text(link.checkpoint_id.to_hex())],
        ))
        .unwrap();
        assert_eq!(
            rows.first().unwrap().get_text(0).unwrap(),
            "bundle-import",
            "the true strategy is unrecoverable once a placeholder writer lands first"
        );
    }

    #[test]
    fn an_incremental_segment_carries_only_what_the_cursor_has_not_seen() {
        let source = seeded(&chain(6, 7));
        let all = block(read_stored_links(&source)).unwrap();
        let mid = head_of(all.get(..2).unwrap());
        let seg = block(export_segment(&source, &mid)).unwrap();
        seg.verify().unwrap();
        assert_eq!(seg.links().len(), 4);
        assert_eq!(seg.from_cursor(), &mid);
        assert_eq!(seg.to_cursor().chain_seq(), 6);

        // And it applies onto a projection that already holds the first two.
        let partial = seeded(all.get(..2).unwrap());
        let report = block(import_segment(&partial, &seg, "bundle-import", 1)).unwrap();
        assert_eq!((report.applied, report.head), (4, 6));
    }

    #[test]
    fn encoding_is_canonical_and_round_trips() {
        let source = seeded(&chain(3, 7));
        let seg = block(export_segment(&source, &StreamCursor::origin())).unwrap();
        let text = seg.encode();
        assert!(text.starts_with("rift-chain-segment/1\n"));
        assert_eq!(text.lines().count(), 3 + 3);
        let decoded = ChainSegment::decode(&text).unwrap();
        assert_eq!(decoded, seg);
        assert_eq!(
            decoded.encode(),
            text,
            "encoding is a function of the value"
        );
    }

    #[test]
    fn an_unknown_segment_version_is_refused_whole() {
        let source = seeded(&chain(2, 7));
        let seg = block(export_segment(&source, &StreamCursor::origin())).unwrap();
        let newer = seg.encode().replacen(
            "rift-chain-segment/1",
            &format!("rift-chain-segment/{}", CHAIN_SEGMENT_SCHEMA_VERSION + 1),
            1,
        );
        let err = ChainSegment::decode(&newer).unwrap_err().to_string();
        assert!(err.contains("unknown schema version"), "{err}");
        assert!(ChainSegment::decode("rift-chain-segment/0\nfrom rift-cursor/1:0:\n").is_err());
        assert!(ChainSegment::decode("").is_err());
        assert!(ChainSegment::decode("rift-chain-segment/1\n").is_err());
    }

    /// One artifact, one name — the property the cursor text already held and
    /// this one did not (a planted defect the review left behind).
    ///
    /// Every row below decodes to the SAME segment as the canonical text under
    /// the old reader, which is exactly what makes it a defect rather than a
    /// curiosity: two byte strings naming one value, and for the uppercase rows
    /// the reader silently re-encoded to a third. The control is asserted first
    /// so a row that fails for some unrelated reason cannot pass as a refusal.
    #[test]
    fn a_segment_has_exactly_one_spelling() {
        let source = seeded(&chain(2, 7));
        let seg = block(export_segment(&source, &StreamCursor::origin())).unwrap();
        let text = seg.encode();
        // The control: this decodes, and it is what the rows below are
        // respellings OF.
        assert_eq!(ChainSegment::decode(&text).unwrap(), seg);

        for spelling in ["01", "+1", "1.0", " 1", "0x1", ""] {
            let respelt = text.replacen(
                "rift-chain-segment/1\n",
                &format!("rift-chain-segment/{spelling}\n"),
                1,
            );
            let err = ChainSegment::decode(&respelt)
                .expect_err(&format!("accepted version spelling {spelling:?}"))
                .to_string();
            assert!(
                err.contains("canonical") || err.contains("unknown schema version"),
                "the refusal must name the reason: {err}"
            );
            assert!(
                !err.contains(spelling) || spelling.is_empty(),
                "a refusal must not echo its input: {err}"
            );
        }

        // The same for the hex: accepted, then re-encoded lowercase, so the
        // segment no longer matched the bytes it was read from.
        let shouted = text
            .lines()
            .map(|l| {
                if l.starts_with("link ") {
                    l.to_ascii_uppercase().replace("LINK ", "link ")
                } else {
                    l.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let err = ChainSegment::decode(&format!("{shouted}\n"))
            .expect_err("accepted uppercase hex")
            .to_string();
        assert!(err.contains("lowercase hex"), "{err}");
        assert!(
            !err.contains("ABCD") && !err.to_lowercase().contains(&"0".repeat(8)),
            "a refusal must not echo its input: {err}"
        );
    }

    /// **The half of "one spelling" that two implementations had already
    /// disagreed about.** `str::lines()` strips `\r` and forgives a missing
    /// final terminator, so a CRLF-transcoded segment — the shape a mail
    /// client, a Windows checkout or a copy-paste produces — decoded clean and
    /// re-encoded with LF. The bytes out were not the bytes in, which is the
    /// exact drift the lowercase-hex and canonical-integer rules above refuse.
    /// An independent second implementation, written blind from the published
    /// format registry, refuses both; this reader now agrees with it.
    #[test]
    fn line_termination_is_part_of_the_one_spelling() {
        let source = seeded(&chain(3, 7));
        let seg = block(export_segment(&source, &StreamCursor::origin())).unwrap();
        let text = seg.encode();
        assert_eq!(ChainSegment::decode(&text).unwrap(), seg, "the control");

        for (why, respelt) in [
            ("CRLF", text.replace('\n', "\r\n")),
            ("a lone CR", text.replace('\n', "\r")),
            (
                "no final terminator",
                text.trim_end_matches('\n').to_owned(),
            ),
            ("empty text", String::new()),
        ] {
            let Err(err) = ChainSegment::decode(&respelt) else {
                panic!("{why}: accepted a second spelling of one segment");
            };
            let err = err.to_string();
            assert!(
                err.contains("line-oriented"),
                "{why}: the refusal must name the reason; got: {err}"
            );
        }

        // DECODE-THEN-ENCODE IS THE IDENTITY ON BYTES, which is the property
        // the format registry actually claims and the one `str::lines()` broke:
        // every text this reader ACCEPTS must re-encode to itself.
        let decoded = ChainSegment::decode(&text).unwrap();
        assert_eq!(decoded.encode(), text);
    }

    /// Each row is a tampered segment and the reason it must be refused. The
    /// point of the table is that ALL of them fail before any write — the
    /// all-or-nothing property is asserted on the store, not on the return
    /// value alone.
    #[test]
    fn every_tampered_segment_is_refused_and_advances_nothing() {
        let source = seeded(&chain(5, 7));
        let good = block(export_segment(&source, &StreamCursor::origin())).unwrap();
        let text = good.encode();

        let dropped = {
            let mut lines: Vec<&str> = text.lines().collect();
            lines.remove(4); // a middle link
            format!("{}\n", lines.join("\n"))
        };
        let reordered = {
            let mut lines: Vec<&str> = text.lines().collect();
            lines.swap(3, 4);
            format!("{}\n", lines.join("\n"))
        };
        let forged_id = text.replacen(&id_of(2, 7).to_hex(), &id_of(2, 9).to_hex(), 1);
        let forged_to = text.replacen(
            &format!("to {}", good.to_cursor()),
            &format!("to {}", StreamCursor::at(5, &id_of(9, 7).to_hex()).unwrap()),
            1,
        );
        let stretched_to = text.replacen(
            &format!("to {}", good.to_cursor()),
            &format!(
                "to {}",
                StreamCursor::at(6, good.to_cursor().checkpoint_id()).unwrap()
            ),
            1,
        );

        for (name, tampered) in [
            ("dropped link", dropped),
            ("reordered links", reordered),
            ("forged checkpoint id", forged_id),
            ("forged to_cursor id", forged_to),
            ("stretched to_cursor position", stretched_to),
        ] {
            let seg = ChainSegment::decode(&tampered)
                .unwrap_or_else(|e| panic!("{name} should decode, then fail verify: {e}"));
            assert!(seg.verify().is_err(), "{name} verified");

            let fresh = SqliteStore::open_in_memory().unwrap();
            fresh.create_canonical_schema().unwrap();
            assert!(block(import_segment(&fresh, &seg, "bundle-import", 1)).is_err());
            assert_eq!(
                block(read_stored_links(&fresh)).unwrap().len(),
                0,
                "{name}: a refused import wrote rows"
            );
        }
    }

    #[test]
    fn a_flipped_merkle_root_still_verifies_which_is_the_documented_limit() {
        // The module docs say a segment binds ORDER and MEMBERSHIP and does NOT
        // authenticate roots. That sentence is pinned here rather than left as
        // prose, so it cannot quietly become false in either direction: a root
        // is not part of the linkage, so nothing in `verify` looks at it, and a
        // reader who trusted a root because the segment verified would be
        // trusting the git side's job. Root authenticity is
        // `integrity::verify_against_source`, against git as the authority.
        //
        // IF THIS TEST EVER FAILS the segment has GAINED root authentication —
        // delete it, say so, and update the module docs and the sync-format
        // entry together.
        let source = seeded(&chain(3, 7));
        let seg = block(export_segment(&source, &StreamCursor::origin())).unwrap();
        let flipped = seg.encode().replacen(
            &Sha256Id::from_bytes([201; 32]).to_hex(),
            &Sha256Id::from_bytes([9; 32]).to_hex(),
            1,
        );
        let tampered = ChainSegment::decode(&flipped).unwrap();
        assert_ne!(tampered, seg, "the fixture did not actually change a root");
        assert!(tampered.verify().is_ok());
    }

    /// A store double whose checkpoint INSERTs fail once `budget` of them have
    /// run; every other call rides through to the real store.
    struct FailingStore {
        inner: SqliteStore,
        budget: usize,
        seen: Arc<AtomicUsize>,
    }
    /// The double's connection, sharing the store's insert counter.
    struct FailingConn {
        inner: Box<dyn StoreConn>,
        budget: usize,
        seen: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Store for FailingStore {
        async fn conn(&self) -> rift_core::Result<Box<dyn StoreConn>> {
            Ok(Box::new(FailingConn {
                inner: self.inner.conn().await?,
                budget: self.budget,
                seen: Arc::clone(&self.seen),
            }))
        }
        fn descriptor(&self) -> &StoreDescriptor {
            self.inner.descriptor()
        }
    }

    #[async_trait::async_trait]
    impl StoreConn for FailingConn {
        async fn execute(&self, sql: &str, params: &[Value]) -> rift_core::Result<u64> {
            if sql.starts_with("INSERT OR IGNORE INTO checkpoint") {
                let n = self.seen.fetch_add(1, Ordering::SeqCst) + 1;
                if n > self.budget {
                    return Err(RiftError::storage("planted mid-loop write failure"));
                }
            }
            self.inner.execute(sql, params).await
        }
        async fn execute_batch(&self, sql: &str) -> rift_core::Result<()> {
            self.inner.execute_batch(sql).await
        }
        async fn query(&self, sql: &str, params: &[Value]) -> rift_core::Result<Vec<Row>> {
            self.inner.query(sql, params).await
        }
        async fn begin(&self, mode: TxMode) -> rift_core::Result<Box<dyn StoreTx + '_>> {
            self.inner.begin(mode).await
        }
    }

    /// A fresh canonical projection behind the double.
    fn fixture_failing_store(budget: usize) -> FailingStore {
        let inner = SqliteStore::open_in_memory().unwrap();
        inner.create_canonical_schema().unwrap();
        FailingStore {
            inner,
            budget,
            seen: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// **Defect (d) of the blind bundle review, planted rather than argued:**
    /// the write loop is ONE transaction, so a failure at link k leaves ZERO
    /// rows — not k-1 of them. Before the fix this test reddens with exactly
    /// one surviving row: the double fails the SECOND checkpoint insert, and
    /// the first insert's row stayed behind, which is a half-applied segment
    /// wearing the module's "all or nothing" docstring.
    ///
    /// The double intercepts only the checkpoint INSERT; schema creation,
    /// reads, and the transaction itself ride through to the real store, so
    /// what is measured afterwards is the real store's row count.
    #[test]
    fn a_mid_loop_write_failure_leaves_zero_rows() {
        let source = seeded(&chain(3, 7));
        let seg = block(export_segment(&source, &StreamCursor::origin())).unwrap();

        // The plant: the first checkpoint insert succeeds, the second fails.
        let failing = fixture_failing_store(1);
        let err = block(import_segment(&failing, &seg, "bundle-import", 1))
            .expect_err("the planted failure must refuse the import")
            .to_string();
        assert!(
            err.contains("planted"),
            "the refusal must be the plant: {err}"
        );
        assert!(
            failing.seen.load(Ordering::SeqCst) >= 2,
            "the plant must have fired MID-loop (after a successful insert)"
        );
        assert_eq!(
            block(read_stored_links(&failing.inner)).unwrap().len(),
            0,
            "a refused import left rows behind: the write loop is not atomic"
        );

        // The control on the same double: an unreachable budget imports whole,
        // so the refusal above is the plant's and not the double's.
        let healthy = fixture_failing_store(usize::MAX);
        let report = block(import_segment(&healthy, &seg, "bundle-import", 1)).unwrap();
        assert_eq!((report.applied, report.head), (3, 3));
        assert_eq!(block(read_stored_links(&healthy.inner)).unwrap().len(), 3);
    }

    #[test]
    fn a_segment_from_a_different_chain_does_not_attach() {
        // Verifies fine on its own — it is a well-formed segment of SOME chain.
        // It is the attachment check that refuses it, and nothing is written.
        let theirs = seeded(&chain(3, 9));
        let seg = block(export_segment(&theirs, &StreamCursor::origin())).unwrap();
        seg.verify().unwrap();

        let mine = seeded(&chain(3, 7));
        assert!(block(import_segment(&mine, &seg, "bundle-import", 1)).is_err());
        assert_eq!(block(read_stored_links(&mine)).unwrap(), chain(3, 7));
    }

    /// **Blind bundle review, defect (c)'s exact production shape:** the
    /// assessment measured that "the incremental bundle (segment attests
    /// links 2-3) imported cleanly into a repo holding no chain at all" —
    /// which can only happen if `import_segment`'s own attach check never ran
    /// (defect (a): it has no `bins/` caller today). Called directly, on the
    /// reference implementation this crate ships, the SAME segment refuses
    /// against a truly empty projection: the origin-cursor escape in
    /// `suffix_after` cannot fire (this segment resumes from a NON-origin
    /// cursor), so the empty local chain is "ahead of" per the position
    /// arithmetic, and nothing is written.
    #[test]
    fn an_incremental_segment_does_not_attach_to_an_empty_projection() {
        let source = seeded(&chain(3, 7));
        let from = head_of(&chain(1, 7)); // position 1, mid-chain
        let seg = block(export_segment(&source, &from)).unwrap();
        assert_eq!(seg.links().len(), 2);
        seg.verify().unwrap();

        let empty = SqliteStore::open_in_memory().unwrap();
        empty.create_canonical_schema().unwrap();
        let err = block(import_segment(&empty, &seg, "bundle-import", 1))
            .expect_err("a resuming cursor must not attach to a chain of zero")
            .to_string();
        assert!(err.contains("ahead of a chain of 0"), "{err}");
        assert_eq!(block(read_stored_links(&empty)).unwrap().len(), 0);
    }

    /// **`attaches_by` is the ONE attach rule**, exercised on a chain given as
    /// bare ids (the ref-side shape `rift bundle import` reads through
    /// `rift_git::chain::read_order`) so the arms are asserted independently
    /// of the store. Every arm below is a distinct refusal or acceptance.
    #[test]
    fn attaches_by_is_the_one_attach_rule() {
        let ids = |links: &[ChainLink]| -> Vec<String> {
            links.iter().map(|l| l.checkpoint_id.to_hex()).collect()
        };
        let source = chain(3, 7);
        let from_origin = ChainSegment::between(&source, &StreamCursor::origin()).unwrap();
        let incremental = ChainSegment::between(&source, &head_of(&chain(1, 7))).unwrap();
        assert_eq!(incremental.links().len(), 2);

        // (1) an origin segment seeds an empty repository…
        attaches_by(&Vec::<String>::new(), &from_origin, Clone::clone).unwrap();
        // …an incremental one cannot bootstrap it.
        let err = attaches_by(&Vec::<String>::new(), &incremental, Clone::clone)
            .expect_err("an incremental segment must not bootstrap an empty chain")
            .to_string();
        assert!(err.contains("cannot bootstrap"), "{err}");
        assert!(err.contains("ahead of a chain of 0"), "{err}");

        // (2) from_cursor names a held position: the chain holds it → attaches
        //     (both when the segment overlaps and when it purely appends)…
        attaches_by(&ids(&source), &incremental, Clone::clone).unwrap();
        attaches_by(&ids(&chain(1, 7)), &incremental, Clone::clone).unwrap();
        // …ahead of the chain → refused…
        let ahead = ChainSegment::between(&source, &head_of(&chain(2, 7))).unwrap();
        let err = attaches_by(&ids(&chain(1, 7)), &ahead, Clone::clone)
            .expect_err("a cursor past the chain must be refused")
            .to_string();
        assert!(err.contains("ahead of a chain of 1"), "{err}");
        // …a different checkpoint at that position → refused.
        let err = attaches_by(&ids(&chain(3, 9)), &incremental, Clone::clone)
            .expect_err("a cursor naming a foreign checkpoint must be refused")
            .to_string();
        assert!(err.contains("different checkpoint"), "{err}");

        // (3) where the two overlap they are the same chain — the origin cursor
        //     carries no identity, so this arm is what stops a foreign genesis.
        let err = attaches_by(&ids(&chain(3, 9)), &from_origin, Clone::clone)
            .expect_err("a foreign chain must not attach from origin")
            .to_string();
        assert!(err.contains("diverges"), "{err}");
    }

    #[test]
    fn an_empty_segment_is_legal_and_moves_nothing() {
        let source = seeded(&chain(3, 7));
        let head = block(export_segment(&source, &StreamCursor::origin()))
            .unwrap()
            .to_cursor()
            .clone();
        let seg = block(export_segment(&source, &head)).unwrap();
        assert!(seg.links().is_empty());
        seg.verify().unwrap();
        let report = block(import_segment(&source, &seg, "bundle-import", 1)).unwrap();
        assert_eq!(
            (report.applied, report.already_present, report.head),
            (0, 0, 3)
        );
    }

    proptest::proptest! {
        /// Round-trip over every prefix cut: exporting from any cursor and
        /// importing into a projection holding exactly that prefix reconstructs
        /// the source chain — no gap, no duplicate.
        #[test]
        fn export_then_import_reconstructs_the_source(n in 1_u8..12, cut in 0_usize..12) {
            let links = chain(n, 7);
            let cut = cut.min(links.len());
            let source = seeded(&links);
            let from = head_of(links.get(..cut).unwrap_or_default());
            let seg = block(export_segment(&source, &from)).unwrap();
            seg.verify().unwrap();
            proptest::prop_assert_eq!(seg.links().len(), links.len() - cut);

            let target = seeded(links.get(..cut).unwrap_or_default());
            let report = block(import_segment(&target, &seg, "bundle-import", 1)).unwrap();
            proptest::prop_assert_eq!(report.applied, links.len() - cut);
            proptest::prop_assert_eq!(block(read_stored_links(&target)).unwrap(), links);
        }

        /// Encoding is deterministic and decoding is its inverse.
        #[test]
        fn encode_decode_is_the_identity(n in 0_u8..12, cut in 0_usize..12) {
            let links = chain(n, 7);
            let cut = cut.min(links.len());
            let source = seeded(&links);
            let from = head_of(links.get(..cut).unwrap_or_default());
            let seg = block(export_segment(&source, &from)).unwrap();
            let text = seg.encode();
            proptest::prop_assert_eq!(seg.encode(), text.clone());
            proptest::prop_assert_eq!(ChainSegment::decode(&text).unwrap(), seg);
            // …and the identity that matters ACROSS implementations is on
            // BYTES, not on the parsed value: a reader that accepts a text and
            // re-emits different bytes has given one artifact two names.
            proptest::prop_assert_eq!(
                ChainSegment::decode(&text).map(|s| s.encode()).unwrap(),
                text
            );
        }
    }
}
