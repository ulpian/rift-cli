//! Pure quantitative layout for Canopy's heat strips and flat treemaps.

use crate::layout::Rect;
use crate::render::{Color, ShadeSample};

/// xRenderEngine and the portable floor both resolve 2x4 samples per cell.
const SX: u32 = 2;
const SY: u32 = 4;
const TREEMAP_LUMA: f32 = 0.82;

/// A generated one-dimensional heat strip.
#[derive(Clone, Debug)]
pub(crate) struct HeatStrip {
    /// Maximum raw value across the unresampled time window.
    pub max: u64,
    /// Max-pooled or nearest-neighbour-expanded values per output sample column.
    #[cfg(test)]
    pub columns: Vec<u64>,
    /// Ordered-dither samples resolved by the render seam.
    pub samples: Vec<ShadeSample>,
}

/// Generate one oldest-to-newest heat strip for `area`.
///
/// When fewer sample columns exist than days, each output column takes the
/// maximum of its contiguous day range so a narrow viewport cannot average a
/// peak away. Wider viewports repeat the nearest day. Density is independently
/// normalized to this strip's raw maximum.
#[must_use]
pub(crate) fn heat_strip(values: &[u64], area: Rect, color: Color) -> HeatStrip {
    let max = values.iter().copied().max().unwrap_or(0);
    let sample_cols = u32::from(area.w).saturating_mul(SX);
    let sample_rows = u32::from(area.h).saturating_mul(SY);
    if values.is_empty() || sample_cols == 0 || sample_rows == 0 {
        return HeatStrip {
            max,
            #[cfg(test)]
            columns: Vec::new(),
            samples: Vec::new(),
        };
    }

    let n = values.len();
    let out_len = usize::try_from(sample_cols).unwrap_or(0);
    let mut columns = Vec::with_capacity(out_len);
    for col in 0..out_len {
        let value = if out_len >= n {
            let idx = ((col * n + out_len / 2) / out_len).min(n - 1);
            values.get(idx).copied().unwrap_or(0)
        } else {
            let start = col * n / out_len;
            let end = (((col + 1) * n) / out_len).max(start + 1).min(n);
            values
                .get(start..end.max(start + 1))
                .unwrap_or(&[])
                .iter()
                .copied()
                .max()
                .unwrap_or(0)
        };
        columns.push(value);
    }

    let mut samples = Vec::new();
    for (sx, value) in columns.iter().copied().enumerate() {
        let filled = if max == 0 || value == 0 {
            0
        } else {
            let numerator = u128::from(value) * u128::from(sample_rows);
            u32::try_from(numerator.div_ceil(u128::from(max)))
                .unwrap_or(sample_rows)
                .min(sample_rows)
        };
        for sy in 0..sample_rows {
            if dither_rank(sy, sample_rows) >= filled {
                continue;
            }
            #[allow(clippy::cast_precision_loss)]
            let x = (sx as f32 + 0.5) / sample_cols as f32;
            #[allow(clippy::cast_precision_loss)]
            let y = (sy as f32 + 0.5) / sample_rows as f32;
            samples.push(ShadeSample {
                x,
                y,
                luma: 1.0,
                color,
            });
        }
    }
    HeatStrip {
        max,
        #[cfg(test)]
        columns,
        samples,
    }
}

