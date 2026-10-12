//! Where a file changed since the revision a context window still holds.
//!
//! One question, asked in three places: the per-turn manifest, which names the
//! changed ranges of a held file; the decline of a verification read, which
//! names them as the reason; and the pull verb, which serves their content.
//! Three answers computed three ways would be three sets of line numbers a
//! reader has to reconcile, so the computation lives here and each caller
//! renders it.
//!
//! Two properties are load-bearing.
//!
//! 1. **The same diff engine, over the same line splitting, as everything else
//!    in this tree.** [`rift_recall_ledger::hunks`] pins `imara-diff`'s
//!    Histogram algorithm and `byte_lines_with_terminator` precisely so that a
//!    range is a fact about the bytes rather than about which function
//!    happened to compute it; this module *calls* that module rather than
//!    reimplementing it, which is what makes the pin hold here too.
//! 2. **Exact or absent.** A range list that is complete may be named; one
//!    that is not — more changed places than an entry may carry, bytes that
//!    are not lines, a basis nobody can reach — is reported as such and the
//!    caller says "changed" without naming a line. Naming *some* of the
//!    changed places would tell a reader that everything unnamed is unchanged,
//!    which is the one thing this must never say.
//!
//! Reaching a basis opens a store, so **this module's basis lookup is not on
//! the `PreToolUse` gate path**. The gate answers a verification read from the
//! agent's own edit lines, which are a file, and allows the read when it has
//! none. The manifest and the pull verb call [`basis_bytes`](crate::held_diff::basis_bytes) on turns where a
//! store is already open.
//!
//! 3. **The ladder is ordered by exactness, not by cost.** A revision is
//!    reached as a git blob first and as a read-time snapshot
//!    ([`crate::basis_store`]) second, because a blob is the revision byte for
//!    byte while a snapshot passed the redaction gate on its way to disk. The
//!    second rung exists because the first cannot reach an agent's own
//!    uncommitted edit at all, which is exactly the revision a window is most
//!    likely to hold. A diff computed against a masked span is not a
//!    difference the repository made, so [`held_diff_of`] degrades the whole
//!    answer where one appears, and the verb that serves lines refuses to
//!    serve them.
//!
//! [`held_diff_of`]: crate::held_diff::held_diff_of

use rift_recall_ledger::hunks::{compute_hunks, is_binary, split_lines, Hunk};
use rift_recall_substrate::worktree::CurrentFile;
use rift_recall_substrate::SubstrateReader;
use rift_store::CanonicalStore;

/// The largest basis or current file this diffs, matching the gate path's own
/// inline-hash cap: past it the answer is "changed", which costs a saving and
/// never a wrong claim.
pub const MAX_DIFF_BYTES: usize = 1024 * 1024;

/// How many changed places one entry may name. Past this the entry says the
/// file changed and names none of them.
pub const MAX_ENTRY_RANGES: usize = 8;

/// How many files one turn may diff. The cap is the caller's to apply — it is
/// stated here because it belongs to the same budget as the two above.
pub const MAX_DIFFED_FILES: usize = 16;

/// What became of a file since the revision a window holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeldDiff {
    /// The bytes are the ones the window read. Nothing to say and nothing to
    /// re-read.
    Unchanged,
    /// The file changed in these places, in the numbering of the bytes a
    /// reader is about to see, and nowhere else. Between one and
    /// [`MAX_ENTRY_RANGES`] ranges, 1-based and inclusive at both ends.
    Changed {
        /// Every changed place, complete.
        ranges: Vec<(u32, u32)>,
        /// How many lines the file grew by, negative when it shrank.
        shift: i64,
    },
    /// The file changed in more places than an entry may name. The count is
    /// carried so a caller can say how many without naming any.
    Diffuse {
        /// How many changed places there are.
        places: usize,
        /// How many lines the file grew by, negative when it shrank.
        shift: i64,
    },
    /// One side is not lines — a run of bytes with a NUL in it. There is no
    /// line number to name and no diff worth reading.
    Binary,
    /// The revision the window holds could not be reached, so what changed is
    /// not known. Never a claim that nothing did.
    Unavailable,
}

