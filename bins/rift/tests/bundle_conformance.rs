//! **The sync format is second-implementable, re-derived in Rust and kept in
//! the tree.**
//!
//! A blind review of the format implemented a bundle reader from the document
//! alone and compared its verdicts with the shipped importer's; its verdicts
//! are tracked under `crates/rift-store/bundle-review/`, but the reader was
//! a git-ignored script and the 2026-08 sync-format reader is gone. This
//! harness is that instrument, tracked: `mod blind` is an independent reader
//! of the pure offline steps written from `docs/sync-format.md` only (the
//! self-pin keeps it free of every rift crate); `fixtures` builds real
//! bundles through the shipped binary; `matrix` mutates them
//! deterministically; `agreement` asserts the blind verdict and the shipped
//! importer's exit code agree on every case, with a shrink-only registry of
//! known disagreements that is EMPTY.
//!
//! Re-certification recipe (also in the bundle-review README): diff the
//! sync-format entries against the last certified text → edit `mod blind`
//! from the document alone → run this file → every disagreement is a doc
//! defect or a shipped defect, never a registry row without a root cause →
//! add a dated Results row.
//!
//! Placement: only `bins/rift`'s own integration tests may name the binary
//! (`common::only_the_chokepoint_names_the_binary`), and a separate crate
//! would churn the crate-graph snapshot for nothing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[path = "common/chain_fixture.rs"]
mod chain_fixture;
mod common;

#[path = "bundle_conformance/blind.rs"]
mod blind;

use chain_fixture::{copy_dir, empty_target, ref_oid, rift, seeded_chain, ChainFixture};

// ---------------------------------------------------------------------------
// fixtures — real bundles through the shipped binary, built once
// ---------------------------------------------------------------------------

/// The three bundles the bundle-review recipe names, from one real
/// three-position chain, plus a second one-position chain for the
/// foreign-history cases.
struct Fixtures {
    /// Kept alive for the process: every path below lives inside it.
    _tmp: tempfile::TempDir,
    root: PathBuf,
    chain: ChainFixture,
    /// Whole chain, three links, from the origin.
    full: Vec<u8>,
    /// From position 1: two links.
    incr: Vec<u8>,
    /// From position 3, the head: zero links, `from == to`, non-origin.
    empty: Vec<u8>,
    /// A one-position chain from an unrelated repository.
    foreign: Vec<u8>,
    /// A target repository that already holds `full` (imported by the
    /// shipped binary), copied per case.
    holding_full: PathBuf,
}

fn fixtures() -> &'static Fixtures {
    static FIXTURES: OnceLock<Fixtures> = OnceLock::new();
    FIXTURES.get_or_init(|| {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let chain = seeded_chain(&root, "chain", 3);
        let ids = chain.chain_ids();
        assert_eq!(ids.len(), 3, "the fixture chain holds three positions");

        let full_path = root.join("full.riftbundle");
        let doc = chain.export(None, &full_path);
        assert_eq!(doc["links"], 3, "full: {doc}");
        assert_eq!(doc["from_cursor"], "rift-cursor/1:0:");
        let to = blind::parse_cursor(doc["to_cursor"].as_str().unwrap()).expect("to cursor");
        assert_eq!(to.chain_seq, 3, "full: to.chain_seq");
        assert_eq!(to.checkpoint_id, ids[2]);

        let incr_path = root.join("incr.riftbundle");
        let doc = chain.export(Some(&chain.cursor_at(1)), &incr_path);
        assert_eq!(doc["links"], 2, "incr: {doc}");
        let empty_path = root.join("empty.riftbundle");
        let doc = chain.export(Some(&chain.cursor_at(3)), &empty_path);
        assert_eq!(doc["links"], 0, "empty: {doc}");
        assert_eq!(doc["chain_head"], ids[2], "empty reports the real head");

        let other = seeded_chain(&root, "foreign", 1);
        let foreign_path = root.join("foreign.riftbundle");
        other.export(None, &foreign_path);

        let holding_full = empty_target(&root, "holding-full");
        let cache = root.join("holding-full-cache");
        std::fs::create_dir_all(&cache).unwrap();
        let (code, out, err) = rift(
            &holding_full,
            &cache,
            &["--json", "bundle", "import", full_path.to_str().unwrap()],
        );
        assert_eq!(code, 0, "seeding the holding-full target: {out}{err}");

        Fixtures {
            _tmp: tmp,
            root,
            chain,
            full: std::fs::read(&full_path).unwrap(),
            incr: std::fs::read(&incr_path).unwrap(),
            empty: std::fs::read(&empty_path).unwrap(),
            foreign: std::fs::read(&foreign_path).unwrap(),
            holding_full,
        }
    })
}

