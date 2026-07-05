//! Layout math for the three-column grid (20/40/40) plus the bottom command row.
//!
//! Pure geometry — no rendering — so the split logic is unit- and property-
//! testable. [`solve`] tiles a given area **exactly** (no gap/overlap) into three
//! columns above a reserved command row, honoring per-panel minimum widths. The
//! responsive degradation ladder (Full3Col → TwoCol → SinglePanel → TooSmall)
//! lands in P3 S2; this step delivers the nominal grid + the exact-tiling solver.

use crate::Panel;

/// A rectangle in terminal cells.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    /// Left edge.
    pub x: u16,
    /// Top edge.
    pub y: u16,
    /// Width in cells.
    pub w: u16,
    /// Height in cells.
    pub h: u16,
}

/// Which panel currently holds focus (the spec's `Focus`; identical to [`Panel`]).
pub type Focus = Panel;

/// Rows reserved for the bottom command panel (1 input line + border).
pub const COMMAND_ROWS: u16 = 3;
/// Minimum width for column 1 (a project name + status fits).
pub const MIN_COL1: u16 = 16;
/// Minimum width for column 2 (detail labels + toggles fit).
pub const MIN_COL2: u16 = 32;
/// Minimum width for column 3 (a graph + axis fits).
pub const MIN_COL3: u16 = 28;
/// Minimum body rows (above the command row) for the full three-column grid.
pub const MIN_BODY_ROWS: u16 = 8;
/// Below this width the dashboard shows the "terminal too small" notice.
pub const MIN_NOTICE_COLS: u16 = 24;
/// Below this height the dashboard shows the "terminal too small" notice.
pub const MIN_NOTICE_ROWS: u16 = 6;

/// The responsive layout tier for a given terminal size — a pure function of the
/// size, so the same size always yields the same tier (no hysteresis).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LayoutMode {
    /// The nominal 20/40/40 grid + command row.
    Full3Col,
    /// Projects on the left; detail stacked over graphs on the right.
    TwoCol,
    /// One panel at a time (switched by focus), with a tab strip.
    SinglePanel,
    /// Below the minimum: a centered "terminal too small" notice.
    TooSmall,
}

/// Choose the layout tier for `size`. Width drives the column count; a hard floor
/// on both dimensions falls back to the notice. Pure and total.
#[must_use]
pub fn choose_mode(size: crate::render::frame::Size) -> LayoutMode {
    let (w, h) = (size.w, size.h);
    if w < MIN_NOTICE_COLS || h < MIN_NOTICE_ROWS {
        LayoutMode::TooSmall
    } else if w < MIN_COL1 + MIN_COL2 + MIN_COL3 {
        if w < MIN_COL1 + MIN_COL2 {
            LayoutMode::SinglePanel
        } else {
            LayoutMode::TwoCol
        }
    } else {
        LayoutMode::Full3Col
    }
}

/// The four panel rectangles the dashboard renders into.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PanelRects {
    /// Column 1 — projects (≈20%).
    pub col1: Rect,
    /// Column 2 — detail + toggles (≈40%).
    pub col2: Rect,
    /// Column 3 — live graphs (≈40%).
    pub col3: Rect,
    /// The bottom command row.
    pub command: Rect,
}

/// Tile `area` into the 20/40/40 columns above a reserved command row. The
/// columns and command row **exactly** cover `area` (no gaps/overlaps) and each
/// column meets its minimum width when `area` is wide enough to host all three
/// (guaranteed by `choose_mode`'s Full3Col threshold, P3 S2).
#[must_use]
pub fn solve(area: Rect) -> PanelRects {
    let cmd_h = area.h.min(COMMAND_ROWS);
    let body_h = area.h.saturating_sub(cmd_h);
    let command = Rect {
        x: area.x,
        y: area.y.saturating_add(body_h),
        w: area.w,
        h: cmd_h,
    };
    let (w1, w2, w3) = split_3(area.w, [20, 40, 40], [MIN_COL1, MIN_COL2, MIN_COL3]);
    let col1 = Rect {
        x: area.x,
        y: area.y,
        w: w1,
        h: body_h,
    };
    let col2 = Rect {
        x: area.x.saturating_add(w1),
        y: area.y,
        w: w2,
        h: body_h,
    };
    let col3 = Rect {
        x: area.x.saturating_add(w1).saturating_add(w2),
        y: area.y,
        w: w3,
        h: body_h,
    };
    PanelRects {
        col1,
        col2,
        col3,
        command,
    }
}

/// Carve the bottom command row off `area`, returning `(body, command)`.
#[must_use]
pub fn command_split(area: Rect) -> (Rect, Rect) {
    let cmd_h = area.h.min(COMMAND_ROWS);
    let body_h = area.h.saturating_sub(cmd_h);
    let body = Rect {
        x: area.x,
        y: area.y,
        w: area.w,
        h: body_h,
    };
    let command = Rect {
        x: area.x,
        y: area.y.saturating_add(body_h),
        w: area.w,
        h: cmd_h,
    };
    (body, command)
}

