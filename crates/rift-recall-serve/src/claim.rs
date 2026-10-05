//! What a pull's caller claimed to hold, counted for the pull's audit record:
//! how many revisions it named, and how many of them this family recorded
//! reading.
//!
//! A separate module from the crediting evidence because the two answer
//! different questions. The evidence decides what a pull is credited with,
//! per candidate and against the checkout; the claim counts every revision
//! the caller named, candidate or not, so a reader of the audit can see how
//! much of an answer was claimed and how much of the claim no recorded read
//! accounts for. It records; it never changes a figure.

use std::collections::{BTreeMap, BTreeSet};

use rift_recall_events::{PullClaim, PullClaimBasis};
use rift_recall_ledger::candidates::recorded_reads;
use rift_store::CanonicalStore;

use crate::basis::{resolve_basis, Basis};
use crate::AvoidedBasis;

/// The claim of a call booked under `basis` that named `stated`, or `None`
/// when the call made none: a rule other than the two a stated hash leads to
/// means nothing was stated or the verb credits nothing.
///
/// A store that cannot answer yields `None` too. A claim row with a recorded
/// count Rift could not establish would say "stated but never read" about
/// revisions it never looked up.
pub(crate) async fn pull_claim(
    store: &impl CanonicalStore,
    stated: &[String],
    basis: AvoidedBasis,
) -> Option<PullClaim> {
    let basis = match basis {
        AvoidedBasis::StatedRecorded => PullClaimBasis::StatedRecorded,
        AvoidedBasis::Unverified => PullClaimBasis::Unverified,
        AvoidedBasis::NoneStated | AvoidedBasis::Declared | AvoidedBasis::NotCredited => {
            return None;
        }
    };
    let named: BTreeSet<&str> = stated.iter().map(String::as_str).collect();
    let mut resolved = BTreeMap::new();
    for given in &named {
        match resolve_basis(store, given).await {
            Ok(Basis::Resolved(hash)) => {
                resolved.insert(*given, hash);
            }
            Ok(Basis::Unknown) => (),
            Err(_) => return None,
        }
    }
    let wanted: BTreeSet<&str> = resolved.values().map(String::as_str).collect();
    let read: BTreeSet<String> = recorded_reads(store, &wanted)
        .await
        .ok()?
        .into_iter()
        .map(|(_, hash)| hash)
        .collect();
    let recorded = resolved
        .values()
        .filter(|hash| read.contains(*hash))
        .count();
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    Some(PullClaim::new(count(named.len()), count(recorded), basis))
}