/// A per-case scratch directory under the fixtures root.
fn case_dir(name: &str) -> PathBuf {
    let dir = fixtures().root.join("cases").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A fresh empty target for a case.
fn fresh_target(case: &Path) -> PathBuf {
    empty_target(case, "target")
}

/// A copy of the target that already holds `full`.
fn target_holding_full(case: &Path) -> PathBuf {
    let to = case.join("target");
    copy_dir(&fixtures().holding_full, &to);
    to
}

/// The fixture is what the recipe says it is: three links, position 3.
#[test]
fn the_recipe_yields_a_three_position_chain_and_three_bundles() {
    let f = fixtures();
    let full = blind::verify_pure(&f.full).expect("full parses through the pure steps");
    assert_eq!(full.segment.links.len(), 3);
    assert_eq!(full.segment.to.chain_seq, 3);
    assert!(full.segment.from.is_origin());
    let incr = blind::verify_pure(&f.incr).expect("incr parses");
    assert_eq!(incr.segment.links.len(), 2);
    assert_eq!(incr.segment.from.chain_seq, 1);
    let empty = blind::verify_pure(&f.empty).expect("empty parses");
    assert!(empty.segment.links.is_empty());
    assert_eq!(empty.segment.from, empty.segment.to);
    assert_eq!(empty.manifest.chain_head, f.chain.chain_ids()[2]);
    assert!(
        full.manifest.writer_conforming
            && incr.manifest.writer_conforming
            && empty.manifest.writer_conforming,
        "the shipped writer emits compact, field-ordered manifests"
    );
}

// ---------------------------------------------------------------------------
// blind — unit vectors from the document, and the self-pin
// ---------------------------------------------------------------------------

/// The canonical-integer rule, cursor rules 1–4, and the origin halves.
#[test]
fn the_cursor_and_integer_rules_read_as_the_document_states_them() {
    assert_eq!(blind::canonical_u64("0"), Some(0));
    assert_eq!(blind::canonical_u64("7"), Some(7));
    for bad in ["007", "+1", "-1", "", "1_000", "0x1", " 1", "1 "] {
        assert_eq!(blind::canonical_u64(bad), None, "{bad:?}");
    }
    let origin = blind::parse_cursor("rift-cursor/1:0:").unwrap();
    assert!(origin.is_origin());
    let id = "ab".repeat(32);
    let at = blind::parse_cursor(&format!("rift-cursor/1:7:{id}")).unwrap();
    assert_eq!((at.chain_seq, at.checkpoint_id.as_str()), (7, id.as_str()));
    for bad in [
        "rift-cursor/1:007:ab",
        "rift-cursor/1:7:ab:extra",
        "rift-cursor/0:0:",
        "rift-cursor/2:0:",
        "rift-cursor/1:0:ab",
        "rift-cursor/1:1:",
        "rift-cursor/1:1:ab-cd",
        "cursor/1:1:ab",
        &format!("rift-cursor/1:1:{}", "a".repeat(65)),
    ] {
        assert!(blind::parse_cursor(bad).is_err(), "{bad:?} must be refused");
    }
}

fn hex64(byte: u8) -> String {
    format!("{byte:02x}").repeat(32)
}

/// Segment canonicity and verification rules 1–4, from the entry.
#[test]
fn the_segment_rules_read_as_the_document_states_them() {
    let (a, b, r) = (hex64(0xab), hex64(0xcd), hex64(0xef));
    let good = format!(
        "rift-chain-segment/1\nfrom rift-cursor/1:0:\nto rift-cursor/1:2:{b}\nlink {a} {r} -\nlink {b} {r} {a}\n"
    );
    let seg = blind::parse_segment(good.as_bytes()).unwrap();
    blind::verify_segment(&seg).unwrap();
    let refused = |text: String, why: &str| {
        let outcome = blind::parse_segment(text.as_bytes()).and_then(|s| blind::verify_segment(&s));
        assert!(outcome.is_err(), "{why}");
    };
    refused(
        good.replace("segment/1", "segment/01"),
        "non-canonical header",
    );
    refused(good.replace('\n', "\r\n"), "CRLF");
    refused(good.trim_end().to_owned(), "missing final newline");
    refused(good.replace(&a, &a.to_uppercase()), "uppercase hex");
    refused(good.replacen('\n', "\n\n", 1), "blank line");
    refused(format!("link {a} {r} - extra\n"), "fewer than three lines");
    refused(
        good.replace(&format!("link {b} {r} {a}\n"), ""),
        "rule 1: dropped link",
    );
    refused(
        good.replace(&format!("{r} -\n"), &format!("{r} {b}\n")),
        "rule 2: prev at origin",
    );
    refused(
        good.replace(&format!("{r} {a}\n"), &format!("{r} {b}\n")),
        "rule 3: prev chaining",
    );
    refused(
        good.replace(
            &format!("to rift-cursor/1:2:{b}"),
            &format!("to rift-cursor/1:2:{a}"),
        ),
        "rule 4: to id",
    );
    refused(
        format!("rift-chain-segment/1\nfrom rift-cursor/1:0:\nto rift-cursor/1:1:{a}\n"),
        "empty with from != to",
    );
    refused(good.replace("link", "line"), "not a link record");
}

/// Ref-name and oid legality, as the format document enumerates it.
#[test]
fn ref_names_and_oids_are_screened_as_enumerated() {
    for ok in [
        "refs/rift/chain/v1",
        "refs/heads/rift/checkpoints/v1",
        "refs/tags/v1.0",
    ] {
        assert!(blind::legal_ref_name(ok), "{ok}");
    }
    for bad in [
        "not/a/ref",
        "HEAD",
        "refs/../x",
        "refs/heads/a\nb",
        "refs/heads/a b",
        "-refs/heads/x",
        "refs/heads/.hidden",
        "refs/heads/x.lock",
        "refs//double",
        "refs/heads/x/",
        "refs/heads/x.",
        "refs/heads/a@{1}",
        "refs/heads/a~1",
        "refs/heads/a:b",
        "refs/heads/a?",
        "refs/heads/a*",
        "refs/heads/a[",
        "refs/heads/a\\b",
        "refs/heads/@",
        "",
    ] {
        assert!(!blind::legal_ref_name(bad), "{bad:?} must be refused");
    }
    assert!(blind::legal_oid(&"a".repeat(40)));
    assert!(blind::legal_oid(&"A".repeat(40)));
    assert!(blind::legal_oid(&"0".repeat(64)));
    assert!(!blind::legal_oid(&"a".repeat(39)));
    assert!(!blind::legal_oid(&"a".repeat(65)));
    assert!(!blind::legal_oid(&"g".repeat(40)));
}

/// The frame's refusals, one per rule, on a synthetic three-part container.
#[test]
fn the_frame_refuses_every_misframing_the_entry_names() {
    let bytes = blind::assemble(b"{}", b"seg\n", b"gitbundle\0bin");
    let frame = blind::parse_frame(&bytes).unwrap();
    assert_eq!(
        (frame.manifest.as_slice(), frame.segment.as_slice()),
        (&b"{}"[..], &b"seg\n"[..])
    );
    assert_eq!(frame.gitbundle, b"gitbundle\0bin");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let refused = |mutated: Vec<u8>, why: &str| {
        assert!(blind::parse_frame(&mutated).is_err(), "{why}");
    };
    refused(
        text.replacen("rift-bundle/1", "rift-bundle/2", 1)
            .into_bytes(),
        "unknown version",
    );
    refused(
        text.replacen("rift-bundle/1", "rift-bundle/01", 1)
            .into_bytes(),
        "non-canonical version",
    );
    refused(
        text.replacen("rift-bundle/1", "rift-bundle/0", 1)
            .into_bytes(),
        "version 0",
    );
    refused(
        text.replacen("manifest 2", "manifest 02", 1).into_bytes(),
        "non-canonical length",
    );
    refused(
        text.replacen("manifest 2", "manifest 3", 1).into_bytes(),
        "length off by one",
    );
    refused(
        text.replacen("manifest 2", "manifest 999", 1).into_bytes(),
        "length past the end",
    );
    refused(
        text.replacen("manifest 2\n{}\n", "manifest 2\n{}", 1)
            .into_bytes(),
        "missing part terminator",
    );
    refused(
        text.replacen("manifest 2", "manifest 536870913", 1)
            .into_bytes(),
        "over the 512 MiB ceiling",
    );
    refused(
        text.replacen("segment 4", "sgment 4", 1).into_bytes(),
        "unknown part name",
    );
    // Control: the part NAMES carry the order, so swapped CONTENTS still
    // frame — the reader never scans for a part by its bytes.
    assert!(blind::parse_frame(&blind::assemble(b"{}", b"gitbundle\0bin", b"seg\n")).is_ok());
    let mut trailing = bytes.clone();
    trailing.push(b'\n');
    refused(trailing, "trailing byte");
}

/// The document's worked vector: an empty window at a non-origin cursor.
/// The three segment lines shown there hash to the `segment_sha256` shown
/// there, the segment verifies, and `chain_head` is the real tip id.
#[test]
fn the_documents_worked_vector_reproduces() {
    let tip = "03".repeat(32);
    let segment =
        format!("rift-chain-segment/1\nfrom rift-cursor/1:1:{tip}\nto rift-cursor/1:1:{tip}\n");
    assert_eq!(
        blind::sha256_hex(segment.as_bytes()),
        "2d6bbd4dbb94b3ad3682ae0e15a91f0ccf22be6e57b8c28438ff4449ebd58dbb",
        "the worked vector's segment_sha256"
    );
    let seg = blind::parse_segment(segment.as_bytes()).unwrap();
    blind::verify_segment(&seg).unwrap();
    assert!(!seg.to.is_origin());
    assert_eq!(
        seg.to.checkpoint_id, tip,
        "chain_head is the REAL tip id, never empty here"
    );
}

/// The blind reader uses nothing of rift's own: no rift crate, no path into
/// this test crate, only std, sha2 and serde_json. Needles assembled so this
/// test's own text is not a hit.
#[test]
fn the_blind_reader_uses_nothing_of_rifts_own() {
    let source = include_str!("bundle_conformance/blind.rs");
    for needle in [
        ["rift_", "core"].concat(),
        ["rift_", "store"].concat(),
        ["rift_", "git"].concat(),
        ["rift_", "hashchain"].concat(),
        ["rift_", "shadow"].concat(),
        ["crate", "::"].concat(),
        ["super", "::"].concat(),
        ["chain_", "fixture"].concat(),
    ] {
        assert!(
            !source.contains(&needle),
            "the blind reader names `{needle}`"
        );
    }
    let uses: Vec<&str> = source.lines().filter(|l| l.starts_with("use ")).collect();
    for line in &uses {
        assert!(
            line.starts_with("use std::") || line.starts_with("use sha2::"),
            "an import outside std/sha2: {line}"
        );
    }
    assert!(
        source.contains("serde_json::"),
        "serde_json is the one other crate"
    );
}

// ---------------------------------------------------------------------------
// real bundles through steps 1–9
// ---------------------------------------------------------------------------

/// The shipped exporter's three bundles pass the blind reader's steps 1–9
/// into the repositories they attach to. `empty` now ACCEPTS: the tracked
/// `empty.verdict.txt` records the pre-B1 reader, and this harness
/// supersedes that script.
#[test]
fn the_three_recipe_bundles_are_accepted_by_the_blind_reader_end_to_end() {
    let f = fixtures();
    let case = case_dir("real-bundles");
    let scratch_fresh = empty_target(&case, "scratch-fresh");
    blind::verify(&f.full, &scratch_fresh).expect("full into a fresh repository");
    let scratch_full = case.join("scratch-holding");
    copy_dir(&f.holding_full, &scratch_full);
    blind::verify(&f.incr, &scratch_full).expect("incr into a repository holding the chain");
    let scratch_full2 = case.join("scratch-holding-2");
    copy_dir(&f.holding_full, &scratch_full2);
    let empty =
        blind::verify(&f.empty, &scratch_full2).expect("empty into a repository holding the chain");
    assert!(empty.segment.links.is_empty());
    assert!(empty.manifest.writer_conforming);
}

// ---------------------------------------------------------------------------
// the shipped importer, as a verdict
// ---------------------------------------------------------------------------

/// The shipped importer's answer: exit 0 ⇔ Accept, 1 | 65 ⇔ Refuse; any other
/// exit is a harness failure, not a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Accept,
    Refuse,
}

