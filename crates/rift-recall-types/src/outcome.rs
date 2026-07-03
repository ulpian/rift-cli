//! Outcome-label vocabulary (Stage 3 Phase 4 Step 2, weakness W8).
//!
//! Ranking is "learned from what actually worked": a candidate whose provenance
//! merged cleanly ranks up; one that was reverted or abandoned ranks down. The
//! labels are **derived from the git lifecycle** (reachability, revert messages,
//! branch state) — never from an agent's assertion — and stored append-only and
//! *bitemporally* (`valid_from` = when it became true in the world,
//! `observed_at` = when Rift learned it) in the `outcome_label` relation, so a
//! corrected belief is a new superseding row, never a mutation.
//!
//! This module is the shared kind vocabulary + its signed ranking term, so
//! [`rift-recall-rank`](../../rift-recall-rank) and the derivation/store in
//! [`rift-recall-ledger`](../../rift-recall-ledger) agree on what each label
//! means.

/// What an outcome label is *about* (the subject of the belief).
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelSubject {
    /// A capture session.
    Session(String),
    /// A checkpoint (the v1 default subject).
    Checkpoint(String),
    /// A specific `path@content` file revision.
    FileRevision(String),
}

impl LabelSubject {
    /// The stable wire tag for the subject kind (matches `outcome_label.subject_kind`).
    #[must_use]
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Session(_) => "session",
            Self::Checkpoint(_) => "checkpoint",
            Self::FileRevision(_) => "file_revision",
        }
    }

    /// The subject id.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Session(s) | Self::Checkpoint(s) | Self::FileRevision(s) => s,
        }
    }
}

/// The kind of outcome a candidate's provenance had. Derived from git/CI/
/// attribution, **never** from an agent's claim.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    /// Reachable from the integration branch — the work landed.
    Merged,
    /// A test signal recorded the work passing (deferred derivation; needs CI).
    TestsPassed,
    /// A later commit reverted/undid the work (within the revert window).
    Reverted,
    /// Branch deleted or long-idle and never merged.
    Abandoned,
    /// Agent output survived with little human rewrite (deferred; needs attribution).
    LowHumanMod,
    /// Humans heavily rewrote the agent output (deferred; needs attribution).
    HighHumanMod,
}

/// The closed set of outcome-label kind strings (mirrors the `outcome_label.kind`
/// CHECK set in `rift-store`).
pub const OUTCOME_KINDS: &[&str] = &[
    "merged",
    "tests_passed",
    "reverted",
    "abandoned",
    "low_human_mod",
    "high_human_mod",
];

impl OutcomeKind {
    /// The stable wire string (matches the `outcome_label.kind` CHECK set).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Merged => "merged",
            Self::TestsPassed => "tests_passed",
            Self::Reverted => "reverted",
            Self::Abandoned => "abandoned",
            Self::LowHumanMod => "low_human_mod",
            Self::HighHumanMod => "high_human_mod",
        }
    }

    /// Parse a wire string back to a kind.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "merged" => Some(Self::Merged),
            "tests_passed" => Some(Self::TestsPassed),
            "reverted" => Some(Self::Reverted),
            "abandoned" => Some(Self::Abandoned),
            "low_human_mod" => Some(Self::LowHumanMod),
            "high_human_mod" => Some(Self::HighHumanMod),
            _ => None,
        }
    }

    /// The signed base term in `[-1, 1]`: positive = "trust this, it worked and
    /// stuck"; negative = "down-weight, it didn't hold". The strongest negative
    /// (`Reverted`, `-1.0`) can bury a candidate below unlabeled work (W8).
    #[must_use]
    pub fn base_term(self) -> f32 {
        match self {
            Self::Merged => 0.6,
            Self::TestsPassed => 0.5,
            Self::LowHumanMod => 0.4,
            Self::HighHumanMod => -0.4,
            Self::Abandoned => -0.7,
            Self::Reverted => -1.0,
        }
    }

    /// Whether this label is a positive (trust-increasing) signal.
    #[must_use]
    pub fn is_positive(self) -> bool {
        self.base_term() > 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_string_roundtrips_and_matches_check_set() {
        for &s in OUTCOME_KINDS {
            let k = OutcomeKind::parse(s).expect("known kind");
            assert_eq!(k.as_str(), s);
        }
        assert!(OutcomeKind::parse("bogus").is_none());
    }

    #[test]
    fn base_terms_are_bounded_and_signed() {
        for &s in OUTCOME_KINDS {
            let k = OutcomeKind::parse(s).unwrap();
            assert!((-1.0..=1.0).contains(&k.base_term()));
        }
        assert!(OutcomeKind::Merged.is_positive());
        assert!(!OutcomeKind::Reverted.is_positive());
        assert!(OutcomeKind::Reverted.base_term() < OutcomeKind::Abandoned.base_term());
    }

    #[test]
    fn subject_kind_and_id_are_exposed() {
        let s = LabelSubject::Checkpoint("c1".into());
        assert_eq!(s.kind_str(), "checkpoint");
        assert_eq!(s.id(), "c1");
    }
}
