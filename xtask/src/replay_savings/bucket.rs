//! The buckets a repository-content call lands in, and their published
//! order.
//!
//! They are a module of their own because the key, the label and the order are
//! what make one run of this command comparable with another: a reader of the
//! table or of the document depends on them, so they are changed deliberately
//! and in one place.

/// Where one repository-content call landed.
///
/// Exactly one per call, and the order below is the order the table prints
/// them: what the gate would have acted on first, then what it would have
/// reached and left alone, then the reasons it would never have reached a
/// decision at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bucket {
    /// First sight of this `(path, revision)` in this context window. The
    /// read that can never be declined — the floor of any design that does
    /// not lie about what a window holds.
    FirstSight,
    /// A repeat wholly inside held coverage, which this build would decline.
    Declined,
    /// A repeat wholly inside coverage, under the line floor: decided and
    /// recorded, never acted on, because the reason frame would cost more
    /// than the lines it replaces.
    BelowFloor,
    /// A repeat wholly inside coverage the build reached a decision about and
    /// did not act on for any other reason.
    ObservedNotDeclined,
    /// The one allowed repeat of a call this window already declined — the
    /// rule that bounds a decline to a single extra round trip.
    RepeatAfterDecline,
    /// A repeat inside coverage the gate reached and had nothing exact to say
    /// about: it asks for exactly the lines an edit changed, or the frame
    /// would have had to name a site that did not survive the clamp. Either
    /// way the read runs.
    NoHonestFrame,
    /// A repeat of a revision this window has read, overlapping held coverage
    /// without being inside it.
    RepeatOverlapping,
    /// A repeat of a revision this window has read, over lines it does not
    /// hold.
    RepeatDisjoint,
    /// The slice is past the size a delivery can still be vouched for, and
    /// the result stated nothing that could stand in for density, so the gate
    /// records nothing and decides nothing.
    OverCreditBound,
    /// The slice is past that size and the client said outright how much of
    /// the file it handed over, so the window is credited from the statement
    /// one hook after the call. The read still runs — nothing past the
    /// ceiling is ever declined — but it now leaves coverage behind for the
    /// next read of the same revision to be decided against.
    CreditedFromStatement,
    /// The client wrote the result to a file; the window holds a pointer and a
    /// preview, never the lines.
    SpilledToAFile,
    /// A recognised read whose span no artifact pins — a decorated whole-file
    /// print, a default-window `head`.
    UnknownSpan,
    /// A read tool call outside the pinned argument set — pages of a document,
    /// cells of a notebook, an argument added after the pin was taken.
    NotReadShaped,
    /// No file on disk and no whole-file write to take a line count from.
    NoLineCount,
    /// The client's own identical-repeat guard answered the call.
    ClientDedup,
    /// The transcript holds the call and no result for it.
    NoResultRecorded,
    /// A name or content search: a repository-content channel the window
    /// journal never decides about.
    Search,
}

impl Bucket {
    /// Every bucket, in the order the table prints them.
    pub const ALL: [Self; 17] = [
        Self::FirstSight,
        Self::Declined,
        Self::BelowFloor,
        Self::ObservedNotDeclined,
        Self::RepeatAfterDecline,
        Self::NoHonestFrame,
        Self::RepeatOverlapping,
        Self::RepeatDisjoint,
        Self::OverCreditBound,
        Self::CreditedFromStatement,
        Self::SpilledToAFile,
        Self::UnknownSpan,
        Self::NotReadShaped,
        Self::NoLineCount,
        Self::ClientDedup,
        Self::NoResultRecorded,
        Self::Search,
    ];

    /// The key this bucket is published under, which a script may depend on.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::FirstSight => "first_sight",
            Self::Declined => "declined",
            Self::BelowFloor => "below_floor",
            Self::ObservedNotDeclined => "observed_not_declined",
            Self::RepeatAfterDecline => "repeat_after_decline",
            Self::NoHonestFrame => "no_honest_frame",
            Self::RepeatOverlapping => "repeat_overlapping",
            Self::RepeatDisjoint => "repeat_disjoint",
            Self::OverCreditBound => "over_credit_bound",
            Self::CreditedFromStatement => "credited_from_statement",
            Self::SpilledToAFile => "spilled_to_a_file",
            Self::UnknownSpan => "unknown_span",
            Self::NotReadShaped => "not_read_shaped",
            Self::NoLineCount => "no_line_count",
            Self::ClientDedup => "client_dedup",
            Self::NoResultRecorded => "no_result_recorded",
            Self::Search => "search",
        }
    }

    /// The sentence the table prints for this bucket.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::FirstSight => "first sight of (path, revision) in this window",
            Self::Declined => "repeat inside coverage — DECLINED",
            Self::BelowFloor => "repeat inside coverage, under the line floor",
            Self::ObservedNotDeclined => "repeat inside coverage, decided not acted on",
            Self::RepeatAfterDecline => "the one allowed repeat of a declined call",
            Self::NoHonestFrame => "repeat inside coverage with nothing exact to say",
            Self::RepeatOverlapping => "repeat overlapping held coverage",
            Self::RepeatDisjoint => "repeat of a read revision, over unheld lines",
            Self::OverCreditBound => "slice past the credit bound; nothing recorded",
            Self::CreditedFromStatement => "past the bound, credited from the client's statement",
            Self::SpilledToAFile => "result written to a file; a pointer reached the window",
            Self::UnknownSpan => "recognised read with no usable line range",
            Self::NotReadShaped => "read tool call outside the pinned argument set",
            Self::NoLineCount => "no name or line count for the path",
            Self::ClientDedup => "the client's own repeat guard answered it",
            Self::NoResultRecorded => "no result for the call in the transcript",
            Self::Search => "name or content search",
        }
    }

    /// Whether the window journal reached a decision that the range was already
    /// held — whether or not it acted on it.
    #[must_use]
    pub const fn declinable(self) -> bool {
        matches!(
            self,
            Self::Declined | Self::BelowFloor | Self::ObservedNotDeclined
        )
    }
}
