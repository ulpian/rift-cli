//! The weighted collapse-to-summary stack solver — the one contract the
//! column-3 panels and the column-2 card stack both size against (M55-08,
//! ADR-0024 §3).
//!
//! Distribute a fixed height across N items, each with a weight, an expanded
//! minimum, and a collapse priority. Deterministic and total:
//! * everything expanded when the minimums fit, surplus split by weight;
//! * under pressure, Low items collapse to one line first, then Medium, then
//!   High — a collapsed item keeps its headline (never omitted);
//! * when even N single lines do not fit, the **first** `height` items render
//!   collapsed and the rest are dropped (the only lossy tier, below N rows).
//!
//! Items are addressed by **index** — the solver is id-agnostic, so both the
//! `SectionId` panels and the `CardKind` cards share one implementation.

/// How reluctantly an item collapses under height pressure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Collapses first.
    Low,
    /// Collapses after every Low item.
    Medium,
    /// Collapses last.
    High,
}

/// One item's sizing registration.
#[derive(Clone, Copy, Debug)]
pub struct Spec {
    /// Relative share of surplus height (all items at their minimums first).
    pub weight: u16,
    /// The smallest useful expanded height (title + content).
    pub min_rows: u16,
    /// Collapse order under pressure.
    pub priority: Priority,
}

/// One solved slot: which item (by index), where it renders, and whether it
/// collapsed to its one-line headline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    /// The item's index in the input `specs`.
    pub index: usize,
    /// Row offset within the stack body.
    pub y: u16,
    /// Rows granted (1 when collapsed).
    pub rows: u16,
    /// Collapsed to a one-line summary.
    pub collapsed: bool,
}

