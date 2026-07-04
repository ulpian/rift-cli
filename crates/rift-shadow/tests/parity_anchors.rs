//! The golden-anchor capture harness (P7 / ADR-0018).
//!
//! `regenerate_golden_anchors` is the one-shot harness that freezes the parity
//! anchors into the tree (`crates/rift-shadow/parity/anchors/<fixture>/`). It is
//! `#[ignore]`d so it runs only when explicitly invoked:
//!
//! ```text
//! cargo test -p rift-shadow --test parity_anchors regenerate -- --ignored
//! ```
//!
//! Provenance is recorded honestly in each `manifest.json`: when the pinned Entire
//! binary is provisioned the anchors are captured from **it** (`source = entire`,
//! the authoritative oracle); otherwise they are bootstrapped from the
//! deterministic native encoder (`source = native-bootstrap`) — still a frozen
//! regression oracle, to be overwritten by an Entire capture while the pinned
//! binary is still available (the one wall-clock-sensitive P7 task).
//!
//! `committed_anchors_match_the_encoder` runs on every CI pass: it loads the
//! committed anchors and asserts the current native encoder still reproduces them
//! byte-for-byte — a single differing byte is a failure.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use rift_shadow::{
    all_match, anchors_from_fixture, anchors_root, fixture_archive, fixture_names, parity_report,
    render_report, AnchorSource, GoldenAnchors,
};

#[test]
#[ignore = "one-shot: writes committed golden anchors into the source tree"]
fn regenerate_golden_anchors() {
    let root = anchors_root();
    // The bytes are produced by the deterministic native encoder, so the recorded
    // provenance is `native-bootstrap` — never `entire`. Flipping the source to
    // `entire` is coupled to actually capturing from the pinned binary (spawn it
    // over each fixture in a sandbox repo, read the shard artifacts back); that
    // authoritative capture is the one wall-clock-sensitive P7 task (ADR-0018) and
    // must overwrite these bytes, not merely relabel them.
    let source = AnchorSource::NativeBootstrap;
    for name in fixture_names() {
        let anchors = anchors_from_fixture(name, source);
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), anchors.manifest_json()).unwrap();
        for (artifact, bytes) in &anchors.artifacts {
            std::fs::write(dir.join(artifact), bytes).unwrap();
        }
        println!(
            "wrote anchors/{name} ({} artifacts)",
            anchors.artifacts.len()
        );
    }
}

#[test]
fn committed_anchors_match_the_encoder() {
    // Every fixture must have committed anchors, and the current native encoder
    // must still reproduce them byte-for-byte (the frozen-oracle regression guard).
    for name in fixture_names() {
        let anchors = GoldenAnchors::load(name).unwrap_or_else(|| {
            panic!("missing committed anchors for fixture '{name}' — run the regenerate harness")
        });
        let archive = fixture_archive(name);
        let report = parity_report(&archive, &anchors);
        assert!(
            all_match(&report),
            "committed anchors for '{name}' ({}) drifted from the native encoder:\n{}",
            anchors.source.as_str(),
            render_report(&report)
        );
    }
}
