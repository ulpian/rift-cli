//! The `load-bearing` leg, which is not built and says so.

use super::Leg;

/// **Not built, and this is the honest report of why.**
///
/// The plan for this gate carried a seventh leg that would have checked a
/// registry file listing each load-bearing test by name. The owner ruled that
/// the program's registers are its own chronology and its decision records, and
/// that no such file is to be created; the tests are registered by name where
/// the work that added them is recorded.
///
/// The leg is kept, printing that, rather than deleted: a leg that vanishes
/// reads as a leg that passed.
#[must_use]
pub fn load_bearing() -> Leg {
    Leg::new(
        "load-bearing",
        "not built (registers live in the program chronology, by owner decision)".to_owned(),
        Vec::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The leg that is deliberately absent says so, passes, and is still
    /// among the legs the command prints: a leg dropped from the command
    /// reads as a leg that passed.
    #[test]
    fn the_unbuilt_leg_reports_itself_rather_than_disappearing() {
        let leg = load_bearing();
        assert!(leg.violations.is_empty());
        assert!(leg.detail.contains("not built"), "{}", leg.detail);
        let legs =
            crate::gate_intercept::run_all(&crate::workspace_root()).expect("every leg runs");
        assert!(
            legs.iter().any(|l| l.name == leg.name),
            "the command still prints `{}`",
            leg.name
        );
    }
}
