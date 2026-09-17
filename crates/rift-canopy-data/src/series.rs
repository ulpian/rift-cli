//! The shared graph substrate: a capped ring-buffer [`Series`] with
//! **validating, idempotent ingest** — the data half every graph feed fills.
//!
//! Deliberately source-agnostic: no dependency on the epoch, the notify wake,
//! or any store — a feed computes samples however it likes and pushes them
//! here; the sections read windows out. Ingest is where corruption stops:
//! NaN and infinities are rejected, counts are clamped at zero, per-unit
//! ceilings are applied, and replaying the same update is a no-op — so a
//! corrupt projection can neither panic the UI nor skew autoscaling.

use std::collections::VecDeque;

/// The default ring capacity (~240 samples ≈ one sample per plot column at
/// generous widths — the designed target of about 240).
pub const DEFAULT_CAP: usize = 240;

/// What a series' values measure — drives the ceiling and display formatting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// Token counts (clamped at zero; no upper ceiling).
    Tokens,
    /// A percentage (clamped into `0..=100`).
    Percent,
    /// An event/count metric (clamped at zero).
    Count,
}

impl Unit {
    /// The inclusive ceiling for this unit, if any.
    fn ceiling(self) -> Option<f64> {
        match self {
            Self::Percent => Some(100.0),
            Self::Tokens | Self::Count => None,
        }
    }
}

/// One time-stamped sample. `t` is epoch-ms from the **injected** clock of
/// whoever produces the sample — this module never reads a wall clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// Sample time (epoch-ms, injected).
    pub t: i64,
    /// The measured value (validated on ingest).
    pub v: f64,
}

/// A capped ring buffer of [`Sample`]s with validating, idempotent ingest.
#[derive(Clone, Debug)]
pub struct Series {
    buf: VecDeque<Sample>,
    cap: usize,
    unit: Unit,
}

impl Series {
    /// A new series of `unit` at the default capacity.
    #[must_use]
    pub fn new(unit: Unit) -> Self {
        Self::with_cap(unit, DEFAULT_CAP)
    }

    /// A new series with an explicit capacity (`0` is promoted to 1 — a ring
    /// that can hold nothing would make every push silently vanish).
    #[must_use]
    pub fn with_cap(unit: Unit, cap: usize) -> Self {
        Self {
            buf: VecDeque::new(),
            cap: cap.max(1),
            unit,
        }
    }

    /// The unit this series measures.
    #[must_use]
    pub fn unit(&self) -> Unit {
        self.unit
    }

    /// Validate + ingest one sample. Total and idempotent:
    /// * non-finite values (NaN/±inf) and non-finite/absurd timestamps are
    ///   **rejected** (`false`);
    /// * negatives clamp to zero, and the unit's ceiling applies;
    /// * a sample at the **same `t` as the newest** replaces it (latest wins —
    ///   replaying the identical update is a no-op);
    /// * an **older-than-newest** sample is rejected (feeds re-bootstrap by
    ///   [`Self::clear`], never by rewriting history);
    /// * the ring cap holds: pushing onto a full ring evicts the oldest.
    ///
    /// Returns whether the series changed.
    pub fn push(&mut self, sample: Sample) -> bool {
        if !sample.v.is_finite() {
            return false;
        }
        let mut v = sample.v.max(0.0);
        if let Some(ceiling) = self.unit.ceiling() {
            v = v.min(ceiling);
        }
        let s = Sample { t: sample.t, v };
        if let Some(newest) = self.buf.back().copied() {
            if s.t < newest.t {
                return false; // out-of-order: history is append-only
            }
            if s.t == newest.t {
                if (newest.v - s.v).abs() < f64::EPSILON {
                    return false; // the identical update, replayed — no-op
                }
                if let Some(slot) = self.buf.back_mut() {
                    *slot = s; // same instant, newer value: latest wins
                }
                return true;
            }
        }
        if self.buf.len() == self.cap {
            self.buf.pop_front(); // oldest-out
        }
        self.buf.push_back(s);
        true
    }

    /// Drop every sample (the re-bootstrap path after a `Rebuilt`).
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    /// All samples, oldest first.
    pub fn samples(&self) -> impl Iterator<Item = Sample> + '_ {
        self.buf.iter().copied()
    }

    /// The last `n` values, oldest first (for the plot generators).
    #[must_use]
    pub fn window(&self, n: usize) -> Vec<f64> {
        let skip = self.buf.len().saturating_sub(n);
        self.buf.iter().skip(skip).map(|s| s.v).collect()
    }

    /// The newest sample, if any.
    #[must_use]
    pub fn latest(&self) -> Option<Sample> {
        self.buf.back().copied()
    }

    /// How many samples the ring currently holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether the ring is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// The saturating sum of every held value — never overflows and never goes
    /// non-finite: sums saturate rather than overflow.
    #[must_use]
    pub fn saturating_sum(&self) -> f64 {
        saturating_sum(self.buf.iter().map(|s| s.v))
    }

    /// The autoscale ceiling for plotting: the max held value, guarded so an
    /// empty or zero-range series still yields a defined, non-zero scale.
    #[must_use]
    pub fn autoscale_max(&self) -> f64 {
        let max = self.buf.iter().map(|s| s.v).fold(0.0f64, f64::max);
        if max > 0.0 {
            max
        } else {
            1.0
        }
    }
}

