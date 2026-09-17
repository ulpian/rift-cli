//! The data-driven layout model: the dashboard grid as *data* the
//! renderer and the pure hit-test both read, instead of three compile-time
//! constants (the `[20,40,40]` split and the `CARDS`/`SECTIONS` order).
//!
//! A [`LayoutModel`] is ordered [`Column`]s, each a width weight and an ordered
//! list of [`Unit`]s (the projects rail, the column-2 cards, the column-3
//! panels). [`solve_model`] tiles the columns over an area — variable weights
//! via `crate::layout::split_n`, column reorder via prefix-sum x-offsets — so
//! exact-tiling holds because no rect is ever stored. [`LayoutModel::default`]
//! reproduces today's grid, so the default renders byte-for-byte identically
//! (proven by the `solve_model(default) ≡ solve` pin) and the panel editor
//! only ever mutates this one value.

use crate::cards::CardKind;
use crate::col3::SectionId;
use crate::layout::{chrome_split, split_n, Rect, MIN_COL1, MIN_COL2, MIN_COL3};

/// A content unit — the atom the panel editor moves. Exactly one [`Unit::Rail`]
/// exists (the projects nav); the three cards and five panels are the movable
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

    /// This unit's stack sizing (weight, min-rows, priority) — so a mixed column
    /// stacks the rail, cards, and panels through the one shared solver. The rail
    /// holds out longest and fills a column it shares.
    #[must_use]
    pub fn stack_spec(self) -> crate::stack::Spec {
        match self {
            Unit::Rail => crate::stack::Spec {
                weight: 3,
                min_rows: 3,
                priority: crate::stack::Priority::High,
            },
            Unit::Card(k) => crate::cards::card_spec(k),
            Unit::Section(s) => crate::col3::section_spec(s),
        }
    }
}

/// The role a (homogeneous) column plays, derived from its units — the
/// render/hit dispatch key. A default column is always homogeneous; mixed
/// columns (a card dragged among panels) arrive with the per-unit path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnRole {
    /// Exactly the projects rail.
    Rail,
    /// All column-2 cards (renders via the card stack).
    Cards,
    /// All column-3 panels (renders via the panel stack).
    Sections,
    /// A heterogeneous mix (a card among panels, the rail among others) — the
    /// per-unit path after a cross-column move (panel-editor mode).
    Mixed,
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
        role_of(&self.units)
    }

    /// This column's cards in order (empty unless it is an all-cards column).
    #[must_use]
    pub fn card_kinds(&self) -> Vec<CardKind> {
        self.units
            .iter()
            .filter_map(|u| match u {
                Unit::Card(k) => Some(*k),
                _ => None,
            })
            .collect()
    }

    /// This column's panels in order (empty unless it is an all-panels column).
    #[must_use]
    pub fn section_ids(&self) -> Vec<SectionId> {
        self.units
            .iter()
            .filter_map(|u| match u {
                Unit::Section(s) => Some(*s),
                _ => None,
            })
            .collect()
    }
}

/// The dispatch role of an ordered unit slice.
#[must_use]
pub fn role_of(units: &[Unit]) -> ColumnRole {
    if units.is_empty() {
        ColumnRole::Empty
    } else if units == [Unit::Rail] {
        ColumnRole::Rail
    } else if units.iter().all(|u| matches!(u, Unit::Card(_))) {
        ColumnRole::Cards
    } else if units.iter().all(|u| matches!(u, Unit::Section(_))) {
        ColumnRole::Sections
    } else {
        ColumnRole::Mixed
    }
}

/// One solved unit slot within a (mixed) column: which unit, its rect, and
/// whether it collapsed — the per-unit analogue of a card/section slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitSlot {
    /// The unit.
    pub unit: Unit,
    /// Its rect within the column.
    pub rect: Rect,
    /// Collapsed to a one-line headline.
    pub collapsed: bool,
}