/// A solid horizontal bar filling `fraction` of `area`, as engine-neutral
/// sub-cell samples for the render seam.
///
/// Samples rather than glyphs for the reason the treemaps and heat strips use
/// them: the caller asks for a shape and a shader kind, the backend decides
/// what a cell looks like, and the ASCII floor draws the same bar in density
/// characters instead of losing it. A bar drawn as literal block glyphs would
/// be a second rendering path with its own degradation story.
///
/// The fill is **solid, not dithered**: a heat strip dithers because its
/// density *is* the value, while a bar's length is the value and a
/// half-lit bar would read as a lighter quantity rather than a shorter one.
/// A positive fraction always lights at least one sample column, so a small
/// but real value is a short bar and never an empty row — absence and a
/// measured near-zero must not draw the same.
#[must_use]
pub(crate) fn hbar_samples(fraction: f64, area: Rect, color: Color) -> Vec<ShadeSample> {
    let cols = u32::from(area.w).saturating_mul(SX);
    let rows = u32::from(area.h).saturating_mul(SY);
    if cols == 0 || rows == 0 || !fraction.is_finite() || fraction <= 0.0 {
        return Vec::new();
    }
    let filled = {
        let scaled = (fraction.clamp(0.0, 1.0) * f64::from(cols)).ceil();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let scaled = scaled.max(1.0) as u32;
        scaled.min(cols)
    };
    let mut samples = Vec::with_capacity((filled as usize).saturating_mul(rows as usize));
    for sx in 0..filled {
        for sy in 0..rows {
            #[allow(clippy::cast_precision_loss)]
            let x = (sx as f32 + 0.5) / cols as f32;
            #[allow(clippy::cast_precision_loss)]
            let y = (sy as f32 + 0.5) / rows as f32;
            samples.push(ShadeSample {
                x,
                y,
                luma: 1.0,
                color,
            });
        }
    }
    samples
}

/// The bit-reversal order spreads low densities over the strip instead of
/// making them look like miniature bars. Full-height activity still fills every
/// sample. The live compact/full heights are four/eight samples respectively;
/// other heights use a total linear order as a safe fallback.
fn dither_rank(row: u32, rows: u32) -> u32 {
    const FOUR: [u32; 4] = [0, 2, 1, 3];
    const EIGHT: [u32; 8] = [0, 4, 2, 6, 1, 5, 3, 7];
    match rows {
        4 => FOUR.get(row as usize).copied().unwrap_or(row),
        8 => EIGHT.get(row as usize).copied().unwrap_or(row),
        _ => row,
    }
}

/// A unit-square rectangle using terminal/top-down Y coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct UnitRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// One positive weighted leaf in the ordered treemap.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TreemapLeaf {
    pub index: usize,
    pub value: f64,
    pub fraction: f64,
    pub rect: UnitRect,
}

/// Stable ordered-binary treemap layout.
///
/// Positive leaves retain the caller's canonical order. Each recursion splits
/// the longest edge at the canonical prefix nearest half of the current weight,
/// so rectangles move continuously without value-sorted swaps.
#[must_use]
pub(crate) fn treemap_layout(values: &[f64]) -> Vec<TreemapLeaf> {
    let sanitized: Vec<(usize, f64)> = values
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(index, value)| {
            let value = if value.is_finite() {
                value.max(0.0)
            } else {
                0.0
            };
            (value > 0.0).then_some((index, value))
        })
        .collect();
    let scale = sanitized
        .iter()
        .map(|(_, value)| *value)
        .fold(0.0f64, f64::max);
    if scale <= 0.0 {
        return Vec::new();
    }
    let items: Vec<(usize, f64, f64)> = sanitized
        .into_iter()
        .map(|(index, value)| (index, value, value / scale))
        .collect();
    let total = items.iter().map(|(_, _, weight)| *weight).sum::<f64>();
    let mut leaves = Vec::with_capacity(items.len());
    layout_items(
        &items,
        UnitRect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        },
        total,
        &mut leaves,
    );
    leaves
}

fn layout_items(
    items: &[(usize, f64, f64)],
    rect: UnitRect,
    root_total: f64,
    out: &mut Vec<TreemapLeaf>,
) {
    if let [(index, value, weight)] = items {
        out.push(TreemapLeaf {
            index: *index,
            value: *value,
            fraction: *weight / root_total,
            rect,
        });
        return;
    }
    if items.is_empty() {
        return;
    }
    let total = items.iter().map(|(_, _, weight)| *weight).sum::<f64>();
    let mut prefix = 0.0f64;
    let mut best_at = 1usize;
    let mut best_diff = f64::INFINITY;
    for (idx, (_, _, weight)) in items.iter().take(items.len() - 1).enumerate() {
        prefix += *weight;
        let diff = (total / 2.0 - prefix).abs();
        if diff < best_diff {
            best_diff = diff;
            best_at = idx + 1;
        }
    }
    let (left, right) = items.split_at(best_at);
    let left_weight = left.iter().map(|(_, _, weight)| *weight).sum::<f64>();
    let ratio = (left_weight / total).clamp(0.0, 1.0);
    let (left_rect, right_rect) = if rect.w >= rect.h {
        let width = rect.w * ratio;
        (
            UnitRect { w: width, ..rect },
            UnitRect {
                x: rect.x + width,
                w: rect.w - width,
                ..rect
            },
        )
    } else {
        let height = rect.h * ratio;
        (
            UnitRect { h: height, ..rect },
            UnitRect {
                y: rect.y + height,
                h: rect.h - height,
                ..rect
            },
        )
    };
    layout_items(left, left_rect, root_total, out);
    layout_items(right, right_rect, root_total, out);
}

