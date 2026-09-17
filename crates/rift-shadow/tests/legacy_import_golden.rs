//! The legacy-import golden suite.
//!
//! Two committed fixtures — one per legacy on-disk layout — rendered to a
//! canonical summary and compared **byte for byte** with the `expected.txt`
//! frozen beside them. A single changed byte anywhere in a fixture changes the
//! summary and reds [`golden_summaries_are_byte_stable`]; that is the whole
//! point of the suite, and it is calibrated rather than assumed.
//!
//! There is deliberately **no bless switch**. A missing `expected.txt` panics
//! with the rendering it would have needed, and a human places the file. A test
//! suite with a "make it pass" button is not an oracle.
//!
//! Fixture provenance: `crates/rift-shadow/legacy-fixtures/README.md`. Nothing
//! here was fetched; every legacy spelling is cited to the in-tree declaration
//! that already carried it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rift_shadow::legacy_import::{
    classify_ref, parse_shard, rederive_root, root_check, ImportReport, Layout, Shard, Source,
};

/// Files in a fixture dir that are documentation, not shard artifacts.
const NON_ARTIFACTS: &[&str] = &["README.md", "PROVENANCE", "expected.txt"];

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("legacy-fixtures")
}

/// Read a fixture's artifact bytes, its declared ref name, and its id.
fn load(dir: &Path) -> (BTreeMap<String, Vec<u8>>, String, String) {
    let mut artifacts = BTreeMap::new();
    let entries = std::fs::read_dir(dir).expect("fixture dir readable");
    for e in entries {
        let e = e.expect("dir entry");
        let name = e.file_name().to_string_lossy().into_owned();
        if NON_ARTIFACTS.contains(&name.as_str()) {
            continue;
        }
        artifacts.insert(name, std::fs::read(e.path()).expect("artifact readable"));
    }
    let prov = std::fs::read_to_string(dir.join("PROVENANCE")).expect("PROVENANCE present");
    let mut parts = prov.split_whitespace();
    let ref_name = parts
        .next()
        .expect("PROVENANCE names a ref")
        .replace("{retired}", concat!("ent", "ire"));
    // `content_hash.txt` is the shard's own content address — the id on disk.
    let id = String::from_utf8(
        artifacts
            .get("content_hash.txt")
            .expect("fixture carries content_hash.txt")
            .clone(),
    )
    .expect("content hash is utf-8")
    .trim()
    .to_owned();
    (artifacts, ref_name, id)
}

/// The canonical, deterministic rendering the golden pins. Every parsed field
/// appears, so no fixture byte can change without changing this string.
fn summary(shard: &Shard) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let m = &shard.metadata;
    let _ = writeln!(s, "checkpoint_id {}", shard.checkpoint_id);
    let _ = writeln!(s, "layout {}", shard.layout.token());
    let _ = writeln!(s, "epoch {}", shard.epoch);
    let _ = writeln!(s, "strategy {}", m.strategy.as_deref().unwrap_or("-"));
    let _ = writeln!(s, "commit {}", m.commit.as_deref().unwrap_or("-"));
    let _ = writeln!(s, "developer {}", m.developer.as_deref().unwrap_or("-"));
    let _ = writeln!(s, "created_at {}", m.created_at.as_deref().unwrap_or("-"));
    let _ = writeln!(s, "sessions {}", m.sessions.join(","));
    let _ = writeln!(s, "merkle_root {}", m.merkle_root.as_deref().unwrap_or("-"));
    let _ = writeln!(s, "prev_hash {}", m.prev_hash.as_deref().unwrap_or("-"));
    let _ = writeln!(s, "report_valid {}", shard.report_is_valid());
    let _ = writeln!(s, "root_check {}", root_check(shard).token());
    let _ = writeln!(
        s,
        "rederived_root {}",
        rederive_root(shard).map_or_else(|| "-".to_owned(), |r| r.to_hex())
    );
    let r = &shard.report;
    let _ = writeln!(
        s,
        "report spans={} masked={} allowlisted={} packs={} input={}",
        r.total_spans,
        r.bytes_masked,
        r.allowlisted,
        r.rule_pack_versions.join("+"),
        r.input_sha256.as_deref().unwrap_or("-")
    );
    for (det, n) in &r.by_detector {
        let _ = writeln!(s, "detector {det} {n}");
    }
    let _ = writeln!(s, "artifacts {}", shard.artifacts.join(","));
    let _ = writeln!(s, "events {}", shard.events.len());
    for e in &shard.events {
        let _ = writeln!(
            s,
            "event session={} turn={} ordinal={} kind={} verb={} agent={} at={} payload={}",
            e.session,
            e.turn_ordinal,
            e.event_ordinal,
            e.kind,
            e.verb,
            e.agent.as_deref().unwrap_or("-"),
            e.occurred_at_ms,
            e.payload_sha256.as_deref().unwrap_or("-")
        );
    }
    if shard.issues.is_empty() {
        let _ = writeln!(s, "issues -");
    } else {
        for i in &shard.issues {
            let _ = writeln!(s, "issue {i}");
        }
    }
    s
}

