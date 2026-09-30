//! The four-state envelope builders: the only constructors of a served
//! artifact, and the place the false-memory guard and the budget ceiling meet.

use rift_recall_types::{ArtifactState, Envelope, OmitReason, Trust};

use crate::summary::{ValidSummary, Validity};

/// Whether the agent has already seen this exact content hash.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SeenStatus {
    /// The agent has seen this exact hash this turn/session.
    SeenSameHash {
        /// The turn the calling window last read this exact revision, when
        /// its own journal records one; `None` when the pull could not be
        /// attributed to a window or the window holds no such read. It rides
        /// on the claim rather than beside it because it is evidence for the
        /// claim, and a turn with no claim to support means nothing.
        last_seen_turn: Option<u64>,
    },
    /// Not seen, or seen at a different hash.
    NotSeenOrChanged,
}

impl SeenStatus {
    /// The turn a known-unchanged claim may cite, which only a sighting has.
    fn last_seen_turn(self) -> Option<u64> {
        match self {
            Self::SeenSameHash { last_seen_turn } => last_seen_turn,
            Self::NotSeenOrChanged => None,
        }
    }
}

/// A shared token budget for a response/pack.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    remaining: u32,
}
impl Budget {
    /// A budget with `tokens` remaining.
    #[must_use]
    pub fn new(tokens: u32) -> Self {
        Self { remaining: tokens }
    }
    /// Remaining tokens.
    #[must_use]
    pub fn remaining(self) -> u32 {
        self.remaining
    }
    fn fits(self, cost: u32) -> bool {
        cost <= self.remaining
    }
    fn charge(&mut self, cost: u32) {
        self.remaining = self.remaining.saturating_sub(cost);
    }
}

/// Build the four-state envelope for one artifact — the *single* way serving
/// constructs a per-artifact result.
///
/// `known_unchanged` is returned **only** when the agent saw the same hash *and*
/// the source is not known-changed — hash-equality is itself the proof, so a
/// bound summary is not required, while an `Invalid` verdict rules it out.
/// Only `summary_included`/`exact_included` charge budget.
#[must_use]
pub fn envelope_for(
    path: &str,
    trust: Trust,
    handle: &str,
    basis_sha256: &str,
    seen: SeenStatus,
    validity: &Validity,
    exact: Option<&str>,
    exact_allowed: bool,
    budget: &mut Budget,
) -> Envelope {
    let mk = |state, tokens| Envelope {
        path: path.to_string(),
        trust,
        state,
        estimated_tokens_returned: tokens,
    };
    let by_handle = |reason, basis| {
        mk(
            ArtifactState::AvailableByHandle {
                handle: handle.to_string(),
                basis_sha256: basis,
                reason,
            },
            0,
        )
    };

    // The false-memory guard comes first, so nothing below can outrank it.
    if is_known_unchanged(seen, validity) {
        return mk(
            ArtifactState::KnownUnchanged {
                handle: handle.to_string(),
                basis_sha256: basis_sha256.to_string(),
                last_seen_turn: seen.last_seen_turn(),
            },
            0,
        );
    }

    if let Validity::Valid(s) = validity {
        let summary = String::from_utf8_lossy(s.digest()).into_owned();
        let Some(cost) = charge_if_fits(budget, &summary) else {
            return by_handle(OmitReason::BudgetExhausted, Some(basis_sha256.to_string()));
        };
        return mk(
            ArtifactState::SummaryIncluded {
                handle: handle.to_string(),
                basis_sha256: basis_sha256.to_string(),
                summary,
                summary_key: s.key().to_string(),
            },
            cost,
        );
    }

    // Exact retrieval: only for trusted classes, when allowed, and in budget.
    if let Some(content) = exact_candidate(exact, exact_allowed, trust) {
        if let Some(cost) = charge_if_fits(budget, content) {
            return mk(
                ArtifactState::ExactIncluded {
                    handle: handle.to_string(),
                    basis_sha256: basis_sha256.to_string(),
                    content: content.to_string(),
                    range: None, // full-artifact exact; ranged exact is served via handles
                },
                cost,
            );
        }
    }

    let (reason, basis) = omission(validity, basis_sha256);
    by_handle(reason, basis)
}