impl HeldDiff {
    /// The changed places, when they are complete enough to name. Empty for
    /// every other answer, so a caller that renders ranges cannot accidentally
    /// render a partial list.
    #[must_use]
    pub fn ranges(&self) -> &[(u32, u32)] {
        match self {
            Self::Changed { ranges, .. } => ranges,
            _ => &[],
        }
    }

    /// The line movement, where one is known.
    #[must_use]
    pub const fn shift(&self) -> Option<i64> {
        match self {
            Self::Changed { shift, .. } | Self::Diffuse { shift, .. } => Some(*shift),
            _ => None,
        }
    }
}

/// The 1-based inclusive run of *current* lines one hunk covers, or `None`
/// when the current bytes have no line to name.
///
/// A pure deletion has no lines of its own in the new numbering (`new_len` is
/// zero), so it is named by the single line the deleted text now sits in front
/// of — clamped to the file, because a deletion at the end sits in front of
/// nothing. That is a line a reader is asked to look at, which is exactly what
/// a changed place is for; the alternative, a zero-width range, is not a range
/// a reader can read.
fn range_of(hunk: &Hunk, total_lines: u32) -> Option<(u32, u32)> {
    if total_lines == 0 {
        return None;
    }
    let lo = hunk.new_start.saturating_add(1).min(total_lines);
    let hi = hunk
        .new_start
        .saturating_add(hunk.new_len)
        .min(total_lines)
        .max(lo);
    Some((lo, hi))
}

/// Diff the revision a window holds against the bytes a reader would get now.
///
/// Pure: two byte slices in, one answer out, no clock, no store and no
/// filesystem. Every refusal below is the conservative direction — the caller
/// learns less than it hoped and never something untrue.
#[must_use]
pub fn held_diff(basis: &[u8], current: &[u8]) -> HeldDiff {
    if is_binary(basis) || is_binary(current) {
        return HeldDiff::Binary;
    }
    if basis.len() > MAX_DIFF_BYTES || current.len() > MAX_DIFF_BYTES {
        return HeldDiff::Unavailable;
    }
    if basis == current {
        return HeldDiff::Unchanged;
    }
    let basis_lines = split_lines(basis).len();
    let current_lines = split_lines(current).len();
    let shift = i64::try_from(current_lines).unwrap_or(i64::MAX)
        - i64::try_from(basis_lines).unwrap_or(i64::MAX);
    let total = u32::try_from(current_lines).unwrap_or(u32::MAX);
    let hunks = compute_hunks(basis, current);
    let ranges: Vec<(u32, u32)> = hunks
        .iter()
        .filter_map(|hunk| range_of(hunk, total))
        .collect();
    if ranges.is_empty() {
        // The bytes differ and no line of the current file can be named for
        // it: an emptied file, or a change only in a trailing newline. Saying
        // "changed" is the whole of what is known.
        return HeldDiff::Diffuse {
            places: hunks.len(),
            shift,
        };
    }
    if ranges.len() > MAX_ENTRY_RANGES {
        return HeldDiff::Diffuse {
            places: ranges.len(),
            shift,
        };
    }
    HeldDiff::Changed { ranges, shift }
}

/// The bytes of the revision `held_hash` names, through git.
///
/// The route is the one the exact resolvers already use: the
/// `content_sha256 → blob_oid` bridge this tree writes at ingest, then the
/// read-only substrate seam. Every step is an `Option` because every step can
/// honestly fail — a hash nothing mapped, a blob this repository no longer
/// has, a file that has since grown past the cap — and each failure is
/// [`HeldDiff::Unavailable`], never an assumption.
///
/// **Opens a store**, so it belongs to the turns on which one is already open
/// (the manifest, the pull verb) and never to the gate path.
pub async fn basis_bytes(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    held_hash: &str,
) -> Option<Vec<u8>> {
    let oid = rift_recall_ledger::blob_map::blob_oid_for(store, held_hash)
        .await
        .ok()
        .flatten()?;
    let bytes = reader.blob(&oid).ok().flatten()?;
    (bytes.len() <= MAX_DIFF_BYTES).then_some(bytes)
}

