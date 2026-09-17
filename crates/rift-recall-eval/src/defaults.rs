//! The versioned, provenance-tracked `EvalDefaults`:
//! per-model decisive-fact placement + tail-repeat strength, the saturation
//! point (`max_prior_sessions`), the query-aware lift, and real-run savings
//! percentiles — every field stamped with the sweep-config version and code
//! version that produced it. Derivation replays OFFLINE from the recorded
//! trial fixtures (re-running analysis needs no API key). Unprofiled or
//! unknown models get the conservative fallback (`Duplicated` +
//! `max_prior_sessions = 2`), and a **pre-sweep** artifact carries exactly
//! that fallback with its provenance saying so — the gate asserts existence
//! + provenance, never values.

use std::collections::BTreeMap;

use crate::sweep::{Position, TrialRecord};

/// The success-rate bar a saturation level must hold for
/// `max_prior_sessions` to include it.
pub const SATURATION_SUCCESS_BAR: f64 = 0.8;

/// The conservative fallback placement for unprofiled models.
pub const FALLBACK_PLACEMENT: Position = Position::Duplicated;

/// The conservative fallback saturation ceiling.
pub const FALLBACK_MAX_PRIOR_SESSIONS: u32 = 2;

/// The default tail-repeat strength (tokens) — the pack spine's both-ends
/// remedy sizing; sweep-derived per model when the curve favors Duplicated.
pub const FALLBACK_TAIL_REPEAT_TOKENS: u32 = 150;

/// One model's derived defaults.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelDefaults {
    /// The provider model id.
    pub model: String,
    /// The success-maximizing decisive-fact placement over the swept curve.
    pub placement: Position,
    /// Tail-repeat strength in tokens (stronger when the curve says the
    /// model loses mid-context facts).
    pub tail_repeat_tokens: u32,
    /// The saturation point: the largest swept prior-session count whose
    /// success rate held the bar (every smaller level holding it too).
    pub max_prior_sessions: u32,
    /// Query-aware success minus bare success, percentage points.
    pub query_aware_lift_pct: f64,
}

/// The committed artifact (`eval-defaults.json`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EvalDefaults {
    /// Artifact version (bump on re-derivation).
    pub version: u32,
    /// The sweep config that produced the values.
    pub sweep_config_version: u32,
    /// The code version that derived them.
    pub code_version: String,
    /// `derived-from-sweep` or `pre-sweep-conservative-fallback`.
    pub provenance: String,
    /// Per-model rows (empty in the pre-sweep artifact).
    pub per_model: Vec<ModelDefaults>,
    /// The placement any unprofiled/unknown model falls to.
    pub fallback_placement: Position,
    /// The saturation ceiling any unprofiled/unknown model falls to.
    pub fallback_max_prior_sessions: u32,
    /// The fallback tail-repeat strength.
    pub fallback_tail_repeat_tokens: u32,
}

impl EvalDefaults {
    /// The pre-sweep artifact: fallback-only, provenance saying exactly that
    /// (the values are the conservative defaults, not measurements — the
    /// one paid sweep run, when it happens, replaces this with `derived-from-sweep`).
    #[must_use]
    pub fn pre_sweep(sweep_config_version: u32, code_version: &str) -> Self {
        Self {
            version: 0,
            sweep_config_version,
            code_version: code_version.to_string(),
            provenance: "pre-sweep-conservative-fallback".to_string(),
            per_model: Vec::new(),
            fallback_placement: FALLBACK_PLACEMENT,
            fallback_max_prior_sessions: FALLBACK_MAX_PRIOR_SESSIONS,
            fallback_tail_repeat_tokens: FALLBACK_TAIL_REPEAT_TOKENS,
        }
    }
}