/// The two-column degraded layout: projects on the left, detail stacked over
/// graphs on the right, above the command row. `col1`=projects, `col2`=detail
/// (top-right), `col3`=graphs (bottom-right).
#[must_use]
pub fn two_col(area: Rect) -> PanelRects {
    let (body, command) = command_split(area);
    let left = (body.w / 3).max(MIN_COL1).min(body.w);
    let right_w = body.w.saturating_sub(left);
    let right_x = body.x.saturating_add(left);
    let top_h = body.h / 2;
    let col1 = Rect {
        x: body.x,
        y: body.y,
        w: left,
        h: body.h,
    };
    let col2 = Rect {
        x: right_x,
        y: body.y,
        w: right_w,
        h: top_h,
    };
    let col3 = Rect {
        x: right_x,
        y: body.y.saturating_add(top_h),
        w: right_w,
        h: body.h.saturating_sub(top_h),
    };
    PanelRects {
        col1,
        col2,
        col3,
        command,
    }
}

/// The single-panel degraded layout: a 1-row tab strip, one body panel, and the
/// command row. Returns `(tabs, panel, command)`.
#[must_use]
pub fn single_panel(area: Rect) -> (Rect, Rect, Rect) {
    let (body, command) = command_split(area);
    let tabs_h = body.h.min(1);
    let tabs = Rect {
        x: body.x,
        y: body.y,
        w: body.w,
        h: tabs_h,
    };
    let panel = Rect {
        x: body.x,
        y: body.y.saturating_add(tabs_h),
        w: body.w,
        h: body.h.saturating_sub(tabs_h),
    };
    (tabs, panel, command)
}

/// Split `total` into three widths by `pct` (the remainder lands on the middle
/// column so the three sum to `total` exactly), then raise any column below its
/// minimum. Returns the proportional split unchanged when `total` is too narrow
/// to host all minimums (the degraded modes, P3 S2, take over there).
fn split_3(total: u16, pct: [u16; 3], min: [u16; 3]) -> (u16, u16, u16) {
    let [p0, _p1, p2] = pct;
    let [m0, m1, m2] = min;
    let t = u32::from(total);
    let w0 = u16::try_from(t.saturating_mul(u32::from(p0)) / 100).unwrap_or(0);
    let w2 = u16::try_from(t.saturating_mul(u32::from(p2)) / 100).unwrap_or(0);
    // Remainder → middle column so the three always sum to `total` exactly.
    let w1 = total.saturating_sub(w0).saturating_sub(w2);
    enforce_minimums((w0, w1, w2), (m0, m1, m2), total)
}