/// The bytes of the revision `held_hash` names **of one file**.
///
/// [`basis_bytes`] answers "which blob has these bytes"; this answers "which
/// blob has these bytes AS THIS FILE", and only the second question is safe to
/// ask on behalf of a caller who chose the hash. A content address is half a
/// name — `Handle::File`'s contract is that a resolver matches path *and*
/// address — so a fetch keyed on the hash alone lets a caller pair any address
/// it has seen with any path it may read, and be handed the other file's bytes
/// under this file's policy decision. The bridge is asked with the path bound,
/// and a blob recorded for some other file is refused here rather than diffed.
///
/// The bridge is the only relation asked, because it is the only one whose
/// `blob_oid` is a git object id: the revisions relation carries a content
/// address in that column on the ingest path this tree writes, and reading it
/// as an oid finds nothing at best. The bridge's row is keyed by content
/// address and keeps its first-seen path, so identical bytes first recorded at
/// another path are not reachable here — the caller is told the basis cannot
/// be reached, which costs a saving and never names a wrong line.
///
/// **Opens a store**, so it belongs to the turns on which one is already open
/// and never to the gate path.
pub async fn basis_bytes_for_path(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    path: &str,
    held_hash: &str,
) -> Option<Vec<u8>> {
    let rows = store
        .query(
            "SELECT blob_oid FROM n_blob_oid_map \
             WHERE content_sha256 = ?1 AND path = ?2 LIMIT 1",
            &[
                rift_store::Value::text(held_hash),
                rift_store::Value::text(path),
            ],
        )
        .await
        .ok()?;
    let oid = rows.first()?.get_text(0).ok()?;
    let bytes = reader.blob(&oid).ok().flatten()?;
    (bytes.len() <= MAX_DIFF_BYTES).then_some(bytes)
}

/// The file the caller named **as it is now**, with the address of those
/// bytes: the checkout is asked, not the projection.
///
/// This is the current side of every held-revision diff, and it has to be the
/// live file rather than the newest revision anyone recorded. A projection's
/// newest revision is the newest one a *checkpoint carries*; an edit made
/// after the last checkpoint is not in it, so diffing against it answers a
/// question about a file's past — and, when the caller happens to hold that
/// same revision, answers "you already have the current bytes" about a file
/// that has since moved. That is the one wrong answer this verb must never
/// give, because a caller believes it and skips the re-read.
///
/// Refused, with the reason, when the reader is not bound to a checkout, the
/// path is not a safe repo-relative file inside it, or the file is past the
/// diff ceiling: the caller then reads the file itself.
#[must_use]
pub fn current_worktree_file(reader: &impl SubstrateReader, path: &str) -> CurrentFile {
    rift_recall_substrate::current_file_of(reader, path, MAX_DIFF_BYTES as u64)
}

// ------------------------------------------------------- the basis ladder ---

/// Where a basis came from, which decides what may be said about the diff
/// computed against it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BasisSource {
    /// A git blob, reached through the `content_sha256 → blob_oid` bridge.
    /// The bytes are the revision byte for byte; git stores them in the clear
    /// and Rift wrote none of them.
    Blob,
    /// A read-time snapshot from the family's store
    /// ([`crate::basis_store`]). The bytes went through the redaction gate on
    /// the way to disk, so some of them may be Rift's placeholder rather than
    /// the repository's text.
    Snapshot,
}

/// The revision a window holds, with everything a caller needs to know about
/// how much of it is really the repository's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Basis {
    /// The bytes.
    pub bytes: Vec<u8>,
    /// The runs of lines a mask touched, 1-based and inclusive, in this
    /// basis's own numbering. Always empty for [`BasisSource::Blob`].
    pub redacted: Vec<(u32, u32)>,
    /// Which rung of the ladder answered.
    pub source: BasisSource,
}

impl Basis {
    /// A basis that is the revision byte for byte.
    #[must_use]
    pub const fn blob(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            redacted: Vec::new(),
            source: BasisSource::Blob,
        }
    }

    /// Whether `lo..=hi` (1-based, inclusive) touches a masked run.
    #[must_use]
    fn touches_mask(&self, lo: u32, hi: u32) -> bool {
        self.redacted.iter().any(|&(a, b)| lo <= b && a <= hi)
    }
}