/// Fill treemap leaves at 2x4 resolution, leaving a one-subcell neutral gap on
/// internal boundaries. The gap is presentation only; layout areas remain the
/// exact quantitative contract.
#[must_use]
pub(crate) fn treemap_samples(
    leaves: &[TreemapLeaf],
    area: Rect,
    colors: &[Color],
) -> Vec<ShadeSample> {
    let width = u32::from(area.w).saturating_mul(SX);
    let height = u32::from(area.h).saturating_mul(SY);
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let half_x = 0.5 / f64::from(width);
    let half_y = 0.5 / f64::from(height);
    let mut samples = Vec::new();
    for sy in 0..height {
        for sx in 0..width {
            let x = (f64::from(sx) + 0.5) / f64::from(width);
            let y_top = (f64::from(sy) + 0.5) / f64::from(height);
            let Some(leaf) = leaves.iter().find(|leaf| contains(leaf.rect, x, y_top)) else {
                continue;
            };
            let rect = leaf.rect;
            let right = rect.x + rect.w;
            let bottom = rect.y + rect.h;
            let on_internal_edge = (rect.x > 0.0 && x - rect.x < half_x)
                || (right < 1.0 && right - x < half_x)
                || (rect.y > 0.0 && y_top - rect.y < half_y)
                || (bottom < 1.0 && bottom - y_top < half_y);
            if on_internal_edge {
                continue;
            }
            samples.push(ShadeSample {
                x: x as f32,
                y: (1.0 - y_top) as f32,
                luma: TREEMAP_LUMA,
                color: colors.get(leaf.index).copied().unwrap_or(Color::Default),
            });
        }
    }
    samples
}

fn contains(rect: UnitRect, x: f64, y: f64) -> bool {
    x >= rect.x && x < rect.x + rect.w && y >= rect.y && y < rect.y + rect.h
}