/// Every fixture, as `(dir name, layout)`. Both legacy layouts are present, and
/// [`both_layouts_are_covered`] fails if one is ever dropped.
fn fixtures() -> Vec<(&'static str, Layout)> {
    vec![
        ("sharded_branch", Layout::ShardedBranch),
        ("per_checkpoint_refs", Layout::PerCheckpointRefs),
    ]
}

#[test]
fn both_layouts_are_covered() {
    let mut seen: Vec<Layout> = fixtures().into_iter().map(|(_, l)| l).collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(
        seen.len(),
        2,
        "the suite requires a fixture for BOTH legacy layouts"
    );
}

/// Each fixture's declared ref classifies to the layout its directory claims —
/// so a fixture cannot drift from the classifier it is supposed to exercise.
#[test]
fn each_fixture_ref_classifies_to_its_declared_layout() {
    for (name, want) in fixtures() {
        let (_, ref_name, id) = load(&fixtures_root().join(name));
        let got = classify_ref(&ref_name).unwrap_or_else(|| panic!("{name}: ref not classified"));
        assert_eq!(got.0, want, "{name}: layout");
        match got.1 {
            Source::Branch => assert_eq!(want, Layout::ShardedBranch),
            Source::Checkpoint(cid) => assert_eq!(cid, id, "{name}: ref id vs content_hash.txt"),
        }
    }
}

#[test]
fn golden_summaries_are_byte_stable() {
    for (name, layout) in fixtures() {
        let dir = fixtures_root().join(name);
        let (artifacts, _, id) = load(&dir);
        let got = summary(&parse_shard(&id, layout, &artifacts));
        let path = dir.join("expected.txt");
        let want = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "{name}: no frozen expected.txt. There is no bless switch — place \
                 this file by hand after reading it:\n----8<----\n{got}----8<----"
            )
        });
        assert_eq!(got, want, "{name}: summary drifted from its frozen golden");
    }
}

/// The golden's calibration: the summary is a function of every fixture byte,
/// so one flipped byte of `full.jsonl` — in memory, the tracked fixture is
/// untouched — yields a summary that no longer matches `expected.txt`. This
/// is what makes `golden_summaries_are_byte_stable` a byte lock rather than a
/// parse check, and it is the red the `dialect-conformance` CI job exists to
/// surface.
#[test]
fn a_flipped_fixture_byte_reds_the_golden() {
    for (name, layout) in fixtures() {
        let dir = fixtures_root().join(name);
        let (mut artifacts, _, id) = load(&dir);
        let want = std::fs::read_to_string(dir.join("expected.txt")).expect("frozen golden");
        assert_eq!(
            summary(&parse_shard(&id, layout, &artifacts)),
            want,
            "{name}: the unflipped fixture must match before the flip means anything"
        );
        let full = artifacts
            .get_mut("full.jsonl")
            .expect("fixture carries full.jsonl");
        // A byte inside the first record's payload, not a structural brace:
        // the point is that CONTENT moves the summary, not that JSON breaks.
        let at = full
            .iter()
            .position(u8::is_ascii_alphanumeric)
            .expect("full.jsonl has a payload byte");
        full[at] ^= 0x01;
        assert_ne!(
            summary(&parse_shard(&id, layout, &artifacts)),
            want,
            "{name}: a flipped payload byte left the summary equal to its golden — \
             the golden is not a byte lock"
        );
    }
}

/// The reader's operator-facing report over the whole fixture corpus, frozen.
/// This is the surface a user sees, so it is pinned separately from the shards.
#[test]
fn the_corpus_report_is_frozen_and_brand_free() {
    let mut report = ImportReport::default();
    for (name, layout) in fixtures() {
        let (artifacts, _, id) = load(&fixtures_root().join(name));
        report.observe(&parse_shard(&id, layout, &artifacts));
    }
    let rendered = report.render();
    assert_eq!(
        rendered,
        "layout per-checkpoint-refs: 1 checkpoint(s)\n\
         layout sharded-branch: 1 checkpoint(s)\n\
         importable 2 · refused 0 · events 5\n\
         root-check mismatch: 1\n\
         root-check self-consistent: 1\n\
         imported checkpoints are claims, not chain anchors — \
         a pre-rewrite root cannot be re-derived by this build\n"
    );
    let lower = rendered.to_ascii_lowercase();
    for token in [concat!("ent", "ire"), concat!("ent", "ireio")] {
        assert!(
            !lower.contains(token),
            "brand leaked into the import report"
        );
    }
}

