//! The pack compiler's tests, one file per subject. `fixtures` holds the
//! request builders, envelope builders and corpora every subject shares; the
//! four snapshot tests stay here because insta names a snapshot by the module
//! that asserts it, and `props` holds every property test because proptest
//! keys its regression seeds by the file that runs them.

mod budget;
mod citations;
mod entries;
mod evidence_packs;
mod fixtures;
mod key;
mod props;
mod red_team;

use super::*;
use fixtures::*;

/// The review pack's rendered shape, pinned at its default budget and under
/// pressure — placement drift in the evidence kinds shows up here first.
#[test]
fn review_pack_snapshots() {
    let comfortable = compile(&review_req(PackKind::Review.default_tokens()), u32::MAX).unwrap();
    insta::assert_snapshot!("review_pack_comfortable", comfortable.render());
    let tight = compile(&review_req(180), u32::MAX).unwrap();
    insta::assert_snapshot!("review_pack_tight", tight.render());
}

/// The investigation pack's rendered shape, pinned. Note what is NOT in it:
/// not one word of either finding's text, and not one word of any seed prose —
/// counts and handles are the whole of what an untrusted contributor gets to
/// put in front of the next agent.
#[test]
fn investigation_pack_snapshots() {
    let comfortable = compile(
        &investigation_req(PackKind::Investigation.default_tokens()),
        u32::MAX,
    )
    .unwrap();
    insta::assert_snapshot!("investigation_pack_comfortable", comfortable.render());
    let tight = compile(&investigation_req(140), u32::MAX).unwrap();
    insta::assert_snapshot!("investigation_pack_tight", tight.render());
}

/// The serialized shape, pinned — this is what a `--json` evidence artifact
/// looks like on disk and on another machine.
#[test]
fn evidence_pack_snapshot() {
    let pack = compile(&review_req(PackKind::Review.default_tokens()), u32::MAX).unwrap();
    let evidence = pack.to_evidence(&key_inputs());
    insta::assert_snapshot!(
        "evidence_pack_review",
        serde_json::to_string_pretty(&evidence).unwrap()
    );
}

/// Degraded-pack shapes, pinned: comfortable (everything fits), tight
/// (spill + tail degradation), tiny (anchor-only survival) — reviewable
/// placement drift shows up here first.
#[test]
fn degraded_pack_snapshots() {
    let comfortable = compile(&degrading_req(1200), u32::MAX).unwrap();
    insta::assert_snapshot!("pack_comfortable", comfortable.render());
    let tight = compile(&degrading_req(150), u32::MAX).unwrap();
    insta::assert_snapshot!("pack_tight", tight.render());
    let tiny = compile(&degrading_req(40), u32::MAX).unwrap();
    insta::assert_snapshot!("pack_tiny", tiny.render());
}