/// Solve a heterogeneous column of `units` over `area` with a scroll `offset`,
/// stacking them through the shared [`crate::stack`] solver (each unit sized by
/// its [`Unit::stack_spec`]). Returns the visible slots and the largest in-range
/// offset. Used for a mixed column (a card among panels) after a cross-column
/// move (panel-editor mode); the homogeneous card/panel stacks keep their own solvers.
#[must_use]
pub fn solve_column(units: &[Unit], area: Rect, offset: usize) -> (Vec<UnitSlot>, usize) {
    let specs: Vec<crate::stack::Spec> = units.iter().map(|u| u.stack_spec()).collect();
    let (slots, max_scroll) = crate::stack::solve_windowed(&specs, area.h, offset);
    let out = slots
        .into_iter()
        .filter_map(|slot| {
            units.get(slot.index).map(|unit| UnitSlot {
                unit: *unit,
                rect: Rect {
                    x: area.x,
                    y: area.y + slot.y,
                    w: area.w,
                    h: slot.rows,
                },
                collapsed: slot.collapsed,
            })
        })
        .collect();
    (out, max_scroll)
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
    /// The canonical grid: rail (weight 20) · the three cards (weight 40) · the
    /// five panels (weight 40) — reproducing the shipped `[20,40,40]` split and
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

// --- the persistence codec (per-project layout persistence) ----------------------------------------

impl LayoutModel {
    /// Encode to a versioned string for the append-only layout store:
    /// `v1;<weight>=<unit>,<unit>;…` where a unit is `rail` / `card.<kind>` /
    /// `sec.<id>`. No serde in the pure crate — a stable, human-legible line.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut out = String::from("v1");
        for col in &self.columns {
            out.push(';');
            out.push_str(&col.weight.to_string());
            out.push('=');
            let joined: Vec<String> = col.units.iter().map(|u| unit_string(*u)).collect();
            out.push_str(&joined.join(","));
        }
        out
    }

    /// Decode a [`Self::encode`] string, or `None` on any malformed or
    /// forward-version blob (the caller falls back to the default). A
    /// store written by a newer binary degrades to the canonical layout rather
    /// than crashing.
    #[must_use]
    pub fn decode(s: &str) -> Option<LayoutModel> {
        let mut parts = s.split(';');
        if parts.next()? != "v1" {
            return None;
        }
        let mut columns = Vec::new();
        for part in parts {
            let (w, units_str) = part.split_once('=')?;
            let weight: u16 = w.parse().ok()?;
            let mut units = Vec::new();
            if !units_str.is_empty() {
                for tok in units_str.split(',') {
                    if is_retired_card_token(tok) {
                        continue;
                    }
                    units.push(parse_unit(tok)?);
                }
            }
            columns.push(Column { weight, units });
        }
        columns.retain(|column| !column.units.is_empty());
        if columns.is_empty() {
            return None;
        }
        Some(LayoutModel { columns })
    }
}

/// A unit's full persisted string (`rail` / `card.<kind>` / `sec.<id>`).
fn unit_string(u: Unit) -> String {
    match u {
        Unit::Rail => "rail".to_string(),
        Unit::Card(k) => format!("card.{}", card_token(k)),
        Unit::Section(s) => format!("sec.{}", section_token(s)),
    }
}

/// Parse a persisted unit token, or `None` on an unknown one.
fn parse_unit(tok: &str) -> Option<Unit> {
    if tok == "rail" {
        return Some(Unit::Rail);
    }
    let (kind, name) = tok.split_once('.')?;
    match kind {
        "card" => card_from_token(name).map(Unit::Card),
        "sec" => section_from_token(name).map(Unit::Section),
        _ => None,
    }
}

/// Retired dashboard cards. Saved v1 layouts migrate by dropping only these
/// known tokens; every other unknown token fails closed.
///
/// The first three were retired into Project Settings. `card.capture` was
/// retired outright: its three counts (events, sessions, checkpoints) are the
/// same three the activity card strips over a month and the dispatch panel
/// reports exactly, so the card was a third rendering of numbers already on
/// screen. Nothing was lost with it, which is why it is skipped rather than
/// re-seated somewhere.
///
/// **Skipping is the migration.** A layout the panel editor saved while the
/// card existed still names it, and `decode` fails closed for the *whole*
/// model on an unknown token — so without this row an arrangement made
/// yesterday would be discarded wholesale rather than losing the one unit that
/// no longer exists.
fn is_retired_card_token(tok: &str) -> bool {
    matches!(
        tok,
        "card.integrations" | "card.settings" | "card.features" | "card.capture"
    )
}