struct Shipped {
    verdict: Verdict,
    stdout: String,
    stderr: String,
}

fn shipped_import(target: &Path, bundle: &[u8], case: &Path) -> Shipped {
    let file = case.join("candidate.riftbundle");
    std::fs::write(&file, bundle).unwrap();
    let cache = case.join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let (code, stdout, stderr) = rift(
        target,
        &cache,
        &["--json", "bundle", "import", file.to_str().unwrap()],
    );
    let verdict = match code {
        0 => Verdict::Accept,
        1 | 65 => Verdict::Refuse,
        other => {
            panic!("the shipped importer exited {other}, outside its contract: {stdout}{stderr}")
        }
    };
    Shipped {
        verdict,
        stdout,
        stderr,
    }
}

fn refs_of(target: &Path) -> (Option<String>, Option<String>) {
    (
        ref_oid(target, chain_fixture::CHAIN_REF),
        ref_oid(target, chain_fixture::CHECKPOINTS_REF),
    )
}

// ---------------------------------------------------------------------------
// matrix — deterministic mutations of the real bundles
// ---------------------------------------------------------------------------

/// Which repository a case imports into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// No capture refs at all.
    Fresh,
    /// Already holds `full` (imported by the shipped binary).
    HoldingFull,
    /// Holds the unrelated one-position chain: divergent history.
    HoldingForeign,
}

