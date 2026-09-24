//! The panel-editor interaction state and the pure reposition operations
//! (panel-editor mode): entered as [`crate::input::Mode::PanelEditor`], it rearranges the
//! [`crate::layout_model::LayoutModel`] live on `App`.
//!
//! Everything here is pure — a cursor over the model's units, a pickup flag, and
//! functions that reorder/move/resize the columns. The reducer
//! (`crate::input::panel_editor::on_key_editor`) drives these; the renderer draws grips over
//! the cursor and the picked unit; a drop is a remove-then-insert that conserves
//! the unit multiset, so the model stays valid (1..=N columns, the same
//! `{Rail, cards, sections}` set) by construction.

use crate::layout_model::LayoutModel;

/// The keyboard cursor: which column, and which unit within it. Clamped to the
/// model on every move, so it can never point past an emptied column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Cursor {
    /// The column index.
    pub col: usize,
    /// The unit index within that column.
    pub unit: usize,
}

/// A live pointer drag (mouse), armed on a grip/divider press and cleared on
/// release (the scoped exception to the ignore-drags idle rule).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drag {
    /// Dragging a unit (identified by its source cursor) — the drop target is
    /// resolved from the pointer against the render geometry.
    Unit(Cursor),
    /// Dragging the divider to the right of column `col` — resize.
    Divider(usize),
}

/// The panel-editor's transient state. Lives on `App`; `Default` is the resting
/// state (cursor at the origin, nothing picked, no drag).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct EditorState {
    /// The keyboard cursor.
    pub cursor: Cursor,
    /// Whether the cursor's unit is lifted (a keyboard pickup).
    pub picked: bool,
    /// A live mouse drag, if any.
    pub drag: Option<Drag>,
}

impl EditorState {
    /// Reset to the resting state (on entering/leaving the mode, or after a reset).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Clamp the cursor into `model` (a column may have emptied under it).
    pub fn clamp(&mut self, model: &LayoutModel) {
        let ncols = model.columns.len();
        if ncols == 0 {
            self.cursor = Cursor::default();
            return;
        }
        self.cursor.col = self.cursor.col.min(ncols - 1);
        let nunits = model
            .columns
            .get(self.cursor.col)
            .map_or(0, |c| c.units.len());
        self.cursor.unit = self.cursor.unit.min(nunits.saturating_sub(1));
    }
}

/// The minimum weight a column keeps under a resize (so it never collapses to a
/// zero share and every column stays grabbable).
const MIN_WEIGHT: u16 = 4;

/// Move the cursor one column left/right (`delta` = -1/+1), clamping the unit
/// index into the new column. Never moves a picked unit — that is [`move_unit`].
pub fn move_cursor_col(ed: &mut EditorState, model: &LayoutModel, right: bool) {
    if model.columns.is_empty() {
        return;
    }
    let last = model.columns.len() - 1;
    ed.cursor.col = if right {
        (ed.cursor.col + 1).min(last)
    } else {
        ed.cursor.col.saturating_sub(1)
    };
    let nunits = model
        .columns
        .get(ed.cursor.col)
        .map_or(0, |c| c.units.len());
    ed.cursor.unit = ed.cursor.unit.min(nunits.saturating_sub(1));
}

/// Move the cursor one unit up/down within its column.
pub fn move_cursor_unit(ed: &mut EditorState, model: &LayoutModel, down: bool) {
    let Some(col) = model.columns.get(ed.cursor.col) else {
        return;
    };
    let last = col.units.len().saturating_sub(1);
    ed.cursor.unit = if down {
        (ed.cursor.unit + 1).min(last)
    } else {
        ed.cursor.unit.saturating_sub(1)
    };
}

/// Reorder the picked unit within its column (swap with the up/down neighbor),
/// carrying the cursor with it. A no-op at the column's edge.
pub fn reorder_within(ed: &mut EditorState, model: &mut LayoutModel, down: bool) {
    let Some(col) = model.columns.get_mut(ed.cursor.col) else {
        return;
    };
    let i = ed.cursor.unit;
    let j = if down {
        if i + 1 >= col.units.len() {
            return;
        }
        i + 1
    } else {
        if i == 0 {
            return;
        }
        i - 1
    };
    col.units.swap(i, j);
    ed.cursor.unit = j;
}

/// Move the picked unit to the adjacent column (`right` = +1), inserting it at
/// the cursor's unit index (clamped), and follow it with the cursor. A no-op at
/// the grid's edge. The unit multiset is conserved (a remove then an insert).
pub fn move_unit(ed: &mut EditorState, model: &mut LayoutModel, right: bool) {
    let from = ed.cursor.col;
    let to = if right {
        from.checked_add(1).filter(|&t| t < model.columns.len())
    } else {
        from.checked_sub(1)
    };
    let Some(to) = to else { return };
    let Some(unit) = model
        .columns
        .get_mut(from)
        .filter(|c| ed.cursor.unit < c.units.len())
        .map(|c| c.units.remove(ed.cursor.unit))
    else {
        return;
    };
    if let Some(dst) = model.columns.get_mut(to) {
        let at = ed.cursor.unit.min(dst.units.len());
        dst.units.insert(at, unit);
        ed.cursor.col = to;
        ed.cursor.unit = at;
    }
}

