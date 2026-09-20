//! The pack key: input-keyed, sensitive to every input, and domain-separated
//! by kind.

use super::fixtures::*;
use crate::*;

#[test]
fn pack_key_is_input_keyed_and_sensitive_to_every_input() {
    // Computable before any render, stable for identical inputs.
    let base = pack_key(&key_inputs());
    assert_eq!(base.len(), 64);
    assert_eq!(base, pack_key(&key_inputs()), "identical inputs, same key");

    let mut other_query = key_inputs();
    other_query.query = "task: other".into();
    assert_ne!(
        base,
        pack_key(&other_query),
        "query is part of the identity"
    );

    let mut reordered = key_inputs();
    reordered.ranked.swap(0, 1);
    assert_ne!(
        base,
        pack_key(&reordered),
        "rank ORDER is part of the identity"
    );

    let mut rehashed = key_inputs();
    rehashed.ranked[0].1 = "hashA2".into();
    assert_ne!(
        base,
        pack_key(&rehashed),
        "content hash is part of the identity"
    );

    let mut repoliced = key_inputs();
    repoliced.policy_version = 2;
    assert_ne!(
        base,
        pack_key(&repoliced),
        "policy version is part of the identity"
    );

    let mut rebudgeted = key_inputs();
    rebudgeted.token_budget = 800;
    assert_ne!(
        base,
        pack_key(&rebudgeted),
        "budget is part of the identity"
    );
}

/// The context domain is frozen (`pack_key` is `pack_key_for(Context, ..)`),
/// and the same inputs under two kinds are two different packs — an evidence
/// pack can never collide with the context pack a turn was served.
#[test]
fn pack_key_is_domain_separated_by_kind() {
    let i = key_inputs();
    assert_eq!(pack_key(&i), pack_key_for(PackKind::Context, &i));
    let keys: Vec<String> = ALL_KINDS.iter().map(|k| pack_key_for(*k, &i)).collect();
    for (a, b) in [(0, 1), (0, 2), (1, 2)] {
        assert_ne!(keys.get(a), keys.get(b), "kinds {a} and {b} collide");
    }
    // and the key still answers to its inputs under a non-context kind
    let mut other = key_inputs();
    other.token_budget = 999;
    assert_ne!(
        pack_key_for(PackKind::Review, &i),
        pack_key_for(PackKind::Review, &other)
    );
}