/// One matrix case: the mutated bytes, where they go, and what both readers
/// must say — at the pure layer (steps 1–6) and end to end.
struct Case {
    name: &'static str,
    bundle: Vec<u8>,
    target: Target,
    expect_pure: Verdict,
    expect: Verdict,
    /// Bytes that must appear RAW in neither stdout nor stderr of the
    /// shipped importer (a hostile `repo_id` is provenance, never echoed).
    never_echoed: Option<&'static str>,
}

/// The three parts of a real bundle, for mutation.
struct Parts {
    manifest: blind::Manifest,
    segment: Vec<u8>,
    gitbundle: Vec<u8>,
}

fn parts_of(bytes: &[u8]) -> Parts {
    let frame = blind::parse_frame(bytes).expect("a real bundle frames");
    Parts {
        manifest: blind::parse_manifest(&frame.manifest).expect("a real manifest parses"),
        segment: frame.segment,
        gitbundle: frame.gitbundle,
    }
}

impl Parts {
    /// Re-derive both digests from the parts as they stand now.
    fn redigest(&mut self) {
        self.manifest.segment_sha256 = blind::sha256_hex(&self.segment);
        self.manifest.gitbundle_sha256 = blind::sha256_hex(&self.gitbundle);
    }

    /// Assemble with the manifest in conforming (compact, ordered) form.
    fn assemble(&self) -> Vec<u8> {
        blind::assemble(
            &blind::conforming_manifest_bytes(&self.manifest),
            &self.segment,
            &self.gitbundle,
        )
    }

    /// Assemble with the manifest bytes given verbatim.
    fn assemble_with_manifest(&self, manifest: &[u8]) -> Vec<u8> {
        blind::assemble(manifest, &self.segment, &self.gitbundle)
    }

    fn segment_text(&self) -> String {
        String::from_utf8(self.segment.clone()).expect("segment is UTF-8")
    }
}

/// A mutated manifest, digests re-derived unless the case targets step 3.
fn with_manifest(base: &[u8], redigest: bool, f: impl Fn(&mut blind::Manifest)) -> Vec<u8> {
    let mut p = parts_of(base);
    if redigest {
        p.redigest();
    }
    f(&mut p.manifest);
    p.assemble()
}

/// A mutated segment text, digests re-derived unless the case targets step 3.
fn with_segment(base: &[u8], redigest: bool, f: impl Fn(String) -> String) -> Vec<u8> {
    let mut p = parts_of(base);
    p.segment = f(p.segment_text()).into_bytes();
    if redigest {
        p.redigest();
    }
    p.assemble()
}

/// The raw container text with one textual substitution (framing cases).
fn with_raw(base: &[u8], from: &str, to: &str) -> Vec<u8> {
    let text = String::from_utf8_lossy(base).into_owned();
    assert!(
        text.contains(from),
        "the framing case's needle `{from}` is absent"
    );
    // The gitbundle part is binary; `from_utf8_lossy` could mangle it, so the
    // substitution is applied on the byte level instead.
    let at = base
        .windows(from.len())
        .position(|w| w == from.as_bytes())
        .expect("needle present as bytes");
    let mut out = base[..at].to_vec();
    out.extend_from_slice(to.as_bytes());
    out.extend_from_slice(&base[at + from.len()..]);
    out
}