/// A diff against a basis, and whether any of it rests on a mask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedactedDiff {
    /// What may be said. Already degraded to [`HeldDiff::Diffuse`] when
    /// `tainted`, so a caller that only renders line numbers is correct
    /// without reading the flag.
    pub diff: HeldDiff,
    /// At least one changed place rests on lines the gate masked in the
    /// basis. What a caller that serves **content** owes such a diff is
    /// counts, never lines: the mask is a difference Rift wrote, so the
    /// "changed" lines it produces are the *live* bytes of a span that was
    /// redacted on the way to storage, and serving them would hand back
    /// exactly what the gate took out.
    pub tainted: bool,
}

/// [`held_diff`] against a basis that may carry masked spans.
///
/// A hunk whose **basis** lines touch a masked run is not a difference this
/// can name, and one such hunk taints the whole answer: naming the rest would
/// tell a reader that everything unnamed is unchanged, which is the one thing
/// this module must never say. The degraded answer is
/// [`HeldDiff::Diffuse`] — the file changed, in this many places, and here is
/// the net shift — which is what every caller already renders as `(changed)`.
///
/// A blob basis carries no masked runs, so this is exactly [`held_diff`] for
/// the rung that has always existed.
#[must_use]
pub fn held_diff_of(basis: &Basis, current: &[u8]) -> RedactedDiff {
    let diff = held_diff(&basis.bytes, current);
    if basis.redacted.is_empty() {
        return RedactedDiff {
            diff,
            tainted: false,
        };
    }
    let (places, shift) = match &diff {
        HeldDiff::Changed { ranges, shift } => (ranges.len(), *shift),
        // Nothing was named, so nothing can rest on a mask: `Unchanged` means
        // the bytes are equal (a masked basis equal to the live file is a
        // file that contains the placeholder), and the other three name no
        // line at all.
        _ => {
            return RedactedDiff {
                diff,
                tainted: false,
            }
        }
    };
    // The hunks again, for their OLD sides: `held_diff` keeps only the new
    // numbering, and the question here is which lines of the BASIS a change
    // was computed from.
    let tainted = compute_hunks(&basis.bytes, current).iter().any(|hunk| {
        let lo = hunk.old_start.saturating_add(1);
        let hi = hunk.old_start.saturating_add(hunk.old_len).max(lo);
        basis.touches_mask(lo, hi)
    });
    if !tainted {
        return RedactedDiff {
            diff,
            tainted: false,
        };
    }
    RedactedDiff {
        diff: HeldDiff::Diffuse { places, shift },
        tainted: true,
    }
}

/// The revision `held_hash` names, down the whole ladder: the git blob, then
/// the family's read-time snapshot, then nothing.
///
/// The order is not a preference, it is a correctness rule. A blob is the
/// revision byte for byte; a snapshot went through the redaction gate, so it
/// can only ever say less. Asking the cheaper, exact rung first means a
/// committed revision is answered exactly as it always was and the snapshot
/// answers only the revisions that had no basis at all — an agent's own
/// uncommitted edit, which is the case this rung exists for.
///
/// **Opens a store**, so it belongs to the turns on which one is already open
/// and never to the gate path.
pub async fn resolve_basis(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    snapshots: Option<&crate::basis_store::BasisStore>,
    held_hash: &str,
) -> Option<Basis> {
    if let Some(bytes) = basis_bytes(store, reader, held_hash).await {
        return Some(Basis::blob(bytes));
    }
    from_snapshot(snapshots?.get(held_hash)?)
}

/// The same ladder with the path bound — the shape a caller that took the hash
/// from its own request must use (see [`basis_bytes_for_path`]).
///
/// **Opens a store**, so it belongs to the turns on which one is already open
/// and never to the gate path.
pub async fn resolve_basis_for_path(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    snapshots: Option<&crate::basis_store::BasisStore>,
    path: &str,
    held_hash: &str,
) -> Option<Basis> {
    if let Some(bytes) = basis_bytes_for_path(store, reader, path, held_hash).await {
        return Some(Basis::blob(bytes));
    }
    from_snapshot(snapshots?.get_for_path(path, held_hash)?)
}