/// Distribute `height` rows across `specs`. See the module docs for the tiers.
#[must_use]
#[allow(clippy::too_many_lines)] // one pass per tier + the exact-tiling spread
pub fn solve(specs: &[Spec], height: u16) -> Vec<Slot> {
    let n = u16::try_from(specs.len()).unwrap_or(u16::MAX);
    if height == 0 || specs.is_empty() {
        return Vec::new();
    }
    // Choose which priorities stay expanded: strongest set that fits.
    let tiers: [&[Priority]; 4] = [
        &[Priority::High, Priority::Medium, Priority::Low], // all expanded
        &[Priority::High, Priority::Medium],
        &[Priority::High],
        &[],
    ];
    let mut chosen: &[Priority] = &[];
    for tier in tiers {
        let need: u16 = specs
            .iter()
            .map(|s| {
                if tier.contains(&s.priority) {
                    s.min_rows
                } else {
                    1
                }
            })
            .fold(0, u16::saturating_add);
        if need <= height {
            chosen = tier;
            break;
        }
    }
    // Below N rows even the all-collapsed tier fails: first `height` only.
    let all_collapsed_fit = height >= n;
    if chosen.is_empty() && !all_collapsed_fit {
        return specs
            .iter()
            .take(usize::from(height))
            .enumerate()
            .map(|(i, _)| Slot {
                index: i,
                y: u16::try_from(i).unwrap_or(0),
                rows: 1,
                collapsed: true,
            })
            .collect();
    }

    // Base allocation: minimums for expanded, one line for collapsed.
    let mut rows: Vec<u16> = specs
        .iter()
        .map(|s| {
            if chosen.contains(&s.priority) {
                s.min_rows
            } else {
                1
            }
        })
        .collect();
    let used: u16 = rows.iter().copied().fold(0, u16::saturating_add);
    let surplus = height.saturating_sub(used);
    // Spread surplus over the expanded items by weight, round-robin so the split
    // is exact (no fractional rows, no gap at the bottom).
    let weights: Vec<u16> = specs
        .iter()
        .map(|s| {
            if chosen.contains(&s.priority) {
                s.weight
            } else {
                0
            }
        })
        .collect();
    let total_weight: u16 = weights.iter().copied().fold(0, u16::saturating_add);
    if total_weight > 0 {
        for (r, w) in rows.iter_mut().zip(&weights) {
            let share = u32::from(surplus) * u32::from(*w) / u32::from(total_weight);
            *r = r.saturating_add(u16::try_from(share).unwrap_or(0));
        }
        let mut leftover = height.saturating_sub(rows.iter().copied().fold(0, u16::saturating_add));
        let mut order: Vec<usize> = (0..rows.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(weights.get(i).copied().unwrap_or(0)));
        for i in order {
            if leftover == 0 {
                break;
            }
            if weights.get(i).copied().unwrap_or(0) > 0 {
                if let Some(r) = rows.get_mut(i) {
                    *r = r.saturating_add(1);
                    leftover -= 1;
                }
            }
        }
    }

    let mut out = Vec::with_capacity(specs.len());
    let mut y = 0u16;
    for (i, spec) in specs.iter().enumerate() {
        if y >= height {
            break; // all-collapsed tier taller than the stack: drop the tail
        }
        let granted = rows.get(i).copied().unwrap_or(1).min(height - y);
        out.push(Slot {
            index: i,
            y,
            rows: granted,
            collapsed: !chosen.contains(&spec.priority),
        });
        y = y.saturating_add(granted);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn spec(weight: u16, min_rows: u16, priority: Priority) -> Spec {
        Spec {
            weight,
            min_rows,
            priority,
        }
    }

    /// The col3-shaped six-item stack (mirrors `col3::SECTIONS` weights).
    fn six() -> Vec<Spec> {
        vec![
            spec(3, 4, Priority::High),
            spec(3, 4, Priority::High),
            spec(2, 3, Priority::High),
            spec(3, 4, Priority::Medium),
            spec(2, 3, Priority::Low),
            spec(1, 2, Priority::Low),
        ]
    }

    #[test]
    fn zero_height_and_empty_specs_are_empty() {
        assert!(solve(&six(), 0).is_empty());
        assert!(solve(&[], 40).is_empty());
    }

    #[test]
    fn tall_expands_everything_and_tiles_exactly() {
        let slots = solve(&six(), 40);
        assert_eq!(slots.len(), 6);
        assert!(slots.iter().all(|s| !s.collapsed));
        let mut y = 0;
        for (i, s) in slots.iter().enumerate() {
            assert_eq!(s.index, i);
            assert_eq!(s.y, y);
            y += s.rows;
        }
        assert!(y <= 40);
    }

    #[test]
    fn low_priority_collapses_first() {
        let slots = solve(&six(), 17);
        let collapsed: Vec<usize> = slots
            .iter()
            .filter(|s| s.collapsed)
            .map(|s| s.index)
            .collect();
        assert_eq!(collapsed, vec![4, 5], "the two Low items fold first");
    }

    #[test]
    fn n_rows_all_collapsed_below_n_drops_tail() {
        let all = solve(&six(), 6);
        assert_eq!(all.len(), 6);
        assert!(all.iter().all(|s| s.collapsed && s.rows == 1));
        let few = solve(&six(), 3);
        assert_eq!(few.len(), 3);
        assert_eq!(
            few.iter().map(|s| s.index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    fn spec_strategy() -> impl Strategy<Value = Spec> {
        (
            1u16..5,
            2u16..6,
            prop_oneof![
                Just(Priority::Low),
                Just(Priority::Medium),
                Just(Priority::High),
            ],
        )
            .prop_map(|(w, m, p)| spec(w, m, p))
    }

    proptest! {
        /// Total and exact-tiling for any item set and height.
        #[test]
        fn tiles_exactly_for_any_stack(
            specs in proptest::collection::vec(spec_strategy(), 1..8),
            h in 0u16..200,
        ) {
            let slots = solve(&specs, h);
            let mut y = 0u16;
            for (pos, s) in slots.iter().enumerate() {
                prop_assert_eq!(s.y, y);
                prop_assert!(s.rows >= 1);
                prop_assert_eq!(s.index, pos); // order preserved
                y = y.saturating_add(s.rows);
            }
            prop_assert!(y <= h);
            // At N+ rows every item is present (information, not fidelity).
            if h >= u16::try_from(specs.len()).unwrap() {
                prop_assert_eq!(slots.len(), specs.len());
            }
        }
    }
}