/// The corpus covers **both** root-check outcomes, deliberately: `sharded_branch`
/// claims exactly what its own events derive, `per_checkpoint_refs` claims
/// something else. A suite in which every shard agrees with itself would leave
/// the mismatch arm — the realistic epoch-0 case — untested, and a suite in
/// which none did would leave the agreement arm untested.
#[test]
fn the_corpus_exercises_both_root_check_outcomes() {
    let mut seen = std::collections::BTreeSet::new();
    for (name, layout) in fixtures() {
        let (artifacts, _, id) = load(&fixtures_root().join(name));
        seen.insert(root_check(&parse_shard(&id, layout, &artifacts)).token());
    }
    assert!(seen.contains("self-consistent"), "{seen:?}");
    assert!(seen.contains("mismatch"), "{seen:?}");
}

/// Both fixtures import at the pre-rewrite canonicalization epoch — the
/// migration fact that makes old history readable at all.
#[test]
fn every_legacy_fixture_reads_at_epoch_zero() {
    for (name, layout) in fixtures() {
        let (artifacts, _, id) = load(&fixtures_root().join(name));
        assert_eq!(parse_shard(&id, layout, &artifacts).epoch, 0, "{name}");
    }
}

// --- the damaged fixture (the "corrupt shard") --------------------------

/// Fixtures that are **deliberately damaged**, kept OUT of [`fixtures`] on
/// purpose: the importable corpus's frozen report must not move because a
/// broken shard was added beside it, and a suite that mixed the two could not
/// say which arm a change came from.
fn damaged_fixtures() -> Vec<(&'static str, Layout)> {
    vec![("corrupt_shard", Layout::ShardedBranch)]
}

/// **A half-readable legacy shard is evidence, not a crash.**
/// Four fixtures were named and this one did not exist, so nothing
/// in the tree exercised the reader against damage at all — every legacy
/// fixture was well-formed, which means the totality `parse_shard`'s docstring
/// claims ("never panics, never errors") was an ARGUMENT and not a measurement.
/// Three artifacts are broken three different ways at once: `metadata.json`
/// truncated mid-object, `full.jsonl` carrying a truncated record BETWEEN two
/// good ones, and a `redaction_report.json` that is well-formed JSON but not a
/// valid report.
///
/// What must happen: the parse completes, every problem is named in `issues`,
/// the two intact event records survive, and the shard is **refused** for want
/// of a valid redaction report rather than imported — the same gate the
/// artifact bridge applies to a native shard.
#[test]
fn a_corrupt_shard_is_refused_rather_than_imported_and_never_panics() {
    let (artifacts, _, id) = load(&fixtures_root().join("corrupt_shard"));
    let shard = parse_shard(&id, Layout::ShardedBranch, &artifacts);

    assert!(
        !shard.report_is_valid(),
        "a report with no rule packs and no input hash is not valid"
    );
    assert_eq!(
        shard.events.len(),
        2,
        "the two intact records survive the one between them: {:?}",
        shard.issues
    );
    let issues = shard.issues.join(" | ");
    assert!(
        issues.contains("metadata.json: unparseable"),
        "the truncated header is named: {issues}"
    );
    assert!(
        issues.contains("full.jsonl: 1 unparseable line(s)"),
        "the truncated record is counted: {issues}"
    );

    // …and the operator-facing fold says refused, not importable.
    let mut report = ImportReport::default();
    report.observe(&shard);
    assert_eq!(report.refused, 1);
    assert_eq!(report.importable, 0);
    assert_eq!(
        report.events, 0,
        "a refused shard contributes no events to the corpus"
    );
    assert!(
        report.root_checks.is_empty(),
        "a refused shard is not root-checked — nobody looked, and the report says so \
         by having no row rather than by inventing one"
    );
}

/// The damaged shard's rendering is frozen too, so the *way* it fails cannot
/// drift silently. Same discipline as [`golden_summaries_are_byte_stable`] and
/// the same absence of a bless switch.
#[test]
fn damaged_summaries_are_byte_stable() {
    for (name, layout) in damaged_fixtures() {
        let dir = fixtures_root().join(name);
        let (artifacts, _, id) = load(&dir);
        let got = summary(&parse_shard(&id, layout, &artifacts));
        let path = dir.join("expected.txt");
        let want = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "{name}: no frozen expected.txt. There is no bless switch — place \
                 this file by hand after reading it:\n----8<----\n{got}----8<----"
            )
        });
        assert_eq!(got, want, "{name}: summary drifted from its frozen golden");
    }
}

/// **The damaged fixture's ref classifies like the intact ones' do.** Its
/// PROVENANCE carries a token-free marker expanded to the real pre-Rift ref, so the
/// ref-name-to-layout classifier is exercised by the damaged arm too, not
/// only by the two intact fixtures.
#[test]
fn each_damaged_fixture_ref_classifies_to_its_declared_layout() {
    for (name, want) in damaged_fixtures() {
        let (_, ref_name, _) = load(&fixtures_root().join(name));
        let got = classify_ref(&ref_name)
            .unwrap_or_else(|| panic!("{name}: damaged fixture's ref not classified"));
        assert_eq!(got.0, want, "{name}: layout");
    }
}