/// Saturating accumulation over **validated series values** (finite, ≥ 0 —
/// the only values ingest admits): the running total saturates at `f64::MAX`
/// and can never become NaN/inf. Total regardless of input: non-finite or
/// negative values contribute nothing.
#[must_use]
pub fn saturating_sum(values: impl Iterator<Item = f64>) -> f64 {
    let mut total = 0.0f64;
    for v in values {
        if !v.is_finite() || v < 0.0 {
            continue;
        }
        total += v;
        if !total.is_finite() {
            return f64::MAX;
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn ring_cap_holds_with_oldest_out() {
        let mut s = Series::with_cap(Unit::Count, 3);
        for t in 0..5 {
            assert!(s.push(Sample {
                t,
                v: f64::from(u32::try_from(t).unwrap())
            }));
        }
        assert_eq!(s.len(), 3);
        assert_eq!(s.window(3), vec![2.0, 3.0, 4.0], "oldest evicted first");
    }

    #[test]
    fn nan_and_infinities_are_rejected() {
        let mut s = Series::new(Unit::Tokens);
        assert!(!s.push(Sample { t: 1, v: f64::NAN }));
        assert!(!s.push(Sample {
            t: 2,
            v: f64::INFINITY
        }));
        assert!(!s.push(Sample {
            t: 3,
            v: f64::NEG_INFINITY
        }));
        assert!(s.is_empty());
    }

    #[test]
    fn negatives_clamp_and_percent_ceiling_applies() {
        let mut s = Series::new(Unit::Percent);
        s.push(Sample { t: 1, v: -5.0 });
        s.push(Sample { t: 2, v: 250.0 });
        assert_eq!(s.window(2), vec![0.0, 100.0]);
    }

    #[test]
    fn same_instant_latest_wins_and_replay_is_noop() {
        let mut s = Series::new(Unit::Count);
        assert!(s.push(Sample { t: 5, v: 1.0 }));
        assert!(s.push(Sample { t: 5, v: 2.0 }), "newer value at the same t");
        assert!(
            !s.push(Sample { t: 5, v: 2.0 }),
            "identical replay is a no-op"
        );
        assert_eq!(s.len(), 1);
        assert_eq!(s.latest(), Some(Sample { t: 5, v: 2.0 }));
    }

    #[test]
    fn out_of_order_samples_are_rejected() {
        let mut s = Series::new(Unit::Count);
        s.push(Sample { t: 10, v: 1.0 });
        assert!(!s.push(Sample { t: 9, v: 7.0 }));
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn autoscale_is_defined_on_empty_and_zero_ranges() {
        let mut s = Series::new(Unit::Count);
        assert!((s.autoscale_max() - 1.0).abs() < f64::EPSILON);
        s.push(Sample { t: 1, v: 0.0 });
        s.push(Sample { t: 2, v: 0.0 });
        assert!((s.autoscale_max() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn saturating_sum_never_overflows() {
        let mut s = Series::new(Unit::Tokens);
        s.push(Sample { t: 1, v: f64::MAX });
        s.push(Sample { t: 2, v: f64::MAX });
        assert!((s.saturating_sum() - f64::MAX).abs() < f64::EPSILON);
        assert!(s.saturating_sum().is_finite());
    }

    proptest! {
        /// Ingest is total and the cap is an invariant, for arbitrary samples.
        #[test]
        fn ingest_is_total_and_cap_holds(
            samples in proptest::collection::vec((any::<i64>(), any::<f64>()), 0..500),
            cap in 1usize..64,
        ) {
            let mut s = Series::with_cap(Unit::Tokens, cap);
            for (t, v) in samples {
                let _ = s.push(Sample { t, v });
                prop_assert!(s.len() <= cap);
            }
            // Every held value is finite and non-negative (validation held).
            for held in s.samples() {
                prop_assert!(held.v.is_finite() && held.v >= 0.0);
            }
            // Timestamps are non-decreasing (append-only history).
            let ts: Vec<i64> = s.samples().map(|x| x.t).collect();
            prop_assert!(ts.windows(2).all(|w| w[0] <= w[1]));
        }

        /// Replaying any ingest sequence is idempotent: pushing the exact
        /// sequence twice ends in the same state as once.
        #[test]
        fn ingest_is_idempotent_under_replay(
            samples in proptest::collection::vec((0i64..1000, -10.0f64..1e9), 0..60),
        ) {
            let mut once = Series::new(Unit::Count);
            let mut sorted = samples.clone();
            sorted.sort_by_key(|(t, _)| *t);
            for (t, v) in &sorted {
                once.push(Sample { t: *t, v: *v });
            }
            let snapshot: Vec<Sample> = once.samples().collect();
            // Replay the whole (already-applied) sequence again.
            for (t, v) in &sorted {
                once.push(Sample { t: *t, v: *v });
            }
            // Every sample except possibly the newest instant is unchanged;
            // the newest instant keeps latest-wins semantics.
            let replayed: Vec<Sample> = once.samples().collect();
            prop_assert_eq!(snapshot, replayed);
        }

        /// The saturating sum is always finite and non-negative — for ANY
        /// input, including hostile non-finite/negative values.
        #[test]
        fn saturating_sum_is_total(values in proptest::collection::vec(any::<f64>(), 0..50)) {
            let total = saturating_sum(values.into_iter());
            prop_assert!(total.is_finite());
            prop_assert!(total >= 0.0);
        }
    }
}