/// A card's stable persisted name.
fn card_token(k: CardKind) -> &'static str {
    match k {
        CardKind::Repository => "repository",
        CardKind::Models => "models",
        CardKind::Activity => "activity",
    }
}

/// A card from its persisted name.
fn card_from_token(name: &str) -> Option<CardKind> {
    CardKind::ALL
        .iter()
        .copied()
        .find(|k| card_token(*k) == name)
}

/// A panel's stable persisted name. The match is exhaustive, so a new
/// [`SectionId`] cannot reach [`LayoutModel::encode`] without being given one.
fn section_token(s: SectionId) -> &'static str {
    match s {
        SectionId::TokenUsage => "tokens",
        SectionId::System => "system",
        SectionId::Recall => "recall",
        SectionId::Dispatch => "dispatch",
        SectionId::Recap => "recap",
    }
}

/// A panel from its persisted name.
///
/// **A match over the tokens, not a scan of [`SectionId::ALL`].** `ALL` is the
/// panels the *default* layout seats, and a registered panel that is not
/// seated by default is still one a saved layout may name — a scan would fail
/// to decode it, and [`LayoutModel::decode`] fails closed, so one unknown token
/// discards the user's whole arrangement rather than one panel of it.
/// `every_panel_round_trips_its_persisted_token` pins both directions.
fn section_from_token(name: &str) -> Option<SectionId> {
    match name {
        "tokens" => Some(SectionId::TokenUsage),
        "system" => Some(SectionId::System),
        "recall" => Some(SectionId::Recall),
        "dispatch" => Some(SectionId::Dispatch),
        "recap" => Some(SectionId::Recap),
        _ => None,
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
        role_of(&self.units)
    }

    /// This column's cards in order (for the [`ColumnRole::Cards`] path).
    #[must_use]
    pub fn card_kinds(&self) -> Vec<CardKind> {
        self.units
            .iter()
            .filter_map(|u| match u {
                Unit::Card(k) => Some(*k),
                _ => None,
            })
            .collect()
    }

    /// This column's panels in order (for the [`ColumnRole::Sections`] path).
    #[must_use]
    pub fn section_ids(&self) -> Vec<SectionId> {
        self.units
            .iter()
            .filter_map(|u| match u {
                Unit::Section(s) => Some(*s),
                _ => None,
            })
            .collect()
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
/// body width across the columns by weight (`split_n`, honoring each column's
/// minimum), and place the columns left to right via prefix-sum x-offsets. Pure
/// geometry — no rect is stored — so exact-tiling holds every frame and a column
/// reorder is just a permutation of the model's columns (the data-driven layout model).
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
        assert_eq!(m.columns[1].units.len(), 3);
        assert_eq!(m.columns[2].units.len(), 5);
        assert!(
            !m.columns[1]
                .units
                .iter()
                .any(|u| matches!(u, Unit::Card(_)) && unit_string(*u) == "card.capture"),
            "the retired capture card is never seated"
        );
    }

    #[test]
    fn default_column_minimums_equal_the_grid_minimums() {
        let m = LayoutModel::default();
        assert_eq!(m.columns[0].min_width(), MIN_COL1);
        assert_eq!(m.columns[1].min_width(), MIN_COL2);
        assert_eq!(m.columns[2].min_width(), MIN_COL3);
    }

    #[test]
    fn codec_round_trips_the_default_and_rejects_garbage() {
        let m = LayoutModel::default();
        assert_eq!(LayoutModel::decode(&m.encode()), Some(m));
        // Malformed / forward-version blobs decode to None (→ caller's default).
        assert_eq!(LayoutModel::decode("v2;20=rail"), None, "forward version");
        assert_eq!(LayoutModel::decode("garbage"), None);
        assert_eq!(LayoutModel::decode("v1;x=rail"), None, "bad weight");
        assert_eq!(
            LayoutModel::decode("v1;20=card.bogus"),
            None,
            "unknown card"
        );
        assert_eq!(LayoutModel::decode("v1"), None, "no columns");
    }

    /// **Every registered panel decodes, whether or not the default seats it.**
    ///
    /// `decode` fails closed on an unknown token, and it fails closed for the
    /// whole model — so a panel that can be *written* into a saved layout and
    /// not read back does not lose one panel, it discards the whole
    /// arrangement. Registration and seating are separate decisions here, which
    /// is exactly the gap where that could happen.
    #[test]
    fn every_panel_round_trips_its_persisted_token() {
        let every = [
            SectionId::TokenUsage,
            SectionId::System,
            SectionId::Recall,
            SectionId::Dispatch,
            SectionId::Recap,
        ];
        for s in every {
            let tok = section_token(s);
            assert_eq!(section_from_token(tok), Some(s), "sec.{tok}");
            assert_eq!(
                parse_unit(&format!("sec.{tok}")),
                Some(Unit::Section(s)),
                "a saved layout naming sec.{tok} must survive decode"
            );
        }
        // **Compiler-forced completeness.** This match is exhaustive: a sixth
        // variant fails to compile here, and the length assertion below then
        // fails until it also joins `every` above — so neither half of the pin
        // can be forgotten independently.
        let position = |s: SectionId| match s {
            SectionId::TokenUsage => 0usize,
            SectionId::System => 1,
            SectionId::Recall => 2,
            SectionId::Dispatch => 3,
            SectionId::Recap => 4,
        };
        assert_eq!(every.len(), 5);
        for (i, s) in every.iter().enumerate() {
            assert_eq!(position(*s), i, "the list and the enum disagree");
        }
        // Tokens are distinct: two panels sharing one would silently alias.
        let mut tokens: Vec<&str> = every.iter().copied().map(section_token).collect();
        tokens.sort_unstable();
        tokens.dedup();
        assert_eq!(tokens.len(), every.len(), "a token is used twice");
        // …and every registered panel is one this table sizes.
        for s in every {
            assert!(
                crate::col3::SECTIONS.iter().any(|spec| spec.id == s),
                "a panel a layout can name must be one the stack solver sizes"
            );
        }
    }

    #[test]
    fn codec_migrates_only_retired_cards_and_removes_empty_columns() {
        let decoded = LayoutModel::decode(
            "v1;20=rail;40=card.repository,card.integrations,card.models,card.settings,card.features,card.activity;9=card.features;40=sec.tokens",
        )
        .expect("known retired cards migrate");
        assert_eq!(decoded.columns.len(), 3, "retired-only column is removed");
        assert_eq!(
            decoded.columns[1].weight, 40,
            "surviving weight is retained"
        );
        assert_eq!(
            decoded.columns[1].units,
            vec![
                Unit::Card(CardKind::Repository),
                Unit::Card(CardKind::Models),
                Unit::Card(CardKind::Activity),
            ],
            "surviving order is conserved"
        );
        assert_eq!(
            LayoutModel::decode("v1;20=rail;40=card.future"),
            None,
            "unknown non-retired cards still fail closed"
        );
    }

    /// An arrangement saved while the capture card was still on the grid must
    /// keep every *other* unit the person placed.
    ///
    /// `decode` fails closed for the whole model, so the alternative to naming
    /// `card.capture` retired is silently discarding a saved layout — the
    /// person's columns, weights and order — the first time the new binary
    /// reads it. The card is gone; the arrangement around it is not.
    #[test]
    fn a_layout_saved_with_the_capture_card_keeps_everything_else() {
        let decoded =
            LayoutModel::decode("v1;20=rail;40=card.capture,card.repository;40=sec.recall")
                .expect("a layout naming the retired capture card still decodes");
        assert_eq!(decoded.columns.len(), 3);
        assert_eq!(
            decoded.columns[1].units,
            vec![Unit::Card(CardKind::Repository)],
            "the retired card leaves; the surviving card and its column stay"
        );
        assert_eq!(
            decoded.columns[2].units,
            vec![Unit::Section(SectionId::Recall)]
        );
        // A column that held nothing else is removed rather than left empty.
        assert_eq!(
            LayoutModel::decode("v1;20=rail;40=card.capture")
                .expect("the rail column survives alone")
                .columns
                .len(),
            1,
        );
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
        /// The fidelity pin (the data-driven layout model): `solve_model(&default, area)` tiles the
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
