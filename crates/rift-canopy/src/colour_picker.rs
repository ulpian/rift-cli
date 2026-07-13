//! The colour picker's interaction state (ADR-0039).
//!
//! Like the profile menu, the picker is a **mode**, not an `Overlay`: a cursor
//! over the five theme slots, then over the ten named presets (plus the reset
//! row), with a live preview — the running dashboard itself re-themes on every
//! selection. State lives here; the reducer branch runs over synthetic keys.

use crate::theme::{ThemeSlot, PRESETS};

/// Which stage the picker is in (ADR-0039 §1). `Esc` steps back:
/// presets → slots → the profile menu.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PickerStage {
    /// Choosing which of the five slots to repaint.
    #[default]
    Slots,
    /// Choosing a preset (or the reset row) for the chosen slot.
    Presets,
}

/// One past the last preset row — the **reset to default** row's index.
pub const RESET_ROW: usize = PRESETS.len();

/// The colour picker's transient state — only meaningful while
/// `mode == Mode::ColourPicker`; resting (`Default`) otherwise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColourPickerState {
    /// The current stage.
    pub stage: PickerStage,
    /// The highlighted slot row (an index into [`ThemeSlot::ALL`]).
    pub slot_cursor: usize,
    /// The highlighted preset row (`0..PRESETS.len()`, or [`RESET_ROW`]).
    pub preset_cursor: usize,
}

impl ColourPickerState {
    /// Reset to the resting state (entering the picker fresh).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The slot the cursor rests on (clamped — total over any cursor value).
    #[must_use]
    pub fn slot(&self) -> ThemeSlot {
        ThemeSlot::ALL
            .get(self.slot_cursor.min(ThemeSlot::ALL.len() - 1))
            .copied()
            .unwrap_or(ThemeSlot::Background)
    }
}