/// The manifest serialized through `serde_json::Value`: keys sorted
/// alphabetically, i.e. REORDERED relative to the writer rule.
fn reordered_manifest(base: &[u8]) -> Vec<u8> {
    let p = parts_of(base);
    let value: serde_json::Value =
        serde_json::from_slice(&blind::conforming_manifest_bytes(&p.manifest)).unwrap();
    let bytes = serde_json::to_vec(&value).unwrap();
    assert_ne!(
        bytes,
        blind::conforming_manifest_bytes(&p.manifest),
        "the reorder must move bytes"
    );
    p.assemble_with_manifest(&bytes)
}

fn pretty_manifest(base: &[u8]) -> Vec<u8> {
    let p = parts_of(base);
    let value: serde_json::Value =
        serde_json::from_slice(&blind::conforming_manifest_bytes(&p.manifest)).unwrap();
    p.assemble_with_manifest(&serde_json::to_vec_pretty(&value).unwrap())
}

fn manifest_with_extra_member(base: &[u8]) -> Vec<u8> {
    let p = parts_of(base);
    let mut value: serde_json::Value =
        serde_json::from_slice(&blind::conforming_manifest_bytes(&p.manifest)).unwrap();
    value["note"] = serde_json::Value::from("stranger");
    p.assemble_with_manifest(&serde_json::to_vec(&value).unwrap())
}

fn manifest_without_member(base: &[u8], member: &str) -> Vec<u8> {
    let p = parts_of(base);
    let mut value: serde_json::Value =
        serde_json::from_slice(&blind::conforming_manifest_bytes(&p.manifest)).unwrap();
    value.as_object_mut().unwrap().remove(member);
    p.assemble_with_manifest(&serde_json::to_vec(&value).unwrap())
}

fn manifest_with_string_version(base: &[u8]) -> Vec<u8> {
    let p = parts_of(base);
    let mut value: serde_json::Value =
        serde_json::from_slice(&blind::conforming_manifest_bytes(&p.manifest)).unwrap();
    value["schema_version"] = serde_json::Value::from("1");
    p.assemble_with_manifest(&serde_json::to_vec(&value).unwrap())
}

/// Flip one byte at `at` inside a part, without re-digesting.
fn flip_segment_byte(base: &[u8]) -> Vec<u8> {
    let mut p = parts_of(base);
    // Inside the first link's merkle root: order and membership stay intact,
    // so only the digest can see it (the entry's own "why the digests exist").
    let root_at = p.segment_text().find("link ").expect("full has links") + 5 + 64 + 1;
    let b = p.segment[root_at];
    p.segment[root_at] = if b == b'0' { b'1' } else { b'0' };
    p.assemble()
}

fn flip_gitbundle_byte(base: &[u8]) -> Vec<u8> {
    let mut p = parts_of(base);
    let mid = p.gitbundle.len() / 2;
    p.gitbundle[mid] ^= 0x01;
    p.assemble()
}

/// The segment's first link with a different merkle root, digests re-derived:
/// the segment binds order and membership, not roots, so this ACCEPTS.
fn flip_root_with_redigest(base: &[u8]) -> Vec<u8> {
    with_segment(base, true, |text| {
        let at = text.find("link ").expect("full has links") + 5 + 64 + 1;
        let mut chars: Vec<char> = text.chars().collect();
        chars[at] = if chars[at] == '0' { '1' } else { '0' };
        chars.into_iter().collect()
    })
}

/// A foreign git-bundle part under `full`'s segment and cursors: the
/// `ref_list` must describe the foreign heads (G13) for the case to reach
/// the ref step, where the divergent history is refused.
fn foreign_gitbundle_under_full(full: &[u8], foreign: &[u8]) -> Vec<u8> {
    let mut p = parts_of(full);
    let other = parts_of(foreign);
    p.gitbundle = other.gitbundle;
    p.manifest.ref_list = other.manifest.ref_list;
    p.redigest();
    p.assemble()
}

