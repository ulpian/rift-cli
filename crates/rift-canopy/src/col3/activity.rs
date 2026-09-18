//! The activity card's view-model: the trailing month's three heat strips and
//! what they could not place on a day.
//!
//! Column 2 draws the card, but the data plane fills it in the same feed
//! frame as the column-3 panels, so its types live with theirs.

/// A single trailing-month day of activity for the three heat strips.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DayBar {
    /// The short weekday label ("Mon".."Sun"), computed from the injected clock.
    pub weekday: &'static str,
    /// Captured events on this calendar day.
    pub events: u64,
    /// Sessions started on this calendar day.
    pub sessions: u64,
    /// Checkpoints created on this calendar day.
    pub checkpoints: u64,
}

/// Capture rows a trailing-month strip could **not place on a day**: they fall
/// outside the window, or their time column is not a time this plane can read.
///
/// # Why a strip owes its reader a remainder
///
/// The strips are built by filtering a time column, and a filter drops what it
/// does not match — silently. That is fine while every row carries a time, and
/// it is a lie the moment one does not: `checkpoint.created_at` carries an
/// explicit unknown-time sentinel, so a strip filtering on it renders thirty
/// zeros for a repository that made a hundred checkpoints, three cards away
/// from the capture tile that says a hundred. Measured 2026-08-18 on the
/// owner's projection: **0 of 103** checkpoints placed, beside 15,887 events
/// and 14 sessions placed by the same read.
///
/// So the remainder is counted and shown rather than dropped. It deliberately
/// does **not** claim *why* a row is off the strip — "older than the window"
/// and "undated" are different facts, and this plane can only tell you that a
/// row is not on the strip. Splitting them needs the checkpoint-time derivation
/// that `rift dispatch` owns (see the crate docs' open-gap note); until that is
/// shared, the honest word is the one this type uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OffStrip {
    /// Captured events not placed on any day of the strip.
    pub events: u64,
    /// Sessions not placed on any day of the strip.
    pub sessions: u64,
    /// Checkpoints not placed on any day of the strip.
    pub checkpoints: u64,
}

impl OffStrip {
    /// Whether anything at all was left off the strip.
    #[must_use]
    pub const fn any(self) -> bool {
        self.events > 0 || self.sessions > 0 || self.checkpoints > 0
    }
}

/// The activity card's data: the trailing thirty calendar days, oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActivityData {
    /// Thirty days of heat-strip values (oldest → today). An absent day is a
    /// zero-count day (a fact), never dropped.
    pub days: Vec<DayBar>,
    /// What the thirty days do **not** account for — see [`OffStrip`]. Zero on
    /// a month whose every row landed on a day, which is why a fully-placed
    /// card renders exactly as it did before this field existed.
    pub off_strip: OffStrip,
}

/// The activity card view-model. Honest about absence: a **missing
/// projection** is the placeholder (never thirty zeros); a readable projection
/// with quiet days is `Days` with zero-count strips.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum ActivityView {
    /// No readable projection — the placeholder (distinct from a quiet month).
    #[default]
    NoProjection,
    /// The trailing month's three heat strips.
    Days(ActivityData),
}
