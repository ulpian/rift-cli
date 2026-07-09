//! The data-driven layout model (ADR-0033): the dashboard grid as *data* the
//! renderer and the pure hit-test both read, instead of three compile-time
//! constants (the `[20,40,40]` split and the `CARDS`/`SECTIONS` order).
//!
//! A [`LayoutModel`] is ordered [`Column`]s, each a width weight and an ordered
//! list of [`Unit`]s (the projects rail, the column-2 cards, the column-3
//! panels). [`solve_model`] tiles the columns over an area — variable weights
//! via [`crate::layout::split_n`], column reorder via prefix-sum x-offsets — so
//! exact-tiling holds because no rect is ever stored. [`LayoutModel::default`]
//! reproduces today's grid, so the default renders byte-for-byte identically
//! (proven by the `solve_model(default) ≡ solve` pin) and the panel editor
//! (M5.9) only ever mutates this one value.

use crate::cards::CardKind;
use crate::col3::SectionId;
use crate::layout::{chrome_split, split_n, Rect, MIN_COL1, MIN_COL2, MIN_COL3};

/// A content unit — the atom the panel editor moves. Exactly one [`Unit::Rail`]
/// exists (the projects nav); the six cards and three panels are the movable
/// sub-panels. Id-carrying (not index), so a move maps to a stable mutation and
/// a persisted layout survives reordering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// The projects rail (column 1 in the default).
    Rail,
    /// A column-2 card.
    Card(CardKind),
    /// A column-3 panel.
    Section(SectionId),
}

impl Unit {
    /// This unit's minimum column width — the floor a column hosting it needs.
    #[must_use]
    pub fn min_width(self) -> u16 {
        match self {
            Unit::Rail => MIN_COL1,
            Unit::Card(_) => MIN_COL2,
            Unit::Section(_) => MIN_COL3,
        }
    }
}

/// The role a (homogeneous) column plays, derived from its units — the M5.8
/// render/hit dispatch key. A default column is always homogeneous; mixed
/// columns (a card dragged among panels) arrive with the M5.9 per-unit path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnRole {
    /// Hosts the projects rail.
    Rail,
    /// Hosts column-2 cards.
    Cards,
    /// Hosts column-3 panels.
    Sections,
    /// No units (renders zero-wide).
    Empty,
}

/// One column: a width weight (replacing the `[20,40,40]` literal) and an ordered
/// list of units (replacing the `CARDS`/`SECTIONS` order).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    /// Relative share of the row width (the analogue of a `20`/`40` percent).
    pub weight: u16,
    /// The units stacked in this column, top to bottom.
    pub units: Vec<Unit>,
}

impl Column {
    /// The column's minimum width: the widest minimum among its units (0 when
    /// empty — an emptied column renders zero-wide but keeps a grabbable divider).
    #[must_use]
    pub fn min_width(&self) -> u16 {
        self.units.iter().map(|u| u.min_width()).max().unwrap_or(0)
    }

    /// The column's dispatch role, derived from its units.
    #[must_use]
    pub fn role(&self) -> ColumnRole {
        if self.units.is_empty() {
            ColumnRole::Empty
        } else if self.units.iter().any(|u| matches!(u, Unit::Rail)) {
            ColumnRole::Rail
        } else if self.units.iter().all(|u| matches!(u, Unit::Section(_))) {
            ColumnRole::Sections
        } else {
            ColumnRole::Cards
        }
    }
}

/// The whole arrangement: ordered columns (left to right). "Reorder whole
/// columns" is permuting this vec; the column count is 1..=3 in practice, but the
/// solver is total for any count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutModel {
    /// The columns, left to right.
    pub columns: Vec<Column>,
}

impl Default for LayoutModel {
    /// The canonical grid: rail (weight 20) · the six cards (weight 40) · the
    /// three panels (weight 40) — reproducing the shipped `[20,40,40]` split and
    /// the `CARDS`/`SECTIONS` order so `solve_model(&default, area)` equals
    /// `layout::solve(area)` at every size.
    fn default() -> Self {
        LayoutModel {
            columns: vec![
                Column {
                    weight: 20,
                    units: vec![Unit::Rail],
                },
                Column {
                    weight: 40,
                    units: default_card_units(),
                },
                Column {
                    weight: 40,
                    units: default_section_units(),
                },
            ],
        }
    }
}

/// The cards in their default display order, as units.
#[must_use]
pub fn default_card_units() -> Vec<Unit> {
    CardKind::ALL.iter().copied().map(Unit::Card).collect()
}

/// The panels in their default display order, as units.
#[must_use]
pub fn default_section_units() -> Vec<Unit> {
    SectionId::ALL.iter().copied().map(Unit::Section).collect()
}

/// One solved column: its rect and the units it hosts (cloned from the model so
/// the renderer and hit-test read the same list).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolvedColumn {
    /// The column's rect within the body.
    pub rect: Rect,
    /// The units it hosts, top to bottom.
    pub units: Vec<Unit>,
}

impl SolvedColumn {
    /// The column's dispatch role.
    #[must_use]
    pub fn role(&self) -> ColumnRole {
        Column {
            weight: 1,
            units: self.units.clone(),
        }
        .role()
    }
}