/// Raise any column below its minimum to that minimum, reclaiming the deficit
/// from the columns that still have surplus (widest surplus first) so the total
/// width is preserved. A no-op when `total < ma + mb + mc` (the degraded modes
/// handle that case).
fn enforce_minimums(
    (a, b, c): (u16, u16, u16),
    (ma, mb, mc): (u16, u16, u16),
    total: u16,
) -> (u16, u16, u16) {
    let min_sum = u32::from(ma) + u32::from(mb) + u32::from(mc);
    if u32::from(total) < min_sum {
        return (a, b, c);
    }
    let need = ma.saturating_sub(a) + mb.saturating_sub(b) + mc.saturating_sub(c);
    if need == 0 {
        return (a, b, c);
    }
    // Raise deficient columns to their minimums…
    let mut a = a.max(ma);
    let mut b = b.max(mb);
    let mut c = c.max(mc);
    // …then reclaim `need` cells from the widest surplus column, one at a time.
    for _ in 0..need {
        let sa = a.saturating_sub(ma);
        let sb = b.saturating_sub(mb);
        let sc = c.saturating_sub(mc);
        if sa >= sb && sa >= sc && sa > 0 {
            a = a.saturating_sub(1);
        } else if sb >= sc && sb > 0 {
            b = b.saturating_sub(1);
        } else if sc > 0 {
            c = c.saturating_sub(1);
        } else {
            break;
        }
    }
    (a, b, c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(w: u16, h: u16) -> Rect {
        Rect { x: 0, y: 0, w, h }
    }

    fn size(w: u16, h: u16) -> crate::render::frame::Size {
        crate::render::frame::Size::new(w, h)
    }

    #[test]
    fn choose_mode_thresholds() {
        // Full three-column grid at and above the min-sum width with enough rows.
        assert_eq!(choose_mode(size(76, 20)), LayoutMode::Full3Col);
        assert_eq!(choose_mode(size(200, 50)), LayoutMode::Full3Col);
        // Just under the three-column width but wide enough for two columns.
        assert_eq!(choose_mode(size(75, 20)), LayoutMode::TwoCol);
        assert_eq!(choose_mode(size(48, 20)), LayoutMode::TwoCol);
        // Under two columns' width → a single panel.
        assert_eq!(choose_mode(size(47, 20)), LayoutMode::SinglePanel);
        assert_eq!(
            choose_mode(size(MIN_NOTICE_COLS, 20)),
            LayoutMode::SinglePanel
        );
        // Below either floor → the notice.
        assert_eq!(
            choose_mode(size(MIN_NOTICE_COLS - 1, 20)),
            LayoutMode::TooSmall
        );
        assert_eq!(
            choose_mode(size(120, MIN_NOTICE_ROWS - 1)),
            LayoutMode::TooSmall
        );
        assert_eq!(choose_mode(size(0, 0)), LayoutMode::TooSmall);
    }

    #[test]
    fn two_col_and_single_panel_tile_within_area() {
        let r = two_col(area(60, 20));
        // projects on the left; detail over graphs on the right, all inside width.
        assert_eq!(r.col1.x, 0);
        assert_eq!(r.col2.x, r.col1.w);
        assert_eq!(r.col3.x, r.col1.w);
        assert_eq!(r.col2.y + r.col2.h, r.col3.y, "detail sits above graphs");
        assert!(r.col1.w + r.col2.w <= 60);

        let (tabs, panel, command) = single_panel(area(40, 16));
        assert_eq!(tabs.h, 1);
        assert_eq!(panel.y, tabs.y + 1);
        assert_eq!(command.y, panel.y + panel.h);
        assert_eq!(command.h, COMMAND_ROWS);
    }

    #[test]
    fn command_row_height_is_reserved() {
        let r = solve(area(120, 40));
        assert_eq!(r.command.h, COMMAND_ROWS);
        assert_eq!(r.command.y, 40 - COMMAND_ROWS);
        assert_eq!(r.col1.h, 40 - COMMAND_ROWS);
        assert_eq!(r.command.w, 120);
    }

    #[test]
    fn split_3_is_20_40_40_at_common_widths() {
        for &w in &[100u16, 120, 200] {
            let (a, b, c) = split_3(w, [20, 40, 40], [MIN_COL1, MIN_COL2, MIN_COL3]);
            assert_eq!(a + b + c, w, "columns sum to width exactly at w={w}");
            assert_eq!(a, w * 20 / 100, "col1 ≈ 20% at w={w}");
            // col2 absorbs the rounding remainder, so it is ≥ 40%.
            assert!(b >= w * 40 / 100, "col2 ≥ 40% at w={w}");
        }
    }

    #[test]
    fn minimums_are_enforced_at_the_narrow_boundary() {
        // At the exact min-sum width every column lands on its minimum.
        let w = MIN_COL1 + MIN_COL2 + MIN_COL3; // 76
        let (a, b, c) = split_3(w, [20, 40, 40], [MIN_COL1, MIN_COL2, MIN_COL3]);
        assert_eq!((a, b, c), (MIN_COL1, MIN_COL2, MIN_COL3));
        assert_eq!(a + b + c, w);
    }

    #[test]
    fn columns_abut_with_no_gap_or_overlap() {
        let r = solve(area(137, 30));
        assert_eq!(r.col1.x, 0);
        assert_eq!(r.col2.x, r.col1.x + r.col1.w);
        assert_eq!(r.col3.x, r.col2.x + r.col2.w);
        assert_eq!(r.col3.x + r.col3.w, 137);
    }

    #[test]
    fn zero_or_huge_size_never_panics() {
        for &(w, h) in &[(0, 0), (1, 1), (2, 3), (u16::MAX, u16::MAX), (u16::MAX, 1)] {
            let r = solve(area(w, h));
            // command never exceeds the area height…
            assert!(r.command.h <= h);
            // …and the columns never exceed the area width.
            assert!(
                u32::from(r.col1.w) + u32::from(r.col2.w) + u32::from(r.col3.w) <= u32::from(w)
            );
        }
    }

    proptest::proptest! {
        /// For any area wide enough for the grid, the three columns + command row
        /// tile it exactly (no gap/overlap) and the columns sum to the width.
        #[test]
        fn tiling_is_exact_and_gapless(
            w in (MIN_COL1 + MIN_COL2 + MIN_COL3)..4000u16,
            h in (COMMAND_ROWS + 1)..300u16,
        ) {
            let r = solve(area(w, h));
            // horizontal: columns abut and cover the full width
            proptest::prop_assert_eq!(r.col1.x, 0);
            proptest::prop_assert_eq!(r.col2.x, r.col1.x + r.col1.w);
            proptest::prop_assert_eq!(r.col3.x, r.col2.x + r.col2.w);
            proptest::prop_assert_eq!(r.col3.x + r.col3.w, w);
            // vertical: body + command cover the full height
            proptest::prop_assert_eq!(r.col1.h + r.command.h, h);
            proptest::prop_assert_eq!(r.command.y, r.col1.h);
        }

        /// No column drops below its minimum width once the grid fits.
        #[test]
        fn no_panel_has_zero_width_above_minimum(
            w in (MIN_COL1 + MIN_COL2 + MIN_COL3)..4000u16,
        ) {
            let (a, b, c) = split_3(w, [20, 40, 40], [MIN_COL1, MIN_COL2, MIN_COL3]);
            proptest::prop_assert!(a >= MIN_COL1);
            proptest::prop_assert!(b >= MIN_COL2);
            proptest::prop_assert!(c >= MIN_COL3);
            proptest::prop_assert_eq!(a + b + c, w);
        }
    }
}