/// Build the four-state envelope for one **manifest** entry (`rift_known_state`) —
/// the sealed constructor that path routes through, so no serve path
/// builds a manifest `ArtifactState` inline. Distinct from [`envelope_for`]
/// (which serves a *summary/exact* artifact and maps a `Degraded` source to
/// `SourceDeletedOrRedacted`): a manifest entry for a live revision with no bound
/// summary is `AvailableByHandle`/`NotRequested`, never "deleted".
///
/// * `seen` is `SeenSameHash` (agent already holds this exact hash) → `KnownUnchanged`
///   (zero bytes — the false-memory guard: hash-equality is the proof), citing
///   the turn the sighting carries.
/// * a `Valid` summary in budget → `SummaryIncluded`; over budget → `AvailableByHandle`
///   (`BudgetExhausted`).
/// * otherwise → `AvailableByHandle` (`NotRequested`).
///
/// `summary` can only be a [`ValidSummary`], which is unconstructible outside this
/// crate ([`resolve`](crate::resolve)/[`classify`](crate::classify) are its
/// sole mints), so a stale summary can never reach the `SummaryIncluded` arm.
#[must_use]
pub fn manifest_envelope(
    path: &str,
    trust: Trust,
    handle: &str,
    basis_sha256: &str,
    seen: SeenStatus,
    summary: Option<&ValidSummary>,
    budget: &mut Budget,
) -> Envelope {
    let mk = |state, tokens| Envelope {
        path: path.to_string(),
        trust,
        state,
        estimated_tokens_returned: tokens,
    };

    if let SeenStatus::SeenSameHash { last_seen_turn } = seen {
        return mk(
            ArtifactState::KnownUnchanged {
                handle: handle.to_string(),
                basis_sha256: basis_sha256.to_string(),
                last_seen_turn,
            },
            0,
        );
    }

    if let Some(s) = summary {
        let text = String::from_utf8_lossy(s.digest()).into_owned();
        let cost = estimate_tokens(&text);
        if budget.fits(cost) {
            budget.charge(cost);
            return mk(
                ArtifactState::SummaryIncluded {
                    handle: handle.to_string(),
                    basis_sha256: basis_sha256.to_string(),
                    summary: text,
                    summary_key: s.key().to_string(),
                },
                cost,
            );
        }
        return mk(
            ArtifactState::AvailableByHandle {
                handle: handle.to_string(),
                basis_sha256: Some(basis_sha256.to_string()),
                reason: OmitReason::BudgetExhausted,
            },
            0,
        );
    }

    mk(
        ArtifactState::AvailableByHandle {
            handle: handle.to_string(),
            basis_sha256: Some(basis_sha256.to_string()),
            reason: OmitReason::NotRequested,
        },
        0,
    )
}

/// The false-memory guard: known_unchanged requires same-hash-seen AND a
/// source that is not known-changed. Hash-equality (`SeenSameHash` == agent's
/// hash equals the current basis) is itself the proof the source is unchanged;
/// a `Valid` summary strengthens it but is not required (a plain file the agent
/// already holds is `known_unchanged` even with no bound summary). An `Invalid`
/// verdict — the bound source genuinely moved — is *never* known_unchanged.
///
/// The turn plays no part: it is evidence the claim may cite, so a missing turn
/// can never turn the claim off and a present one can never turn it on.
fn is_known_unchanged(seen: SeenStatus, validity: &Validity) -> bool {
    matches!(seen, SeenStatus::SeenSameHash { .. }) && !matches!(validity, Validity::Invalid { .. })
}

/// Charge the estimated cost of `text` when it fits and return that cost, so
/// an included artifact is charged exactly once and an unaffordable one never.
fn charge_if_fits(budget: &mut Budget, text: &str) -> Option<u32> {
    let cost = estimate_tokens(text);
    if !budget.fits(cost) {
        return None;
    }
    budget.charge(cost);
    Some(cost)
}