/// Success rate over a filtered trial subset (`None` when empty).
fn rate(trials: &[&TrialRecord]) -> Option<f64> {
    if trials.is_empty() {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    Some(trials.iter().filter(|t| t.success).count() as f64 / trials.len() as f64)
}

/// The saturation point: the largest swept level such that EVERY level up to
/// and including it held the bar (a prefix property — the derivation never
/// skips over a failing level to a luckier larger one). No passing prefix ⇒
/// the conservative fallback.
fn saturation_point(by_level: &BTreeMap<u32, Vec<&TrialRecord>>) -> u32 {
    let mut point = None;
    for (&level, trials) in by_level {
        match rate(trials) {
            Some(r) if r >= SATURATION_SUCCESS_BAR => point = Some(level),
            _ => break,
        }
    }
    point.unwrap_or(FALLBACK_MAX_PRIOR_SESSIONS)
}

/// Derive per-model defaults from recorded trials (offline — fixtures only).
/// Models present in the trials get measured rows; everything else falls to
/// the artifact's fallback fields.
#[must_use]
pub fn derive_defaults(
    trials: &[TrialRecord],
    sweep_config_version: u32,
    code_version: &str,
) -> EvalDefaults {
    let mut by_model: BTreeMap<&str, Vec<&TrialRecord>> = BTreeMap::new();
    for t in trials {
        by_model.entry(&t.cell.model).or_default().push(t);
    }
    let per_model = by_model
        .into_iter()
        .map(|(model, rows)| {
            // Success-maximizing placement (ties break toward the SAFER
            // Duplicated via iteration order: ALL lists it last, and `>=`
            // keeps the later winner on a tie).
            let mut placement = FALLBACK_PLACEMENT;
            let mut best = -1.0f64;
            for p in Position::ALL {
                let subset: Vec<&TrialRecord> = rows
                    .iter()
                    .copied()
                    .filter(|t| t.cell.position == p)
                    .collect();
                if let Some(r) = rate(&subset) {
                    if r >= best {
                        best = r;
                        placement = p;
                    }
                }
            }
            // Tail strength: a model whose Middle curve collapses needs the
            // strong both-ends tail; one that holds mid-context can run the
            // light tail.
            let middle: Vec<&TrialRecord> = rows
                .iter()
                .copied()
                .filter(|t| t.cell.position == Position::Middle)
                .collect();
            let tail_repeat_tokens = match rate(&middle) {
                Some(r) if r >= SATURATION_SUCCESS_BAR => 100,
                _ => 250,
            };
            // Saturation point over query-aware trials at the chosen placement.
            let mut by_level: BTreeMap<u32, Vec<&TrialRecord>> = BTreeMap::new();
            for t in rows.iter().filter(|t| t.cell.position == placement) {
                by_level.entry(t.cell.saturation).or_default().push(t);
            }
            let max_prior_sessions = saturation_point(&by_level);
            // The lift evidence: query-aware minus bare, percentage points.
            let aware: Vec<&TrialRecord> = rows
                .iter()
                .copied()
                .filter(|t| t.cell.query_aware)
                .collect();
            let bare: Vec<&TrialRecord> = rows
                .iter()
                .copied()
                .filter(|t| !t.cell.query_aware)
                .collect();
            let query_aware_lift_pct =
                (rate(&aware).unwrap_or(0.0) - rate(&bare).unwrap_or(0.0)) * 100.0;
            ModelDefaults {
                model: model.to_string(),
                placement,
                tail_repeat_tokens,
                max_prior_sessions,
                query_aware_lift_pct,
            }
        })
        .collect();
    EvalDefaults {
        version: 1,
        sweep_config_version,
        code_version: code_version.to_string(),
        provenance: "derived-from-sweep".to_string(),
        per_model,
        fallback_placement: FALLBACK_PLACEMENT,
        fallback_max_prior_sessions: FALLBACK_MAX_PRIOR_SESSIONS,
        fallback_tail_repeat_tokens: FALLBACK_TAIL_REPEAT_TOKENS,
    }
}

/// Resolve the effective defaults for a client model id: a measured row when
/// the sweep profiled it, the conservative fallback otherwise (an unswept
/// model is never guessed at; the `.rift` config hint wins upstream of this call).
#[must_use]
pub fn defaults_for(artifact: &EvalDefaults, model_id: &str) -> ModelDefaults {
    artifact
        .per_model
        .iter()
        .find(|m| m.model == model_id)
        .cloned()
        .unwrap_or(ModelDefaults {
            model: model_id.to_string(),
            placement: artifact.fallback_placement,
            tail_repeat_tokens: artifact.fallback_tail_repeat_tokens,
            max_prior_sessions: artifact.fallback_max_prior_sessions,
            query_aware_lift_pct: 0.0,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sweep::TrialCell;

    fn trial(
        model: &str,
        position: Position,
        saturation: u32,
        query_aware: bool,
        success: bool,
    ) -> TrialRecord {
        TrialRecord {
            cell: TrialCell {
                model: model.into(),
                position,
                saturation,
                query_aware,
                trial: 0,
            },
            pack_sha256: "p".into(),
            question: "q".into(),
            response: String::new(),
            success,
            tokens_in: 100,
            tokens_out: 10,
            cost_micro_usd: 0,
        }
    }

    #[test]
    fn pre_sweep_artifact_is_fallback_only_with_honest_provenance() {
        let a = EvalDefaults::pre_sweep(1, "0.0.1");
        assert_eq!(a.version, 0);
        assert_eq!(a.provenance, "pre-sweep-conservative-fallback");
        assert!(a.per_model.is_empty());
        assert_eq!(a.fallback_placement, Position::Duplicated);
        assert_eq!(a.fallback_max_prior_sessions, 2);
        // Round-trips as the committed artifact.
        let json = serde_json::to_string_pretty(&a).unwrap();
        assert_eq!(serde_json::from_str::<EvalDefaults>(&json).unwrap(), a);
        // Any model resolves to the fallback.
        let d = defaults_for(&a, "unknown-model");
        assert_eq!(d.placement, Position::Duplicated);
        assert_eq!(d.max_prior_sessions, 2);
    }

    #[test]
    fn derivation_picks_the_success_maximizing_placement() {
        // Begin wins 100%, everything else loses.
        let mut trials = Vec::new();
        for p in Position::ALL {
            for sat in [1u32, 2] {
                trials.push(trial("m1", p, sat, true, p == Position::Begin));
            }
        }
        let d = derive_defaults(&trials, 1, "0.0.1");
        assert_eq!(d.provenance, "derived-from-sweep");
        assert_eq!(d.per_model.len(), 1);
        assert_eq!(d.per_model[0].placement, Position::Begin);
        // Middle collapsed ⇒ the strong tail.
        assert_eq!(d.per_model[0].tail_repeat_tokens, 250);
    }

    #[test]
    fn a_tie_breaks_toward_duplicated() {
        // Begin and Duplicated both at 100%: the safer Duplicated wins.
        let trials = vec![
            trial("m1", Position::Begin, 1, true, true),
            trial("m1", Position::Duplicated, 1, true, true),
            trial("m1", Position::Middle, 1, true, false),
            trial("m1", Position::End, 1, true, false),
        ];
        let d = derive_defaults(&trials, 1, "0.0.1");
        assert_eq!(d.per_model[0].placement, Position::Duplicated);
    }

    proptest::proptest! {
        /// The saturation point is prefix-monotone (the monotonicity
        /// property): every swept level UP TO the derived point held the
        /// bar — the derivation can never skip a failing level to reach a
        /// luckier larger one.
        #[test]
        fn saturation_point_is_prefix_monotone(successes in proptest::collection::vec(0u8..=3, 4)) {
            // Levels 1,2,5,10 with 0..=3 successes out of 3 trials each.
            let levels = [1u32, 2, 5, 10];
            let mut trials = Vec::new();
            for (i, &level) in levels.iter().enumerate() {
                for t in 0..3u8 {
                    trials.push(trial(
                        "m",
                        Position::Duplicated,
                        level,
                        true,
                        t < successes[i],
                    ));
                }
            }
            let d = derive_defaults(&trials, 1, "test");
            let point = d.per_model[0].max_prior_sessions;
            // Either the fallback (no passing prefix), or every level up to
            // the point passed the bar.
            if point != FALLBACK_MAX_PRIOR_SESSIONS || levels.iter().position(|&l| l == point).is_some_and(|i| (0..=i).all(|j| f64::from(successes[j]) / 3.0 >= SATURATION_SUCCESS_BAR)) {
                for (i, &level) in levels.iter().enumerate() {
                    if level <= point {
                        proptest::prop_assert!(
                            f64::from(successes[i]) / 3.0 >= SATURATION_SUCCESS_BAR || point == FALLBACK_MAX_PRIOR_SESSIONS,
                            "level {level} under the point must have held the bar"
                        );
                    }
                }
            }
        }
    }
}