/// The matrix: one row per documented rule, in the order the agreement test
/// runs them, built family by family so each family reads beside the step of
/// the entry it tests.
fn cases() -> Vec<Case> {
    let f = fixtures();
    [
        real_bundle_cases(f),
        framing_cases(&f.full),
        digest_cases(&f.full),
        segment_cases(&f.full),
        window_cases(f),
        manifest_shape_cases(&f.full),
        ref_cases(&f.full),
        provenance_cases(&f.full),
        foreign_history_cases(f),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The real bundles, where they attach: the recipe's bundles accepted into
/// the targets they were exported for.
fn real_bundle_cases(f: &Fixtures) -> Vec<Case> {
    let full = f.full.as_slice();
    vec![
        accept("full.into_fresh", full.to_vec(), Target::Fresh),
        accept(
            "full.into_holding_full_is_already_present",
            full.to_vec(),
            Target::HoldingFull,
        ),
        accept(
            "incr.into_holding_full",
            f.incr.clone(),
            Target::HoldingFull,
        ),
        accept(
            "empty.into_holding_full",
            f.empty.clone(),
            Target::HoldingFull,
        ),
    ]
}

/// Step 1, framing: the container version, the part lengths and the part
/// order, each refused.
fn framing_cases(full: &[u8]) -> Vec<Case> {
    let manifest_len = format!("manifest {}", parts_of(full).manifest_len());
    vec![
        refuse(
            "frame.container_version_2",
            with_raw(full, "rift-bundle/1\n", "rift-bundle/2\n"),
        ),
        refuse(
            "frame.container_version_01",
            with_raw(full, "rift-bundle/1\n", "rift-bundle/01\n"),
        ),
        refuse(
            "frame.container_version_0",
            with_raw(full, "rift-bundle/1\n", "rift-bundle/0\n"),
        ),
        refuse(
            "frame.length_noncanonical",
            with_raw(
                full,
                &manifest_len,
                &manifest_len.replace("manifest ", "manifest 0"),
            ),
        ),
        refuse(
            "frame.length_past_end",
            with_raw(full, &manifest_len, "manifest 99999999"),
        ),
        refuse(
            "frame.over_the_ceiling",
            with_raw(full, &manifest_len, "manifest 536870913"),
        ),
        refuse(
            "frame.part_out_of_order",
            with_raw(full, "\nsegment ", "\ngitbundle "),
        ),
        refuse("frame.trailing_byte", [full, b"\n"].concat()),
        refuse("frame.truncated", full[..full.len() - 1].to_vec()),
    ]
}

/// Step 3, digests: a byte flipped under an unchanged digest is refused, while
/// a root flipped with the digests re-derived is accepted, because the segment
/// binds order and membership, not roots.
fn digest_cases(full: &[u8]) -> Vec<Case> {
    vec![
        refuse(
            "digest.segment_byte_flipped_without_redigest",
            flip_segment_byte(full),
        ),
        refuse(
            "digest.gitbundle_byte_flipped_without_redigest",
            flip_gitbundle_byte(full),
        ),
        refuse(
            "digest.segment_sha256_wrong",
            with_manifest(full, true, |m| m.segment_sha256 = "0".repeat(64)),
        ),
        accept(
            "digest.root_flipped_with_redigest",
            flip_root_with_redigest(full),
            Target::Fresh,
        ),
    ]
}

/// Step 4, segment canonicity and verification: every non-canonical spelling
/// and every reordered or dropped link is refused.
fn segment_cases(full: &[u8]) -> Vec<Case> {
    vec![
        refuse(
            "segment.crlf",
            with_segment(full, true, |t| t.replace('\n', "\r\n")),
        ),
        refuse(
            "segment.no_final_newline",
            with_segment(full, true, |t| t.trim_end().to_owned()),
        ),
        refuse(
            "segment.uppercase_hex",
            with_segment(full, true, |t| {
                let mut lines: Vec<String> = t.lines().map(str::to_owned).collect();
                lines[3] = lines[3].to_uppercase().replace("LINK", "link");
                format!("{}\n", lines.join("\n"))
            }),
        ),
        refuse(
            "segment.header_01",
            with_segment(full, true, |t| t.replacen("segment/1", "segment/01", 1)),
        ),
        refuse(
            "segment.dropped_link",
            with_segment(full, true, |t| {
                let mut lines: Vec<&str> = t.lines().collect();
                lines.remove(4);
                format!("{}\n", lines.join("\n"))
            }),
        ),
        refuse(
            "segment.swapped_links",
            with_segment(full, true, |t| {
                let mut lines: Vec<&str> = t.lines().collect();
                lines.swap(3, 4);
                format!("{}\n", lines.join("\n"))
            }),
        ),
        refuse(
            "segment.blank_line",
            with_segment(full, true, |t| t.replacen('\n', "\n\n", 1)),
        ),
    ]
}

/// Step 5, the manifest disagrees with the segment it carries.
fn window_cases(f: &Fixtures) -> Vec<Case> {
    let full = f.full.as_slice();
    let pos1 = f.chain.cursor_at(1);
    vec![
        refuse(
            "window.manifest_from_differs",
            with_manifest(full, true, |m| m.from_cursor.clone_from(&pos1)),
        ),
        refuse(
            "window.chain_head_wrong",
            with_manifest(full, true, |m| m.chain_head = "0".repeat(64)),
        ),
        Case {
            name: "window.empty_chain_head_blank",
            bundle: with_manifest(&f.empty, true, |m| m.chain_head = String::new()),
            target: Target::HoldingFull,
            expect_pure: Verdict::Refuse,
            expect: Verdict::Refuse,
            never_echoed: None,
        },
    ]
}

/// Step 2, manifest shape: writer rules are not reader rules, so order and
/// whitespace are accepted while a missing, unknown or mistyped member is
/// refused.
fn manifest_shape_cases(full: &[u8]) -> Vec<Case> {
    vec![
        accept(
            "manifest.reordered",
            reordered_manifest(full),
            Target::Fresh,
        ),
        accept(
            "manifest.pretty_printed",
            pretty_manifest(full),
            Target::Fresh,
        ),
        refuse("manifest.unknown_member", manifest_with_extra_member(full)),
        refuse(
            "manifest.missing_member",
            manifest_without_member(full, "chain_head"),
        ),
        refuse(
            "manifest.schema_version_2",
            with_manifest(full, true, |m| m.schema_version = 2),
        ),
        refuse(
            "manifest.schema_version_string",
            manifest_with_string_version(full),
        ),
    ]
}

/// Step 6, refs: every illegal name and oid the entry enumerates, plus the
/// two that pass step 6 and fall to step 7's set equality.
fn ref_cases(full: &[u8]) -> Vec<Case> {
    vec![
        refuse(
            "refs.not_a_ref",
            with_manifest(full, true, |m| {
                "not/a/ref".clone_into(&mut m.ref_list[0].name);
            }),
        ),
        refuse(
            "refs.HEAD",
            with_manifest(full, true, |m| "HEAD".clone_into(&mut m.ref_list[0].name)),
        ),
        refuse(
            "refs.traversal",
            with_manifest(full, true, |m| {
                "refs/../x".clone_into(&mut m.ref_list[0].name);
            }),
        ),
        refuse(
            "refs.newline",
            with_manifest(full, true, |m| {
                "refs/heads/a\nb".clone_into(&mut m.ref_list[0].name);
            }),
        ),
        refuse(
            "refs.39_hex",
            with_manifest(full, true, |m| {
                m.ref_list[0].target.pop();
            }),
        ),
        refuse(
            "refs.65_hex",
            with_manifest(full, true, |m| m.ref_list[0].target.push('a')),
        ),
        // Step 6 accepts either case; step 7's set-equality against git's
        // own (lowercase) head lines then refuses — in both readers.
        pure_ok_git_refuses(
            "refs.uppercase_oid",
            with_manifest(full, true, |m| {
                m.ref_list[0].target = m.ref_list[0].target.to_uppercase();
            }),
        ),
        // G13: an empty ref_list under a bundle that carries heads.
        pure_ok_git_refuses(
            "refs.empty_ref_list",
            with_manifest(full, true, |m| m.ref_list.clear()),
        ),
    ]
}

/// `repo_id` is provenance: accepted, and never echoed raw.
fn provenance_cases(full: &[u8]) -> Vec<Case> {
    vec![Case {
        name: "repo_id.junk_is_provenance_only",
        bundle: with_manifest(full, true, |m| {
            "\u{1b}[31mjunk\nline2".clone_into(&mut m.repo_id);
        }),
        target: Target::Fresh,
        expect_pure: Verdict::Accept,
        expect: Verdict::Accept,
        never_echoed: Some("\u{1b}[31mjunk\nline2"),
    }]
}

/// Git-side: a foreign history and a non-fast-forward, both refused only
/// once git sees them.
fn foreign_history_cases(f: &Fixtures) -> Vec<Case> {
    let full = f.full.as_slice();
    vec![
        Case {
            name: "foreign.gitbundle_under_full_into_holding_full",
            bundle: foreign_gitbundle_under_full(full, &f.foreign),
            target: Target::HoldingFull,
            expect_pure: Verdict::Accept,
            expect: Verdict::Refuse,
            never_echoed: None,
        },
        Case {
            name: "foreign.full_into_divergent_history",
            bundle: full.to_vec(),
            target: Target::HoldingForeign,
            expect_pure: Verdict::Accept,
            expect: Verdict::Refuse,
            never_echoed: None,
        },
    ]
}

/// A row both readers refuse, into a fresh target.
fn refuse(name: &'static str, bundle: Vec<u8>) -> Case {
    Case {
        name,
        bundle,
        target: Target::Fresh,
        expect_pure: Verdict::Refuse,
        expect: Verdict::Refuse,
        never_echoed: None,
    }
}

/// A row both readers accept, into the target it names.
fn accept(name: &'static str, bundle: Vec<u8>, target: Target) -> Case {
    Case {
        name,
        bundle,
        target,
        expect_pure: Verdict::Accept,
        expect: Verdict::Accept,
        never_echoed: None,
    }
}

/// A row the pure steps accept and the git side refuses, into a fresh
/// target.
fn pure_ok_git_refuses(name: &'static str, bundle: Vec<u8>) -> Case {
    Case {
        name,
        bundle,
        target: Target::Fresh,
        expect_pure: Verdict::Accept,
        expect: Verdict::Refuse,
        never_echoed: None,
    }
}

impl Parts {
    fn manifest_len(&self) -> usize {
        blind::conforming_manifest_bytes(&self.manifest).len()
    }
}

// ---------------------------------------------------------------------------
// agreement — the blind verdict and the shipped importer agree on every case
// ---------------------------------------------------------------------------

/// Cases where the two readers are KNOWN to disagree, each with a root cause
/// in a doc defect or a shipped defect. SHRINK-ONLY: a row is removed in the
/// commit that fixes its cause, never added without one. The agreement test
/// asserts every listed row still disagrees, so a stale row is a red too.
const KNOWN_DISAGREEMENTS: &[&str] = &[];

fn target_for(kind: Target, case: &Path) -> PathBuf {
    match kind {
        Target::Fresh => fresh_target(case),
        Target::HoldingFull => target_holding_full(case),
        Target::HoldingForeign => {
            let target = fresh_target(case);
            let cache = case.join("seed-cache");
            std::fs::create_dir_all(&cache).unwrap();
            let file = case.join("foreign.riftbundle");
            std::fs::write(&file, &fixtures().foreign).unwrap();
            let (code, out, err) = rift(
                &target,
                &cache,
                &["--json", "bundle", "import", file.to_str().unwrap()],
            );
            assert_eq!(code, 0, "seeding the divergent target: {out}{err}");
            target
        }
    }
}

/// Every documented verdict agrees with the shipped importer: the blind
/// reader's pure verdict matches the case's expectation at steps 1–6, its
/// end-to-end verdict and the shipped importer's exit code match at steps
/// 1–9, and after every refusal the target's refs are exactly what they were.
#[test]
fn every_documented_verdict_agrees_with_the_shipped_importer() {
    let mut findings: Vec<String> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for case in cases() {
        assert!(seen.insert(case.name), "duplicate case name {}", case.name);
        let dir = case_dir(case.name);
        let target = target_for(case.target, &dir);
        let scratch = dir.join("blind-scratch");
        copy_dir(&target, &scratch);
        let before = refs_of(&target);

        let pure = match blind::verify_pure(&case.bundle) {
            Ok(_) => Verdict::Accept,
            Err(_) => Verdict::Refuse,
        };
        let blind_full = match blind::verify(&case.bundle, &scratch) {
            Ok(_) => Verdict::Accept,
            Err(_) => Verdict::Refuse,
        };
        let shipped = shipped_import(&target, &case.bundle, &dir);
        let after = refs_of(&target);

        if pure != case.expect_pure {
            findings.push(format!(
                "{}: blind pure verdict {pure:?}, expected {:?}",
                case.name, case.expect_pure
            ));
        }
        if blind_full != case.expect {
            findings.push(format!(
                "{}: blind end-to-end verdict {blind_full:?}, expected {:?}",
                case.name, case.expect
            ));
        }
        let known = KNOWN_DISAGREEMENTS.contains(&case.name);
        if known {
            if shipped.verdict == blind_full {
                findings.push(format!("{}: listed as a known disagreement but the readers now AGREE — shrink the registry", case.name));
            }
        } else if shipped.verdict != blind_full {
            findings.push(format!(
                "{}: shipped importer says {:?}, the blind reader says {blind_full:?}\n  stdout: {}\n  stderr: {}",
                case.name,
                shipped.verdict,
                shipped.stdout.trim(),
                shipped.stderr.trim()
            ));
        }
        if shipped.verdict == Verdict::Refuse && before != after {
            findings.push(format!(
                "{}: a refusal moved a ref: {before:?} -> {after:?}",
                case.name
            ));
        }
        if case.target == Target::Fresh
            && shipped.verdict == Verdict::Refuse
            && after != (None, None)
        {
            findings.push(format!(
                "{}: refs present after a refusal into a fresh repository",
                case.name
            ));
        }
        if let Some(raw) = case.never_echoed {
            if shipped.stdout.contains(raw) || shipped.stderr.contains(raw) {
                findings.push(format!("{}: hostile repo_id echoed raw", case.name));
            }
        }
    }
    assert!(
        findings.is_empty(),
        "disagreements:\n{}",
        findings.join("\n")
    );
    assert!(
        seen.len() >= 40,
        "the matrix shrank to {} cases",
        seen.len()
    );
}

/// The documented leniency, recorded rather than argued: compactness and
/// field order are WRITER rules, so a reordered or pretty-printed manifest
/// is ACCEPTED by both readers — and the blind reader records it as
/// non-conforming output (INFO), which is what a re-certification diff
/// looks at when the writer rule is ever promoted to a reader rule.
#[test]
fn a_reordered_or_pretty_manifest_is_accepted_and_recorded_as_nonconforming() {
    let f = fixtures();
    for (label, bundle) in [
        ("reordered", reordered_manifest(&f.full)),
        ("pretty", pretty_manifest(&f.full)),
    ] {
        let accepted = blind::verify_pure(&bundle).unwrap_or_else(|e| panic!("{label}: {e:?}"));
        assert!(
            !accepted.manifest.writer_conforming,
            "{label}: must be recorded as non-conforming"
        );
    }
    assert!(
        blind::verify_pure(&f.full)
            .unwrap()
            .manifest
            .writer_conforming
    );
}

/// **Decision 8, TAKEN 2026-08-29: an incremental bundle into a repository
/// with no chain is REFUSED by both readers.** The chain-segment entry's
/// attach rule is now `mod blind`'s step 6b, and the shipped importer applies
/// the same rule in `cmd_import` before any object is written (`from` ≠ origin
/// against an empty chain cannot bootstrap — "export from origin"). This test
/// was `…_accepted_today_decision_8_pending` while the rule lived only in the
/// store-side reference implementation; a follow-up landed it on both sides in
/// the same tree.
#[test]
fn an_incremental_bundle_into_an_empty_repository_is_refused_by_both_readers() {
    let f = fixtures();
    let dir = case_dir("decision-8");
    let target = fresh_target(&dir);
    let scratch = dir.join("scratch");
    copy_dir(&target, &scratch);
    let reason = match blind::verify(&f.incr, &scratch) {
        Err(blind::Refusal(m)) => m,
        Ok(_) => "accepted",
    };
    assert!(
        reason.starts_with("attach:"),
        "the blind reader must refuse at the attach step, not {reason}"
    );
    let shipped = shipped_import(&target, &f.incr, &dir);
    assert_eq!(
        shipped.verdict,
        Verdict::Refuse,
        "{}{}",
        shipped.stdout,
        shipped.stderr
    );
    assert!(
        shipped.stderr.contains("bootstrap") || shipped.stdout.contains("bootstrap"),
        "the shipped refusal names the bootstrap rule: {}{}",
        shipped.stdout,
        shipped.stderr
    );
}