/// The content eligible for exact inline retrieval: an untrusted class never
/// is, whatever the caller allows, because its bytes are adversary-controlled.
fn exact_candidate(exact: Option<&str>, exact_allowed: bool, trust: Trust) -> Option<&str> {
    if !exact_allowed || !trust.allows_raw_inline() {
        return None;
    }
    exact
}

/// Why an artifact that is neither known-unchanged nor included is served by
/// handle, and the basis hash that may go with it: a deleted or redacted
/// source has no current basis hash to disclose.
fn omission(validity: &Validity, basis_sha256: &str) -> (OmitReason, Option<String>) {
    match validity {
        Validity::Invalid { .. } => (
            OmitReason::SummaryInvalidSourceChanged,
            Some(basis_sha256.to_string()),
        ),
        Validity::Degraded { .. } => (OmitReason::SourceDeletedOrRedacted, None),
        Validity::Valid(_) => (OmitReason::BudgetExhausted, Some(basis_sha256.to_string())),
    }
}

fn estimate_tokens(s: &str) -> u32 {
    u32::try_from(s.chars().count() / 4)
        .unwrap_or(u32::MAX)
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::summary::tests::versions;
    use crate::summary::{resolve, sha256_hex, MemoryDigestStore, SummaryStore, WorktreeSnapshot};

    #[test]
    fn changed_file_is_never_known_unchanged() {
        // If the source changed, validity is Invalid AND seen != SeenSameHash;
        // the envelope is never known_unchanged.
        let mut budget = Budget::new(1000);
        let env = envelope_for(
            "a.rs",
            Trust::CodeFact,
            "summary:k",
            "basis",
            SeenStatus::NotSeenOrChanged,
            &Validity::Invalid {
                key: "k".into(),
                now: "new".into(),
            },
            None,
            true,
            &mut budget,
        );
        assert!(!matches!(env.state, ArtifactState::KnownUnchanged { .. }));
        assert!(matches!(
            env.state,
            ArtifactState::AvailableByHandle {
                reason: OmitReason::SummaryInvalidSourceChanged,
                ..
            }
        ));
    }

    #[test]
    fn seen_and_valid_is_known_unchanged_with_zero_tokens() {
        let dg = MemoryDigestStore::default();
        let store = SummaryStore::new();
        let key = store.put_summary(&dg, "a.rs", b"src", versions(), b"digest");
        let mut snap = WorktreeSnapshot::new();
        snap.insert("a.rs".into(), sha256_hex(b"src"));
        let v = resolve(&store, &dg, &snap, &key);
        let mut budget = Budget::new(1000);
        let env = envelope_for(
            "a.rs",
            Trust::CodeFact,
            "summary:k",
            "basis",
            SeenStatus::SeenSameHash {
                last_seen_turn: None,
            },
            &v,
            None,
            true,
            &mut budget,
        );
        assert!(matches!(env.state, ArtifactState::KnownUnchanged { .. }));
        assert_eq!(env.estimated_tokens_returned, 0);
        assert_eq!(budget.remaining(), 1000, "known_unchanged charges nothing");
    }

    #[test]
    fn seen_same_hash_is_known_unchanged_even_without_a_summary() {
        // A plain file the agent already holds (hash match) is known_unchanged
        // even when no summary is bound (Degraded) — hash-equality is the proof.
        let mut budget = Budget::new(1000);
        let env = envelope_for(
            "a.rs",
            Trust::CodeFact,
            "file:a.rs@sha256:x",
            "basis",
            SeenStatus::SeenSameHash {
                last_seen_turn: None,
            },
            &Validity::Degraded { key: "k".into() },
            None,
            false,
            &mut budget,
        );
        assert!(matches!(env.state, ArtifactState::KnownUnchanged { .. }));
        assert_eq!(env.estimated_tokens_returned, 0);
    }

    #[test]
    fn seen_but_invalid_is_never_known_unchanged() {
        // If the bound source genuinely moved (Invalid), the entry is never
        // known_unchanged regardless of the seen flag (the guard's hard edge).
        let mut budget = Budget::new(1000);
        let env = envelope_for(
            "a.rs",
            Trust::CodeFact,
            "summary:k",
            "basis",
            SeenStatus::SeenSameHash {
                last_seen_turn: None,
            },
            &Validity::Invalid {
                key: "k".into(),
                now: "new".into(),
            },
            None,
            false,
            &mut budget,
        );
        assert!(!matches!(env.state, ArtifactState::KnownUnchanged { .. }));
    }

    #[test]
    fn manifest_envelope_covers_the_four_states() {
        let dg = MemoryDigestStore::default();
        let store = SummaryStore::new();
        let key = store.put_summary(&dg, "a.rs", b"src", versions(), b"digest");
        let mut snap = WorktreeSnapshot::new();
        snap.insert("a.rs".into(), sha256_hex(b"src"));
        let valid = resolve(&store, &dg, &snap, &key);
        let Validity::Valid(vs) = &valid else {
            panic!("expected Valid");
        };

        // seen → known_unchanged (no bytes)
        let mut b = Budget::new(1000);
        let e = manifest_envelope(
            "a.rs",
            Trust::CodeFact,
            "file:a.rs@sha256:x",
            "x",
            SeenStatus::SeenSameHash {
                last_seen_turn: None,
            },
            Some(vs),
            &mut b,
        );
        assert!(matches!(e.state, ArtifactState::KnownUnchanged { .. }));
        assert_eq!(b.remaining(), 1000);

        // not seen + summary in budget → summary_included
        let mut b = Budget::new(1000);
        let e = manifest_envelope(
            "a.rs",
            Trust::CodeFact,
            "file:a.rs@sha256:x",
            "x",
            SeenStatus::NotSeenOrChanged,
            Some(vs),
            &mut b,
        );
        assert!(matches!(e.state, ArtifactState::SummaryIncluded { .. }));
        assert!(e.estimated_tokens_returned > 0);

        // not seen + summary over budget → available_by_handle (budget_exhausted)
        let mut b = Budget::new(0);
        let e = manifest_envelope(
            "a.rs",
            Trust::CodeFact,
            "file:a.rs@sha256:x",
            "x",
            SeenStatus::NotSeenOrChanged,
            Some(vs),
            &mut b,
        );
        assert!(matches!(
            e.state,
            ArtifactState::AvailableByHandle {
                reason: OmitReason::BudgetExhausted,
                ..
            }
        ));

        // not seen + no summary → available_by_handle (not_requested), never "deleted"
        let mut b = Budget::new(1000);
        let e = manifest_envelope(
            "a.rs",
            Trust::CodeFact,
            "file:a.rs@sha256:x",
            "x",
            SeenStatus::NotSeenOrChanged,
            None,
            &mut b,
        );
        assert!(matches!(
            e.state,
            ArtifactState::AvailableByHandle {
                reason: OmitReason::NotRequested,
                ..
            }
        ));
    }

    #[test]
    fn untrusted_artifact_is_never_exact_included() {
        let mut budget = Budget::new(1000);
        let env = envelope_for(
            "tool.out",
            Trust::ToolOutputUntrusted,
            "log:x",
            "basis",
            SeenStatus::NotSeenOrChanged,
            &Validity::Degraded { key: "k".into() },
            Some("raw untrusted instructions"),
            true,
            &mut budget,
        );
        assert!(!matches!(env.state, ArtifactState::ExactIncluded { .. }));
    }

    // --- envelope_for: the budget is a ceiling, not a suggestion -------------

    fn valid_summary_for(text: &str) -> (SummaryStore, MemoryDigestStore, Validity) {
        let dg = MemoryDigestStore::default();
        let store = SummaryStore::new();
        let source = b"the bound source";
        let key = store.put_summary(&dg, "f", source, versions(), text.as_bytes());
        let mut snap = WorktreeSnapshot::new();
        snap.insert("f".into(), sha256_hex(source));
        let v = resolve(&store, &dg, &snap, &key);
        assert!(matches!(v, Validity::Valid(_)));
        (store, dg, v)
    }

    #[test]
    fn a_valid_summary_in_budget_is_included_and_charged_exactly_once() {
        let (_s, _d, validity) = valid_summary_for("digest text for the summary");
        let mut budget = Budget::new(1000);
        let before = budget.remaining();
        let env = envelope_for(
            "f",
            Trust::CodeFact,
            "summary:k",
            "basis",
            SeenStatus::NotSeenOrChanged,
            &validity,
            None,
            false,
            &mut budget,
        );
        let ArtifactState::SummaryIncluded {
            summary,
            summary_key,
            basis_sha256,
            ..
        } = &env.state
        else {
            panic!(
                "a valid, affordable summary must be included: {:?}",
                env.state
            );
        };
        assert_eq!(summary, "digest text for the summary");
        assert!(!summary_key.is_empty(), "the served summary names its key");
        assert_eq!(basis_sha256, "basis");
        assert_eq!(
            before - budget.remaining(),
            env.estimated_tokens_returned,
            "the charge and the declared cost must be the same number"
        );
        assert!(env.estimated_tokens_returned > 0);
    }

    #[test]
    fn a_valid_summary_over_budget_becomes_a_handle_and_carries_no_summary_text() {
        // The ceiling is enforced, not advisory: when the
        // summary does not fit, the caller gets a retrieval handle and NOT the
        // digest — serving it uncharged is how a budget silently stops meaning
        // anything.
        let (_s, _d, validity) = valid_summary_for(&"long digest ".repeat(200));
        let mut budget = Budget::new(3);
        let env = envelope_for(
            "f",
            Trust::CodeFact,
            "summary:k",
            "basis",
            SeenStatus::NotSeenOrChanged,
            &validity,
            None,
            false,
            &mut budget,
        );
        let ArtifactState::AvailableByHandle {
            reason,
            basis_sha256,
            ..
        } = &env.state
        else {
            panic!(
                "an unaffordable summary must degrade to a handle: {:?}",
                env.state
            );
        };
        assert_eq!(*reason, OmitReason::BudgetExhausted);
        assert_eq!(basis_sha256.as_deref(), Some("basis"));
        assert_eq!(env.estimated_tokens_returned, 0);
        assert_eq!(budget.remaining(), 3, "nothing was charged");
    }

    #[test]
    fn a_known_unchanged_entry_names_the_turn_the_window_last_read_it() {
        let serve = |last_seen_turn| {
            envelope_for(
                "a.rs",
                Trust::CodeFact,
                "file:a.rs@sha256:x",
                "basis",
                SeenStatus::SeenSameHash { last_seen_turn },
                &Validity::Degraded { key: "k".into() },
                None,
                false,
                &mut Budget::new(1000),
            )
        };
        let turn_of = |env: Envelope| match env.state {
            ArtifactState::KnownUnchanged { last_seen_turn, .. } => last_seen_turn,
            other => panic!("expected known_unchanged, got {other:?}"),
        };
        assert_eq!(turn_of(serve(Some(7))), Some(7));
        assert_eq!(turn_of(serve(None)), None, "no evidence is no turn");
    }

    #[test]
    fn a_manifest_entry_names_the_turn_the_window_last_read_it() {
        let serve = |last_seen_turn| {
            manifest_envelope(
                "a.rs",
                Trust::CodeFact,
                "file:a.rs@sha256:x",
                "x",
                SeenStatus::SeenSameHash { last_seen_turn },
                None,
                &mut Budget::new(1000),
            )
        };
        let with_turn = serve(Some(3));
        assert_eq!(
            with_turn.estimated_tokens_returned, 0,
            "a turn costs nothing"
        );
        assert!(matches!(
            with_turn.state,
            ArtifactState::KnownUnchanged {
                last_seen_turn: Some(3),
                ..
            }
        ));
        assert!(matches!(
            serve(None).state,
            ArtifactState::KnownUnchanged {
                last_seen_turn: None,
                ..
            }
        ));
    }
}