/// One snapshot as a basis, refused where it is past the diff ceiling.
fn from_snapshot(snapshot: crate::basis_store::Snapshot) -> Option<Basis> {
    (snapshot.bytes.len() <= MAX_DIFF_BYTES).then_some(Basis {
        bytes: snapshot.bytes,
        redacted: snapshot.redacted_lines,
        source: BasisSource::Snapshot,
    })
}

/// [`held_diff`] against a basis fetched down the whole ladder — the question
/// in one call for the callers that have a store open and render line
/// numbers.
///
/// The answer is already degraded where it rests on a mask, so a caller that
/// renders ranges cannot render one it should not. A caller that serves the
/// *content* of those ranges asks [`resolve_basis_for_path`] and
/// [`held_diff_of`] and reads the taint for itself.
pub async fn held_diff_against_basis(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    snapshots: Option<&crate::basis_store::BasisStore>,
    held_hash: &str,
    current: &[u8],
) -> HeldDiff {
    match resolve_basis(store, reader, snapshots, held_hash).await {
        Some(basis) => held_diff_of(&basis, current).diff,
        None => HeldDiff::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use rift_recall_substrate::MemorySubstrate;

    use super::*;

    /// The pin, asserted rather than assumed: the ranges this module names are
    /// the changed-hunk module's own `new_start`/`new_len`, mapped into 1-based
    /// inclusive line numbers. Computed here from `compute_hunks` directly, so
    /// a change to either side's mapping breaks this.
    #[test]
    fn held_diff_ranges_equal_the_hunks_module_on_a_fixture() {
        let basis = b"alpha\nbravo\ncharlie\ndelta\necho\nfoxtrot\n";
        let current = b"alpha\nBRAVO\ncharlie\ndelta\nECHO\nextra\nfoxtrot\n";
        let total = u32::try_from(split_lines(current).len()).unwrap();
        let expected: Vec<(u32, u32)> = compute_hunks(basis, current)
            .iter()
            .map(|h| {
                (
                    h.new_start + 1,
                    (h.new_start + h.new_len).max(h.new_start + 1).min(total),
                )
            })
            .collect();
        assert!(expected.len() >= 2, "the fixture has two changed places");
        let diff = held_diff(basis, current);
        assert_eq!(diff.ranges(), expected.as_slice(), "{diff:?}");
        assert_eq!(diff.shift(), Some(1));
        // And the concrete numbers, so the fixture is readable without running
        // the diff engine in one's head.
        assert_eq!(diff.ranges(), [(2, 2), (5, 6)]);
    }

    #[test]
    fn identical_bytes_are_unchanged_and_name_nothing() {
        let diff = held_diff(b"a\nb\n", b"a\nb\n");
        assert_eq!(diff, HeldDiff::Unchanged);
        assert!(diff.ranges().is_empty());
    }

    #[test]
    fn a_side_that_is_not_lines_is_binary() {
        assert_eq!(held_diff(b"a\nb\n", b"a\x00b"), HeldDiff::Binary);
        assert_eq!(held_diff(b"a\x00b", b"a\nb\n"), HeldDiff::Binary);
    }

    /// More changed places than an entry may name is reported as such, and the
    /// range list stays empty: a truncated list would tell a reader that
    /// everything it does not mention is unchanged.
    #[test]
    fn more_changed_places_than_may_be_named_names_none_of_them() {
        let basis: Vec<u8> = (0..40)
            .flat_map(|n| format!("line {n}\n").into_bytes())
            .collect();
        let current: Vec<u8> = (0..40)
            .flat_map(|n| {
                if n % 3 == 0 {
                    format!("changed {n}\n").into_bytes()
                } else {
                    format!("line {n}\n").into_bytes()
                }
            })
            .collect();
        let diff = held_diff(&basis, &current);
        let HeldDiff::Diffuse { places, shift } = diff else {
            panic!("{diff:?}");
        };
        assert!(places > MAX_ENTRY_RANGES, "{places}");
        assert_eq!(shift, 0);
        assert!(held_diff(&basis, &current).ranges().is_empty());
    }

    /// A file that grew past the cap is not diffed. The answer is the one that
    /// costs a saving rather than the one that costs the budget.
    #[test]
    fn a_file_past_the_cap_is_unavailable_not_diffed() {
        let big = vec![b'x'; MAX_DIFF_BYTES + 1];
        assert_eq!(held_diff(b"a\n", &big), HeldDiff::Unavailable);
        assert_eq!(held_diff(&big, b"a\n"), HeldDiff::Unavailable);
    }

    /// A deletion has no line of its own in the new numbering, so it names the
    /// line it now sits in front of — and at the end of a file, the last line.
    #[test]
    fn a_deletion_names_the_line_it_sits_in_front_of() {
        assert_eq!(held_diff(b"a\nb\nc\n", b"a\nc\n").ranges(), [(2, 2)]);
        assert_eq!(held_diff(b"a\nb\nc\n", b"a\nb\n").ranges(), [(2, 2)]);
    }

    /// An emptied file has no line to name at all: the honest answer says the
    /// file changed and stops.
    #[test]
    fn an_emptied_file_names_no_line() {
        let diff = held_diff(b"a\nb\n", b"");
        assert!(matches!(diff, HeldDiff::Diffuse { .. }), "{diff:?}");
        assert_eq!(diff.shift(), Some(-2));
    }

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime")
            .block_on(f)
    }

    /// A recall store with the content→blob bridge the ingest writes.
    fn store_mapping(content_sha256: &str, blob_oid: &str) -> rift_store::SqliteStore {
        let store = rift_store::SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&store, &rift_store::recall_migrations()).unwrap();
        block(rift_recall_ledger::blob_map::persist_blob_oid_map(
            &store,
            &[rift_recall_substrate::worktree::WorktreeBlob {
                path: "src.rs".into(),
                content_sha256: content_sha256.to_owned(),
                blob_oid: blob_oid.to_owned(),
            }],
            0,
        ))
        .unwrap();
        store
    }

    /// The whole route, end to end: a held hash, the bridge this tree writes
    /// at ingest, a blob the substrate seam hands back, and the ranges the
    /// caller renders.
    #[test]
    fn a_basis_reached_through_the_blob_bridge_yields_the_changed_ranges() {
        let basis = b"a\nb\nc\n";
        let held = rift_recall_substrate::worktree::sha256_hex(basis);
        let oid = rift_recall_substrate::worktree::git_blob_oid(basis);
        let store = store_mapping(&held, &oid);
        let reader = MemorySubstrate::default().with_blob(&oid, basis);
        assert_eq!(
            block(basis_bytes(&store, &reader, &held)).as_deref(),
            Some(basis.as_slice())
        );
        let diff = block(held_diff_against_basis(
            &store,
            &reader,
            None,
            &held,
            b"a\nB\nc\n",
        ));
        assert_eq!(diff.ranges(), [(2, 2)], "{diff:?}");
    }

    /// The path is part of the question. The same bytes fetched for the file
    /// they were recorded against come back; fetched for another file they do
    /// not, even though the row and the blob are both right there.
    #[test]
    fn a_blob_recorded_for_another_file_is_not_this_file_s_basis() {
        let basis = b"a\nb\nc\n";
        let held = rift_recall_substrate::worktree::sha256_hex(basis);
        let oid = rift_recall_substrate::worktree::git_blob_oid(basis);
        // `store_mapping` records the row under `src.rs`.
        let store = store_mapping(&held, &oid);
        let reader = MemorySubstrate::default().with_blob(&oid, basis);
        assert_eq!(
            block(basis_bytes_for_path(&store, &reader, "src.rs", &held)).as_deref(),
            Some(basis.as_slice()),
            "its own file"
        );
        assert_eq!(
            block(basis_bytes_for_path(&store, &reader, "other.rs", &held)),
            None,
            "another file's blob is not reachable under this file's name"
        );
    }

    /// A hash nothing mapped, and a hash mapped to a blob this repository no
    /// longer holds, are both "not known", never "unchanged".
    #[test]
    fn a_basis_nothing_maps_or_nothing_holds_is_unavailable() {
        let basis = b"a\nb\nc\n";
        let held = rift_recall_substrate::worktree::sha256_hex(basis);
        let oid = rift_recall_substrate::worktree::git_blob_oid(basis);
        let empty = rift_store::SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&empty, &rift_store::recall_migrations()).unwrap();
        let reader = MemorySubstrate::default().with_blob(&oid, basis);
        assert_eq!(
            block(held_diff_against_basis(
                &empty, &reader, None, &held, b"a\n"
            )),
            HeldDiff::Unavailable,
            "no row maps this revision"
        );
        let store = store_mapping(&held, &oid);
        let forgotten = MemorySubstrate::default();
        assert_eq!(
            block(held_diff_against_basis(
                &store, &forgotten, None, &held, b"a\n"
            )),
            HeldDiff::Unavailable,
            "the blob is gone"
        );
    }

    // ------------------------------------------------- the second rung ---

    /// A store holding one snapshot of `bytes` at `path`, sealed the one way
    /// bytes are ever sealed.
    fn snapshot_store(
        dir: &std::path::Path,
        path: &str,
        raw: &[u8],
    ) -> crate::basis_store::BasisStore {
        let store = crate::basis_store::BasisStore::for_family(dir);
        let out = rift_shadow_redact::redact(&rift_core::SecretBytes::new(raw.to_vec()))
            .expect("the gate clears this fixture");
        let sealed = out.redacted;
        let masked: Vec<(u32, u32)> = rift_recall_ledger::hunks::split_lines(raw)
            .iter()
            .zip(rift_recall_ledger::hunks::split_lines(sealed.as_slice()))
            .enumerate()
            .filter(|(_, (a, b))| **a != *b)
            .map(|(i, _)| {
                let n = u32::try_from(i + 1).unwrap();
                (n, n)
            })
            .collect();
        store
            .put(
                &sealed,
                &crate::basis_store::SnapshotMeta {
                    schema_version: crate::basis_store::SNAPSHOT_SCHEMA_VERSION,
                    content_sha256: rift_recall_substrate::worktree::sha256_hex(raw),
                    path: path.to_owned(),
                    redacted_sha256: sealed.redacted_sha256().to_hex(),
                    policy_fingerprint: "b3:fixture".to_owned(),
                    rule_packs: sealed.provenance().rule_pack_versions.clone(),
                    line_count: crate::basis_store::line_count(raw),
                    redacted_lines: masked,
                    captured_at_ms: 1,
                },
            )
            .expect("the snapshot lands");
        store
    }

    /// ★ The gap this rung exists to close: a revision that was **never
    /// committed** has no blob, so the ladder's first rung answers nothing —
    /// and the snapshot answers it exactly.
    #[test]
    fn an_uncommitted_revision_is_reached_through_its_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let basis = b"alpha\nbravo\ncharlie\n";
        let held = rift_recall_substrate::worktree::sha256_hex(basis);
        // An empty bridge and an empty substrate: nothing git has ever seen.
        let store = rift_store::SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&store, &rift_store::recall_migrations()).unwrap();
        let reader = MemorySubstrate::default();
        let snapshots = snapshot_store(tmp.path(), "src/a.rs", basis);

        assert_eq!(
            block(held_diff_against_basis(
                &store,
                &reader,
                None,
                &held,
                b"alpha\nBRAVO\ncharlie\n"
            )),
            HeldDiff::Unavailable,
            "without the rung, this is the old answer"
        );
        let diff = block(held_diff_against_basis(
            &store,
            &reader,
            Some(&snapshots),
            &held,
            b"alpha\nBRAVO\ncharlie\n",
        ));
        assert_eq!(diff.ranges(), [(2, 2)], "{diff:?}");
    }

    /// The order of the ladder, asserted rather than assumed: where a blob
    /// exists it answers, and the answer is marked as the exact rung.
    #[test]
    fn a_committed_revision_is_still_answered_by_its_blob() {
        let tmp = tempfile::tempdir().unwrap();
        let basis = b"a\nb\nc\n";
        let held = rift_recall_substrate::worktree::sha256_hex(basis);
        let oid = rift_recall_substrate::worktree::git_blob_oid(basis);
        let store = store_mapping(&held, &oid);
        let reader = MemorySubstrate::default().with_blob(&oid, basis);
        let snapshots = snapshot_store(tmp.path(), "src.rs", basis);
        let got = block(resolve_basis_for_path(
            &store,
            &reader,
            Some(&snapshots),
            "src.rs",
            &held,
        ))
        .unwrap();
        assert_eq!(got.source, BasisSource::Blob);
        assert!(got.redacted.is_empty());
    }

    /// ★ **A changed place that rests on a mask is never named.** The basis
    /// was redacted on its way to disk, so the line the gate masked differs
    /// from the live file whether or not the repository touched it — and the
    /// lines this would name are the live bytes of exactly that span. One
    /// such hunk degrades the whole answer to a count.
    #[test]
    fn a_change_that_rests_on_a_mask_is_a_count_and_never_a_line() {
        let tmp = tempfile::tempdir().unwrap();
        let secret = "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB";
        let raw = format!("fn one() {{}}\nconst T: &str = \"{secret}\";\nfn two() {{}}\n");
        let held = rift_recall_substrate::worktree::sha256_hex(raw.as_bytes());
        let snapshots = snapshot_store(tmp.path(), "src/a.rs", raw.as_bytes());
        let basis = block(resolve_basis_for_path(
            &rift_store::SqliteStore::open_in_memory().unwrap(),
            &MemorySubstrate::default(),
            Some(&snapshots),
            "src/a.rs",
            &held,
        ))
        .unwrap();
        assert_eq!(basis.source, BasisSource::Snapshot);
        assert_eq!(basis.redacted, [(2, 2)], "the gate masked line 2");
        assert!(
            !String::from_utf8_lossy(&basis.bytes).contains(secret),
            "and the raw secret is not on disk"
        );

        // The live file still holds the secret and nothing else changed. The
        // only "difference" is the mask.
        let answer = held_diff_of(&basis, raw.as_bytes());
        assert!(answer.tainted, "{answer:?}");
        assert!(
            matches!(answer.diff, HeldDiff::Diffuse { .. }),
            "a count, never a line: {answer:?}"
        );
        assert!(answer.diff.ranges().is_empty());

        // A real change elsewhere in the same file is taken down with it: a
        // partial list would say everything unnamed is unchanged.
        let edited = raw.replace("fn two() {}", "fn three() {}");
        let answer = held_diff_of(&basis, edited.as_bytes());
        assert!(answer.tainted);
        assert!(answer.diff.ranges().is_empty(), "{answer:?}");
    }

    /// A snapshot the gate did not change is the revision byte for byte, so
    /// it names line numbers exactly as a blob would.
    #[test]
    fn a_snapshot_with_no_mask_names_its_changed_places() {
        let tmp = tempfile::tempdir().unwrap();
        let raw = b"alpha\nbravo\ncharlie\ndelta\n";
        let held = rift_recall_substrate::worktree::sha256_hex(raw);
        let snapshots = snapshot_store(tmp.path(), "src/a.rs", raw);
        let basis = block(resolve_basis_for_path(
            &rift_store::SqliteStore::open_in_memory().unwrap(),
            &MemorySubstrate::default(),
            Some(&snapshots),
            "src/a.rs",
            &held,
        ))
        .unwrap();
        assert!(basis.redacted.is_empty());
        let answer = held_diff_of(&basis, b"alpha\nbravo\nCHARLIE\ndelta\n");
        assert!(!answer.tainted);
        assert_eq!(answer.diff.ranges(), [(3, 3)]);
    }

    /// A snapshot recorded for one file is not another file's basis, exactly
    /// as a blob recorded for one file is not.
    #[test]
    fn a_snapshot_recorded_for_another_file_is_not_this_files_basis() {
        let tmp = tempfile::tempdir().unwrap();
        let raw = b"a\nb\n";
        let held = rift_recall_substrate::worktree::sha256_hex(raw);
        let snapshots = snapshot_store(tmp.path(), "src/a.rs", raw);
        let store = rift_store::SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&store, &rift_store::recall_migrations()).unwrap();
        assert!(block(resolve_basis_for_path(
            &store,
            &MemorySubstrate::default(),
            Some(&snapshots),
            "src/other.rs",
            &held
        ))
        .is_none());
    }
}
