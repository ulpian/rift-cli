//! What a pull's caller claimed to hold, carried on the serve event that
//! answered it so the audit writer can book it beside the serve's own row.
//!
//! A module of its own because the claim is a fact about the request, not
//! about any one kind of decision: three event kinds carry it, and the
//! invariant that keeps its counts adding up lives with the type.

use crate::MemdEvent;

/// How many held revisions a pull's caller named, and how many of them this
/// family had recorded reading — the facts that make an over-claim visible
/// beside the credit the pull booked.
///
/// Counts and the rule's word only, never a hash: the audit relation it lands
/// in carries identifiers and counts. The two counts are private so that
/// `recorded <= stated` holds for every value, and the third count is
/// derived rather than stored, so the three always add up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct PullClaim {
    /// Distinct revisions the caller said it held.
    stated: u32,
    /// Of those, the ones this family recorded a read of.
    recorded: u32,
    /// The rule the pull's figure was booked under.
    basis: PullClaimBasis,
}

/// The rule a pull that stated revisions was booked under, spelled as the
/// serving envelope spells it. Only the two rules a stated hash leads to: a
/// call that stated nothing makes no claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PullClaimBasis {
    /// The checkout was bound, so credit rested on the bytes and the record.
    StatedRecorded,
    /// No checkout was bound, so the pull booked zero.
    Unverified,
}

impl PullClaim {
    /// A claim over `stated` revisions, `recorded` of them read by this
    /// family. A recorded count past the stated one cannot describe one call,
    /// so it is held to the stated count rather than trusted.
    #[must_use]
    pub fn new(stated: u32, recorded: u32, basis: PullClaimBasis) -> Self {
        Self {
            stated,
            recorded: recorded.min(stated),
            basis,
        }
    }

    /// Distinct revisions the caller said it held.
    #[must_use]
    pub fn stated(self) -> u32 {
        self.stated
    }

    /// Of those, the ones this family recorded a read of.
    #[must_use]
    pub fn recorded(self) -> u32 {
        self.recorded
    }

    /// Of those, the ones no read in this family's record accounts for.
    #[must_use]
    pub fn unrecorded(self) -> u32 {
        self.stated.saturating_sub(self.recorded)
    }

    /// The rule the pull's figure was booked under.
    #[must_use]
    pub fn basis(self) -> PullClaimBasis {
        self.basis
    }
}

impl PullClaimBasis {
    /// The word the envelope and the audit relation carry.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StatedRecorded => "stated_recorded",
            Self::Unverified => "unverified",
        }
    }
}

impl MemdEvent {
    /// The same event carrying `claim`, on the three kinds a successful pull
    /// is booked as. Every other kind is returned unchanged: a refusal or an
    /// invalidation answered nothing the caller held, so it records no claim.
    #[must_use]
    pub fn with_pull_claim(self, claim: PullClaim) -> Self {
        match self {
            Self::ManifestServed {
                estimated_tokens_returned,
                estimated_tokens_avoided,
                ..
            } => Self::ManifestServed {
                estimated_tokens_returned,
                estimated_tokens_avoided,
                pull_claim: Some(claim),
            },
            Self::SummaryServed {
                handle,
                estimated_tokens_returned,
                estimated_tokens_avoided,
                ..
            } => Self::SummaryServed {
                handle,
                estimated_tokens_returned,
                estimated_tokens_avoided,
                pull_claim: Some(claim),
            },
            Self::PackBuilt {
                sections,
                total_tokens,
                estimated_tokens_returned,
                estimated_tokens_avoided,
                ..
            } => Self::PackBuilt {
                sections,
                total_tokens,
                estimated_tokens_returned,
                estimated_tokens_avoided,
                pull_claim: Some(claim),
            },
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemdEvent;

    fn manifest() -> MemdEvent {
        MemdEvent::ManifestServed {
            estimated_tokens_returned: 3,
            estimated_tokens_avoided: 4,
            pull_claim: None,
        }
    }

    fn envelope(event: &MemdEvent) -> serde_json::Value {
        serde_json::from_str(&event.to_hook_json_keyed("k")).unwrap()
    }

    /// The envelope a manifest serve with these figures had before a claim
    /// could ride on it, byte for byte: the key hashes the three-member
    /// payload and the occurrence, so an absent claim moves neither.
    #[test]
    fn a_serve_with_no_claim_keeps_the_key_and_payload_it_always_had() {
        assert_eq!(
            manifest().to_hook_json_keyed("k"),
            "{\"idempotency_key\":\
             \"326806172073cf2426c6bde5be8430d65cede4121656bf06ea5c94f46f95cf2d\",\
             \"payload\":{\"estimated_tokens_avoided\":4,\"estimated_tokens_returned\":3,\
             \"kind\":\"manifest_served\"},\"schema\":1,\"verb\":\"memd.manifest_served\"}"
        );
    }

    #[test]
    fn a_pull_claim_rides_in_the_payload_as_counts_and_a_word() {
        let claim = PullClaim::new(3, 2, PullClaimBasis::StatedRecorded);
        let doc = envelope(&manifest().with_pull_claim(claim));
        assert_eq!(
            doc["payload"]["pull_claim"],
            serde_json::json!({ "stated": 3, "recorded": 2, "basis": "stated_recorded" })
        );
        assert_ne!(
            doc["idempotency_key"],
            envelope(&manifest())["idempotency_key"]
        );
    }

    #[test]
    fn a_refusal_and_an_invalidation_take_no_claim() {
        let claim = PullClaim::new(1, 1, PullClaimBasis::Unverified);
        let refused = MemdEvent::Refused {
            target: "src/a.rs".to_owned(),
            reason: "path_not_found".to_owned(),
            estimated_tokens_returned: 5,
        };
        let invalidated = MemdEvent::Invalidated {
            handle: "summary:src/a.rs".to_owned(),
            estimated_tokens_returned: 5,
        };
        assert_eq!(refused.clone().with_pull_claim(claim), refused);
        assert_eq!(invalidated.clone().with_pull_claim(claim), invalidated);
    }

    #[test]
    fn every_claimed_kind_carries_the_claim_and_keeps_its_figures() {
        let claim = PullClaim::new(2, 0, PullClaimBasis::Unverified);
        let summary = MemdEvent::SummaryServed {
            handle: "summary:src/a.rs".to_owned(),
            estimated_tokens_returned: 1,
            estimated_tokens_avoided: 2,
            pull_claim: None,
        };
        let pack = MemdEvent::PackBuilt {
            sections: 1,
            total_tokens: 9,
            estimated_tokens_returned: 1,
            estimated_tokens_avoided: 2,
            pull_claim: None,
        };
        for event in [manifest(), summary, pack] {
            let claimed = event.clone().with_pull_claim(claim);
            assert_eq!(claimed.tokens_avoided(), event.tokens_avoided());
            assert_eq!(claimed.tokens_returned(), event.tokens_returned());
            assert_eq!(
                envelope(&claimed)["payload"]["pull_claim"]["basis"],
                "unverified"
            );
        }
    }

    proptest::proptest! {
        /// Whatever counts a caller hands in, the three always add up and the
        /// recorded count never passes the stated one.
        #[test]
        fn the_stated_count_is_the_recorded_plus_the_unrecorded(
            stated in 0u32..10_000,
            recorded in 0u32..20_000,
        ) {
            let claim = PullClaim::new(stated, recorded, PullClaimBasis::StatedRecorded);
            proptest::prop_assert!(claim.recorded() <= claim.stated());
            proptest::prop_assert_eq!(claim.stated(), claim.recorded() + claim.unrecorded());
        }
    }
}