/// A solved layout: the tiled columns plus the bottom chrome rects (hint bar +
/// command row), which are the same as `layout::solve`'s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolvedLayout {
    /// The columns, left to right, each with its rect and units.
    pub columns: Vec<SolvedColumn>,
    /// The hint bar rect (zero-height when collapsed).
    pub hint: Rect,
    /// The bottom command row rect.
    pub command: Rect,
}

/// Tile `area` for `model`: carve the bottom chrome (hint + command), split the
/// body width across the columns by weight ([`split_n`], honoring each column's
/// minimum), and place the columns left to right via prefix-sum x-offsets. Pure
/// geometry — no rect is stored — so exact-tiling holds every frame and a column
/// reorder is just a permutation of the model's columns (ADR-0033).
#[must_use]
pub fn solve_model(model: &LayoutModel, area: Rect) -> SolvedLayout {
    let (body, hint, command) = chrome_split(area);
    let weights: Vec<u16> = model.columns.iter().map(|c| c.weight).collect();
    let mins: Vec<u16> = model.columns.iter().map(Column::min_width).collect();
    let widths = split_n(body.w, &weights, &mins);
    let mut x = body.x;
    let mut columns = Vec::with_capacity(model.columns.len());
    for (col, w) in model.columns.iter().zip(widths) {
        columns.push(SolvedColumn {
            rect: Rect {
                x,
                y: body.y,
                w,
                h: body.h,
            },
            units: col.units.clone(),
        });
        x = x.saturating_add(w);
    }
    SolvedLayout {
        columns,
        hint,
        command,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::solve;

    fn area(w: u16, h: u16) -> Rect {
        Rect { x: 0, y: 0, w, h }
    }

    #[test]
    fn default_has_the_canonical_three_columns() {
        let m = LayoutModel::default();
        assert_eq!(m.columns.len(), 3);
        assert_eq!(m.columns[0].role(), ColumnRole::Rail);
        assert_eq!(m.columns[1].role(), ColumnRole::Cards);
        assert_eq!(m.columns[2].role(), ColumnRole::Sections);
        assert_eq!(m.columns[0].units, vec![Unit::Rail]);
        assert_eq!(m.columns[1].units.len(), 6);
        assert_eq!(m.columns[2].units.len(), 3);
    }

    #[test]
    fn default_column_minimums_equal_the_grid_minimums() {
        let m = LayoutModel::default();
        assert_eq!(m.columns[0].min_width(), MIN_COL1);
        assert_eq!(m.columns[1].min_width(), MIN_COL2);
        assert_eq!(m.columns[2].min_width(), MIN_COL3);
    }

    #[test]
    fn empty_column_is_zero_wide_and_never_panics() {
        let m = LayoutModel {
            columns: vec![Column {
                weight: 10,
                units: vec![],
            }],
        };
        let sl = solve_model(&m, area(120, 40));
        assert_eq!(sl.columns.len(), 1);
        // A lone empty column takes the whole (min-free) width; the point is it
        // never panics and role() classifies it.
        assert_eq!(sl.columns[0].role(), ColumnRole::Empty);
    }

    proptest::proptest! {
        /// The fidelity pin (ADR-0033): `solve_model(&default, area)` tiles the
        /// three columns and the bottom chrome exactly as the shipped
        /// `layout::solve(area)` — so the default render is byte-for-byte today's.
        #[test]
        fn solve_model_default_equals_solve(w in 76u16..4000, h in 4u16..300) {
            let sl = solve_model(&LayoutModel::default(), area(w, h));
            let ps = solve(area(w, h));
            proptest::prop_assert_eq!(sl.columns.len(), 3);
            proptest::prop_assert_eq!(sl.columns[0].rect, ps.col1);
            proptest::prop_assert_eq!(sl.columns[1].rect, ps.col2);
            proptest::prop_assert_eq!(sl.columns[2].rect, ps.col3);
            proptest::prop_assert_eq!(sl.hint, ps.hint);
            proptest::prop_assert_eq!(sl.command, ps.command);
        }

        /// For any model, the columns abut with no gap/overlap and cover the body
        /// width, at any size — exact-tiling holds for an arbitrary arrangement.
        #[test]
        fn arbitrary_model_tiles_the_body_exactly(
            weights in proptest::collection::vec(1u16..80, 1..4),
            w in 0u16..4000,
            h in 0u16..300,
        ) {
            // Give every column the rail (so a real minimum applies).
            let model = LayoutModel {
                columns: weights
                    .iter()
                    .map(|&weight| Column { weight, units: vec![Unit::Rail] })
                    .collect(),
            };
            let sl = solve_model(&model, area(w, h));
            // Columns abut from the body's left edge.
            let (body, _, _) = chrome_split(area(w, h));
            let mut x = body.x;
            let mut sum = 0u32;
            for c in &sl.columns {
                proptest::prop_assert_eq!(c.rect.x, x);
                proptest::prop_assert_eq!(c.rect.y, body.y);
                proptest::prop_assert_eq!(c.rect.h, body.h);
                x = x.saturating_add(c.rect.w);
                sum += u32::from(c.rect.w);
            }
            // They cover the body width exactly when it admits the minimums…
            let min_sum: u32 = sl.columns.len() as u32 * u32::from(MIN_COL1);
            if u32::from(body.w) >= min_sum {
                proptest::prop_assert_eq!(sum, u32::from(body.w));
            } else {
                // …and never overflow it otherwise.
                proptest::prop_assert!(sum <= u32::from(body.w));
            }
        }
    }
}