/// Swap the cursor's whole column with its left/right neighbor, carrying the
/// cursor. "Reorder whole columns" (panel-editor mode). A no-op at the edge.
pub fn move_column(ed: &mut EditorState, model: &mut LayoutModel, right: bool) {
    let from = ed.cursor.col;
    let to = if right {
        from.checked_add(1).filter(|&t| t < model.columns.len())
    } else {
        from.checked_sub(1)
    };
    let Some(to) = to else { return };
    model.columns.swap(from, to);
    ed.cursor.col = to;
}

/// Resize: grow the cursor's column by one weight and shrink the neighbor to its
/// right (`grow`) or vice-versa, clamped so neither drops below `MIN_WEIGHT`.
/// The divider between `col` and `col+1` moves; weights are relative, so the
/// solver re-tiles exactly (the data-driven layout model). A no-op without a right neighbor.
pub fn resize(ed: &EditorState, model: &mut LayoutModel, grow: bool) {
    let a = ed.cursor.col;
    let b = a + 1;
    let (Some(wa), Some(wb)) = (
        model.columns.get(a).map(|c| c.weight),
        model.columns.get(b).map(|c| c.weight),
    ) else {
        return;
    };
    // Shift one cell of weight across the divider, holding each side's minimum.
    let (na, nb) = if grow {
        if wb <= MIN_WEIGHT {
            return;
        }
        (wa.saturating_add(1), wb - 1)
    } else {
        if wa <= MIN_WEIGHT {
            return;
        }
        (wa - 1, wb.saturating_add(1))
    };
    if let Some(c) = model.columns.get_mut(a) {
        c.weight = na;
    }
    if let Some(c) = model.columns.get_mut(b) {
        c.weight = nb;
    }
}

/// Drop a dragged/picked unit from `src` into `dst` at `insert_at` (clamped),
/// conserving the multiset. Empty source columns are kept (they render zero-wide
/// but stay grabbable — panel-editor mode). Returns the unit's new cursor, or the source
/// cursor unchanged when the move is a no-op.
#[must_use]
pub fn drop_unit(model: &mut LayoutModel, src: Cursor, dst_col: usize, insert_at: usize) -> Cursor {
    let src_len = match model.columns.get(src.col) {
        Some(c) => c.units.len(),
        None => return src,
    };
    if dst_col >= model.columns.len() || src.unit >= src_len {
        return src;
    }
    let Some(unit) = model
        .columns
        .get_mut(src.col)
        .map(|c| c.units.remove(src.unit))
    else {
        return src;
    };
    // Removing from the source shifts indices in the same column left of the drop.
    let dst_len = model.columns.get(dst_col).map_or(0, |c| c.units.len());
    let mut at = insert_at.min(dst_len);
    if src.col == dst_col && src.unit < at {
        at = at.saturating_sub(1);
    }
    if let Some(dst) = model.columns.get_mut(dst_col) {
        let at = at.min(dst.units.len());
        dst.units.insert(at, unit);
        return Cursor {
            col: dst_col,
            unit: at,
        };
    }
    src
}

/// The unit under the cursor, if any (for the grip highlight).
#[must_use]
pub fn unit_at(model: &LayoutModel, cursor: Cursor) -> Option<&crate::layout_model::Unit> {
    model
        .columns
        .get(cursor.col)
        .and_then(|c| c.units.get(cursor.unit))
}

/// Whether `model` still holds exactly the canonical unit multiset (used by the
/// property tests): one rail, the four cards, the five panels, in any order.
#[must_use]
pub fn is_conserved(model: &LayoutModel) -> bool {
    let mut units: Vec<crate::layout_model::Unit> =
        model.columns.iter().flat_map(|c| c.units.clone()).collect();
    let mut want: Vec<crate::layout_model::Unit> = LayoutModel::default()
        .columns
        .iter()
        .flat_map(|c| c.units.clone())
        .collect();
    units.sort_by_key(|u| unit_key(*u));
    want.sort_by_key(|u| unit_key(*u));
    units == want
}

