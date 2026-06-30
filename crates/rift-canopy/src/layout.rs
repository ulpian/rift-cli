//! Layout math for the three-column grid (20/40/40), responsive on resize.
//!
//! Pure geometry — no rendering — so the split logic is unit-testable. Below a
//! minimum width the grid collapses to a single column (a degraded but usable
//! fallback for narrow terminals).

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

/// Minimum total width below which the grid collapses to one column.
pub const MIN_GRID_WIDTH: u16 = 60;

/// Split `area` into the projects/detail/graphs columns (20/40/40). Returns one
/// rect when the area is too narrow to host three columns.
#[must_use]
pub fn three_column(area: Rect) -> Vec<Rect> {
    if area.w < MIN_GRID_WIDTH {
        return vec![area];
    }
    // Compute in u32 to avoid overflowing u16 on wide terminals.
    let w = u32::from(area.w);
    let c1 = u16::try_from(w * 20 / 100).unwrap_or(0);
    let c2 = u16::try_from(w * 40 / 100).unwrap_or(0);
    let c3 = area.w - c1 - c2; // remainder absorbs rounding so columns tile exactly
    vec![
        Rect {
            x: area.x,
            y: area.y,
            w: c1,
            h: area.h,
        },
        Rect {
            x: area.x + c1,
            y: area.y,
            w: c2,
            h: area.h,
        },
        Rect {
            x: area.x + c1 + c2,
            y: area.y,
            w: c3,
            h: area.h,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_columns_tile_exactly() {
        let area = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 40,
        };
        let cols = three_column(area);
        assert_eq!(cols.len(), 3);
        // columns abut and cover the whole width with no gaps/overlaps
        assert_eq!(cols[0].x, 0);
        assert_eq!(cols[1].x, cols[0].x + cols[0].w);
        assert_eq!(cols[2].x, cols[1].x + cols[1].w);
        assert_eq!(cols[2].x + cols[2].w, area.w);
        // 20/40/40 split
        assert_eq!(cols[0].w, 20);
        assert_eq!(cols[1].w, 40);
    }

    #[test]
    fn narrow_area_collapses_to_one_column() {
        let area = Rect {
            x: 0,
            y: 0,
            w: 40,
            h: 20,
        };
        assert_eq!(three_column(area).len(), 1);
    }

    proptest::proptest! {
        /// For any wide area, columns always tile it exactly (no gap/overlap).
        #[test]
        fn columns_partition_width(w in 60u16..2000, h in 1u16..200) {
            let cols = three_column(Rect { x: 0, y: 0, w, h });
            proptest::prop_assert_eq!(cols.len(), 3);
            proptest::prop_assert_eq!(cols[2].x + cols[2].w, w);
            for pair in cols.windows(2) {
                proptest::prop_assert_eq!(pair[1].x, pair[0].x + pair[0].w);
            }
        }
    }
}