/// Quantize a unit leaf to terminal cells for optional label placement.
#[must_use]
pub(crate) fn treemap_cell_rect(leaf: TreemapLeaf, area: Rect) -> Rect {
    let left = (leaf.rect.x * f64::from(area.w)).floor();
    let top = (leaf.rect.y * f64::from(area.h)).floor();
    let right = ((leaf.rect.x + leaf.rect.w) * f64::from(area.w)).ceil();
    let bottom = ((leaf.rect.y + leaf.rect.h) * f64::from(area.h)).ceil();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let left = left.clamp(0.0, f64::from(area.w)) as u16;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let top = top.clamp(0.0, f64::from(area.h)) as u16;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let right = right.clamp(0.0, f64::from(area.w)) as u16;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let bottom = bottom.clamp(0.0, f64::from(area.h)) as u16;
    Rect {
        x: area.x + left,
        y: area.y + top,
        w: right.saturating_sub(left),
        h: bottom.saturating_sub(top),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn narrow_heat_strip_max_pools_and_keeps_window_edges() {
        let values: Vec<u64> = (1..=30).collect();
        let strip = heat_strip(
            &values,
            Rect {
                x: 0,
                y: 0,
                w: 5,
                h: 1,
            },
            Color::Default,
        );
        assert_eq!(strip.columns.len(), 10);
        assert_eq!(strip.columns.first().copied(), Some(3));
        assert_eq!(strip.columns.last().copied(), Some(30));
        assert_eq!(strip.max, 30);
    }

    #[test]
    fn heat_density_is_monotone_and_quiet_is_empty() {
        let area = Rect {
            x: 0,
            y: 0,
            w: 15,
            h: 1,
        };
        let quiet = heat_strip(&[0; 30], area, Color::Default);
        assert!(quiet.samples.is_empty());
        let low = heat_strip(&[1, 4], area, Color::Default);
        let high = heat_strip(&[4, 4], area, Color::Default);
        assert!(high.samples.len() >= low.samples.len());
    }

    proptest! {
        #[test]
        fn narrow_heat_resampling_preserves_the_window_maximum(
            values in prop::collection::vec(0u64..1_000_000, 30),
            width in 1u16..15,
        ) {
            let strip = heat_strip(
                &values,
                Rect { x: 0, y: 0, w: width, h: 1 },
                Color::Default,
            );
            prop_assert_eq!(strip.columns.iter().copied().max().unwrap_or(0), strip.max);
            prop_assert!(!strip.columns.is_empty());
        }

        #[test]
        fn treemap_is_bounded_non_overlapping_and_proportional(values in prop::array::uniform4(0u32..1_000_000)) {
            let values = values.map(f64::from);
            let leaves = treemap_layout(&values);
            for leaf in &leaves {
                prop_assert!(leaf.rect.x >= 0.0 && leaf.rect.y >= 0.0);
                prop_assert!(leaf.rect.x + leaf.rect.w <= 1.0 + 1e-9);
                prop_assert!(leaf.rect.y + leaf.rect.h <= 1.0 + 1e-9);
                let area = leaf.rect.w * leaf.rect.h;
                prop_assert!((area - leaf.fraction).abs() <= 1e-9);
            }
            for (i, a) in leaves.iter().enumerate() {
                for b in leaves.iter().skip(i + 1) {
                    let overlap_w = (a.rect.x + a.rect.w).min(b.rect.x + b.rect.w) - a.rect.x.max(b.rect.x);
                    let overlap_h = (a.rect.y + a.rect.h).min(b.rect.y + b.rect.h) - a.rect.y.max(b.rect.y);
                    prop_assert!(overlap_w <= 1e-9 || overlap_h <= 1e-9);
                }
            }
            let total_area: f64 = leaves.iter().map(|leaf| leaf.rect.w * leaf.rect.h).sum();
            if values.iter().any(|value| *value > 0.0) {
                prop_assert!((total_area - 1.0).abs() <= 1e-9);
            } else {
                prop_assert!(leaves.is_empty());
            }
        }
    }

    #[test]
    fn treemap_keeps_canonical_order_and_zero_has_no_inflation() {
        let leaves = treemap_layout(&[10.0, 0.0, 5.0, 1.0]);
        assert_eq!(
            leaves.iter().map(|leaf| leaf.index).collect::<Vec<_>>(),
            vec![0, 2, 3]
        );
        assert_eq!(
            leaves.iter().map(|leaf| leaf.value).collect::<Vec<_>>(),
            vec![10.0, 5.0, 1.0]
        );
        assert!(treemap_layout(&[0.0, f64::NAN, -1.0, 0.0]).is_empty());
    }

    #[test]
    fn treemap_supports_dynamic_model_counts_and_a_single_full_area_leaf() {
        let leaves = treemap_layout(&[30.0, 20.0, 10.0, 5.0, 3.0, 2.0, 1.0]);
        assert_eq!(leaves.len(), 7);
        assert_eq!(
            leaves.iter().map(|leaf| leaf.index).collect::<Vec<_>>(),
            (0..7).collect::<Vec<_>>()
        );
        let total_area: f64 = leaves.iter().map(|leaf| leaf.rect.w * leaf.rect.h).sum();
        assert!((total_area - 1.0).abs() <= 1e-9);

        let one = treemap_layout(&[0.0, 42.0, 0.0]);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].index, 1);
        assert_eq!(
            one[0].rect,
            UnitRect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0
            }
        );
        assert!((one[0].fraction - 1.0).abs() <= f64::EPSILON);
    }
}