/// A stable total order on units, for multiset comparison in tests.
fn unit_key(u: crate::layout_model::Unit) -> (u8, u8) {
    use crate::layout_model::Unit;
    match u {
        Unit::Rail => (0, 0),
        Unit::Card(k) => (1, k as u8),
        Unit::Section(s) => (2, s as u8),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_model::Unit;

    fn model() -> LayoutModel {
        LayoutModel::default()
    }

    #[test]
    fn cursor_moves_and_clamps() {
        let m = model();
        let mut ed = EditorState::default();
        // Right across all columns, clamped at the last.
        move_cursor_col(&mut ed, &m, true);
        move_cursor_col(&mut ed, &m, true);
        move_cursor_col(&mut ed, &m, true);
        assert_eq!(ed.cursor.col, 2);
        // Down within the (sections) column, clamped at the last unit. Read
        // from the model rather than written as a literal: the seated panel
        // count is a product decision that has moved once already, and the
        // property here is "clamped at the last", not "clamped at 2".
        let last = m.columns[2].units.len() - 1;
        for _ in 0..(last + 7) {
            move_cursor_unit(&mut ed, &m, true);
        }
        assert_eq!(ed.cursor.unit, last);
    }

    #[test]
    fn reorder_within_a_column_swaps_neighbors() {
        let mut m = model();
        let mut ed = EditorState {
            cursor: Cursor { col: 1, unit: 0 },
            ..Default::default()
        };
        let first = m.columns[1].units[0];
        reorder_within(&mut ed, &mut m, true);
        assert_eq!(m.columns[1].units[1], first, "the unit moved down one");
        assert_eq!(ed.cursor.unit, 1, "the cursor followed it");
        assert!(is_conserved(&m));
    }

    #[test]
    fn move_unit_across_columns_conserves_the_multiset() {
        let mut m = model();
        let mut ed = EditorState {
            cursor: Cursor { col: 1, unit: 0 },
            ..Default::default()
        };
        let moved = m.columns[1].units[0];
        move_unit(&mut ed, &mut m, true); // card → sections column
        assert_eq!(ed.cursor.col, 2);
        assert!(m.columns[2].units.contains(&moved));
        assert!(!m.columns[1].units.contains(&moved));
        assert!(is_conserved(&m), "the unit set is unchanged");
    }

    #[test]
    fn move_column_swaps_and_follows() {
        let mut m = model();
        let mut ed = EditorState {
            cursor: Cursor { col: 0, unit: 0 },
            ..Default::default()
        };
        assert_eq!(m.columns[0].units, vec![Unit::Rail]);
        move_column(&mut ed, &mut m, true);
        assert_eq!(ed.cursor.col, 1);
        assert_eq!(
            m.columns[1].units,
            vec![Unit::Rail],
            "the rail column moved right"
        );
        assert!(is_conserved(&m));
    }

    #[test]
    fn resize_shifts_weight_and_holds_the_minimum() {
        let mut m = model();
        let ed = EditorState {
            cursor: Cursor { col: 0, unit: 0 },
            ..Default::default()
        };
        let before = (m.columns[0].weight, m.columns[1].weight);
        resize(&ed, &mut m, true);
        assert_eq!(m.columns[0].weight, before.0 + 1);
        assert_eq!(m.columns[1].weight, before.1 - 1);
        // A column never resizes its neighbor below the minimum.
        for _ in 0..100 {
            resize(&ed, &mut m, true);
        }
        assert!(m.columns[1].weight >= MIN_WEIGHT);
    }

    #[test]
    fn drop_unit_conserves_and_handles_same_column_shift() {
        let mut m = model();
        // Move card index 0 to index 3 within the same column.
        let moved = m.columns[1].units[0];
        let c = drop_unit(&mut m, Cursor { col: 1, unit: 0 }, 1, 3);
        assert_eq!(m.columns[1].units[c.unit], moved);
        assert!(is_conserved(&m));
    }

    proptest::proptest! {
        /// Any sequence of reposition ops keeps the model valid: the unit
        /// multiset is conserved and the column count never grows past the start.
        #[test]
        fn arbitrary_reposition_keeps_the_model_valid(
            ops in proptest::collection::vec(0u8..7, 0..40),
        ) {
            let mut m = model();
            let mut ed = EditorState::default();
            let start_cols = m.columns.len();
            for op in ops {
                ed.clamp(&m);
                match op {
                    0 => move_cursor_col(&mut ed, &m, false),
                    1 => move_cursor_col(&mut ed, &m, true),
                    2 => move_cursor_unit(&mut ed, &m, false),
                    3 => move_cursor_unit(&mut ed, &m, true),
                    4 => reorder_within(&mut ed, &mut m, true),
                    5 => move_unit(&mut ed, &mut m, true),
                    _ => move_column(&mut ed, &mut m, true),
                }
                proptest::prop_assert!(is_conserved(&m), "multiset conserved after op {}", op);
                proptest::prop_assert_eq!(m.columns.len(), start_cols, "column count stable");
            }
        }
    }
}
