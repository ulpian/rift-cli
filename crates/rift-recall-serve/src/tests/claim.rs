//! What a pull's audit record says its caller claimed: how many held
//! revisions it named, and how many of them this family had read.

use proptest::prelude::{prop_assert, prop_assert_eq};
use rift_recall_policy::{PolicyEngine, TurnBudget};
use rift_recall_substrate::MemorySubstrate;
use rift_store::{CanonicalStore, SqliteStore};
use serde_json::{json, Value};

use super::fixtures::*;
use crate::*;

/// A revision of `src/c.rs` the projection holds and no read recorded.
const UNREAD: &str = "cc00000000000000000000000000000000000000000000000000000000000000";

fn dispatched(
    store: &SqliteStore,
    reader: &MemorySubstrate,
    tool: ToolName,
    req: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let cfg = cfg();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let mut budget = TurnBudget::new(8, 100_000);
    block(s.dispatch(tool, req, &ctx(store, reader), &mut budget))
}

fn claim_of(resp: &ToolResponse) -> Value {
    serde_json::to_value(&resp.event).unwrap()["pull_claim"].clone()
}

fn stating(hashes: &[&str]) -> ToolRequest {
    ToolRequest {
        known_hashes: hashes.iter().map(|h| (*h).to_owned()).collect(),
        ..ranking_req()
    }
}

/// [`held_store`] plus one revision nobody recorded reading.
fn store_with_an_unread_revision() -> (SqliteStore, Checkout, String, String) {
    let (store, checkout, a, b) = held_store();
    seed_revision(&store, "rev_c", "src/c.rs", UNREAD, UNREAD, 10);
    (store, checkout, a, b)
}

#[test]
fn a_pull_names_how_many_revisions_were_stated_and_how_many_this_family_read() {
    let (store, checkout, a, b) = store_with_an_unread_revision();
    let reader = checkout.reader();
    let stated = [a.as_str(), b.as_str(), UNREAD, "ffff"];
    for tool in [
        ToolName::KnownState,
        ToolName::DiffSince,
        ToolName::ContextPack,
    ] {
        let resp = dispatched(&store, &reader, tool, &stating(&stated)).unwrap();
        assert_eq!(
            claim_of(&resp),
            json!({ "stated": 4, "recorded": 2, "basis": "stated_recorded" }),
            "{tool:?}"
        );
    }
}

/// The audit row and its claim row land from the one envelope the serve
/// hands on, keyed alike.
#[test]
fn the_claim_reaches_its_own_row_beside_the_serves_audit_row() {
    let (store, checkout, a, _) = store_with_an_unread_revision();
    let reader = checkout.reader();
    let resp = dispatched(
        &store,
        &reader,
        ToolName::KnownState,
        &stating(&[&a, UNREAD]),
    )
    .unwrap();
    let envelope = resp.event.to_hook_json_keyed("call-1");
    block(rift_recall_ledger::audit::append_memd_audit(
        &store,
        envelope.as_bytes(),
        5,
    ))
    .unwrap();
    let rows = block(store.query(
        "SELECT c.stated_count, c.recorded_count, c.unrecorded_count, c.basis \
         FROM memd_audit_pull_claim c JOIN memd_audit_event e ON e.audit_id = c.audit_id",
        &[],
    ))
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_i64(0).unwrap(), 2);
    assert_eq!(rows[0].get_i64(1).unwrap(), 1);
    assert_eq!(rows[0].get_i64(2).unwrap(), 1);
    assert_eq!(rows[0].get_text(3).unwrap(), "stated_recorded");
}

#[test]
fn a_pull_with_no_checkout_still_names_what_was_read_under_its_own_rule() {
    let (store, _checkout, a, _) = store_with_an_unread_revision();
    let unbound = MemorySubstrate::default();
    let resp = dispatched(
        &store,
        &unbound,
        ToolName::KnownState,
        &stating(&[&a, UNREAD]),
    )
    .unwrap();
    assert_eq!(
        claim_of(&resp),
        json!({ "stated": 2, "recorded": 1, "basis": "unverified" })
    );
}

#[test]
fn a_pull_that_stated_nothing_a_refusal_and_a_verb_that_credits_nothing_claim_nothing() {
    let (store, checkout, a, _) = store_with_an_unread_revision();
    let reader = checkout.reader();
    let silent = dispatched(&store, &reader, ToolName::KnownState, &ranking_req()).unwrap();
    assert_eq!(claim_of(&silent), Value::Null);
    let missing = ToolRequest {
        target: Some("../outside/evil.rs".into()),
        known_hashes: vec![a.clone()],
        ..Default::default()
    };
    let refused = dispatched(&store, &reader, ToolName::ChangedHunks, &missing).unwrap();
    assert!(matches!(
        refused.event,
        rift_recall_events::MemdEvent::Refused { .. }
    ));
    assert_eq!(claim_of(&refused), Value::Null);
    let hotspots = dispatched(&store, &reader, ToolName::CostHotspots, &stating(&[&a])).unwrap();
    assert_eq!(claim_of(&hotspots), Value::Null);
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(24))]

    /// Whatever mix of read, unread and unknown revisions a caller names, the
    /// recorded count is the number of named revisions with a read, and the
    /// stated count is that plus the rest.
    #[test]
    fn the_stated_count_is_the_read_plus_the_unread(
        pick in proptest::collection::vec(0usize..4, 1..8),
    ) {
        let (store, checkout, a, b) = store_with_an_unread_revision();
        let reader = checkout.reader();
        let pool = [a.as_str(), b.as_str(), UNREAD, "dd"];
        let named: std::collections::BTreeSet<&str> = pick.iter().map(|i| pool[*i]).collect();
        let read = named.iter().filter(|h| [a.as_str(), b.as_str()].contains(h)).count();
        let stated: Vec<&str> = named.iter().copied().collect();
        let resp = dispatched(&store, &reader, ToolName::DiffSince, &stating(&stated)).unwrap();
        let claim = claim_of(&resp);
        let (n, r) = (claim["stated"].as_u64().unwrap(), claim["recorded"].as_u64().unwrap());
        prop_assert_eq!(n, u64::try_from(named.len()).unwrap());
        prop_assert_eq!(r, u64::try_from(read).unwrap());
        prop_assert!(r <= n);
    }
}
