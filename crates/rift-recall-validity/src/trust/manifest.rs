//! What a caller hands the manifest renderer: one entry per changed file, and the
//! five things an entry may say about it.
//!
//! A separate module because these types carry no wording of their own — every
//! sentence a model reads is written in `frames` — so a caller can build an entry
//! without touching the file the renderer gate reads whole.

/// What a manifest entry says about a changed file beyond naming it.
///
/// Five states and no sixth, because each one is a different act for the model
/// reading it: pull these lines, ask what changed, do not expect lines at all,
/// stop holding the file, or nothing beyond the fact that it moved. A state is
/// never inferred from the absence of another — a basis nobody could reach is
/// [`EntryDetail::Changed`], never a range list that happens to be empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryDetail<'a> {
    /// The changed places, in the numbering of the bytes a reader would get
    /// now, and how many lines the file grew by. Complete: everything not
    /// named is unchanged.
    ChangedAt {
        /// Every changed place, 1-based and inclusive at both ends.
        ranges: &'a [(u32, u32)],
        /// The net line movement, negative when the file shrank.
        shift: i64,
    },
    /// It changed and where is not known — a basis nobody could reach, or more
    /// places than an entry may name.
    Changed,
    /// One side is not lines, so there is no line number to name.
    Binary,
    /// It is gone from the worktree.
    Removed,
    /// Nothing beyond the path. The session holds no revision of this file, so
    /// there is nothing to diff it against and nothing to claim — the entry a
    /// manifest drawn from the baseline alone has always rendered.
    Unstated,
}

/// One file a manifest names: the path, the revision the session holds, and
/// what is true of the file now.
///
/// Plain borrowed data for the same reason [`HeldEntry`](super::HeldEntry) is: this renderer sits
/// below the ledger and the diff, and taking their types here would put an edge
/// into the crate graph for the sake of three fields.
#[derive(Clone, Copy, Debug)]
pub struct ManifestEntry<'a> {
    /// The path as the delta names it.
    pub path: &'a str,
    /// The content hash the session **holds** — 12 hex renders, 16 is the
    /// argument form of the pull verb. Empty (or anything that is not a hash)
    /// renders no revision at all, which is the honest reading of "the session
    /// holds nothing of this file".
    pub hash: &'a str,
    /// What is true of the file now.
    pub detail: EntryDetail<'a>,
}

impl<'a> ManifestEntry<'a> {
    /// An entry that names a path and claims nothing else — a delta the
    /// baseline reported and the session's own reads say nothing about.
    #[must_use]
    pub const fn path_only(path: &'a str) -> Self {
        Self {
            path,
            hash: "",
            detail: EntryDetail::Unstated,
        }
    }
}
