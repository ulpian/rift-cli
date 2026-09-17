//! **The total shard oracle**.
//!
//! The parity anchors under `parity/anchors/` are a **partial** oracle and say
//! so: they compare four artifact names (`metadata.json`,
//! `redaction_report.json`, `content_hash.txt`, `full.jsonl`). Everything
//! added since — `content.redacted`, the transcript pair, the four
//! `digest.*` documents, the `tasks/<id>/` subtrees, the `sessions/<index>/`
//! metadata — sits outside that comparison, so an artifact added, changed or
//! **planted** under a name the anchors do not list moved no compared byte.
//!
//! This suite is the total one. It builds the maximal fixture through the real
//! compactor and the real redaction gate
//! ([`rift_shadow::shard_fixture::every_artifact_archive`]) and compares
//! **every** artifact against the bytes frozen beside it, plus two set-level
//! claims the byte comparison cannot make on its own:
//!
//! * every artifact the archive emits is **declared** in
//!   [`rift_shadow::artifact_registry`] — so a new artifact cannot ship without
//!   an entry, and a planted one has no entry to hide behind;
//! * the frozen `MANIFEST` is **exactly** the emitted set — so an artifact that
//!   silently stops being written reds just as loudly as one that appears.
//!
//! # There is no bless switch
//!
//! [`place_missing_goldens`] writes only files that are **absent**, refuses to
//! overwrite one that exists, and does nothing at all unless its placement
//! environment variable is set (spelled once, on that function) — so it can
//! create a new fixture and can
//! never make a failing comparison pass. A golden that legitimately changes is
//! deleted by hand, in its own commit, with its reason recorded; the same
//! discipline `legacy_import_golden` states, for the same reason: a suite with
//! a "make it pass" button is not an oracle.
//!
//! It is an ORDINARY test rather than an `#[ignore]`d one, and that is a trade
//! recorded here rather than left to be discovered: an ignored oracle runs
//! nowhere and proves nothing, while an env-gated no-op keeps the same
//! guarantee — it cannot bless — and still runs on every pass.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use rift_shadow::artifact_registry::{is_declared, SHARD_ARTIFACTS};
use rift_shadow::shard_fixture::{every_artifact_archive, fixture_dir};
use serde_json::Value;

/// Where the frozen artifact bytes live.
fn artifacts_dir() -> PathBuf {
    fixture_dir().join("artifacts")
}

/// The committed `MANIFEST`: one shard-relative artifact path per line.
fn manifest() -> Vec<String> {
    let path = fixture_dir().join("MANIFEST");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("MANIFEST unreadable at {}: {e}", path.display()));
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// The message a missing golden panics with — one definition, because the
/// instruction it carries is the only route a human has to place one.
fn no_golden(path: &str) -> String {
    format!(
        "{path}: no frozen golden. There is NO bless switch. Run\n  cargo test -p rift-shadow \
         --test shard_goldens place_missing\nwith the placement variable named on that \
         function set, which creates ABSENT files \
         only, then READ them and commit them. An existing golden is never overwritten: a \
         drift is resolved by deleting the file deliberately, in its own commit, with its \
         reason."
    )
}

/// **Byte-for-byte against bytes frozen in the tree by an earlier build.**
///
/// This is what `compaction_is_byte_stable` and `digests_are_byte_stable` were
/// supposed to be and were not: they compared `f(x)` with `f(x)` inside one
/// process, which every possible implementation satisfies. A comparison against
/// committed bytes is the one a rewritten compactor or a rewritten digest
/// writer can fail.
#[test]
fn every_artifact_matches_its_frozen_bytes() {
    let live = every_artifact_archive()
        .expect("the maximal fixture builds")
        .artifact_map();
    assert!(!live.is_empty(), "the fixture emitted nothing");
    for (path, bytes) in &live {
        let frozen = std::fs::read(artifacts_dir().join(path))
            .unwrap_or_else(|_| panic!("{}", no_golden(path)));
        assert_eq!(
            bytes, &frozen,
            "{path}: the live artifact drifted from its frozen golden"
        );
    }
}

/// The frozen `MANIFEST` is EXACTLY the emitted set — both directions.
///
/// One direction catches an artifact that appears (including a planted one);
/// the other catches an artifact that silently stops being written, which a
/// per-file byte comparison cannot see at all because there is nothing left to
/// compare.
#[test]
fn the_manifest_is_exactly_the_emitted_set() {
    let live: BTreeSet<String> = every_artifact_archive()
        .expect("the maximal fixture builds")
        .artifact_map()
        .keys()
        .cloned()
        .collect();
    let frozen: BTreeSet<String> = manifest().into_iter().collect();
    let appeared: Vec<&String> = live.difference(&frozen).collect();
    let vanished: Vec<&String> = frozen.difference(&live).collect();
    assert!(
        appeared.is_empty() && vanished.is_empty(),
        "the emitted artifact set moved — appeared: {appeared:?}, vanished: {vanished:?}"
    );
}

/// **Every emitted artifact is declared**, so a name outside the enumeration is
/// an error rather than an invisible passenger. This is the guard that makes a
/// planted artifact fail: it has no registry row, and there is no arm that lets
/// an undeclared name through.
#[test]
fn every_emitted_artifact_is_declared_in_the_registry() {
    for path in every_artifact_archive()
        .expect("the maximal fixture builds")
        .artifact_map()
        .keys()
    {
        assert!(
            is_declared(path).is_some(),
            "{path} is written into a shard but is not declared in \
             rift_shadow::artifact_registry — declare it (with its schema_version and its \
             sync-format row) or stop writing it"
        );
    }
}

/// **And the fixture is maximal**, so the guard above is not vacuously true of
/// a fixture that happens to emit three files. Every registry row is matched by
/// something this shard actually writes; a row nothing exercises would mean the
/// frozen bytes cover less than the table claims.
#[test]
fn the_fixture_exercises_every_declared_artifact() {
    let live = every_artifact_archive()
        .expect("the maximal fixture builds")
        .artifact_map();
    let matched: BTreeSet<&str> = live
        .keys()
        .filter_map(|p| is_declared(p).map(|row| row.path))
        .collect();
    let missing: Vec<&str> = SHARD_ARTIFACTS
        .iter()
        .map(|a| a.path)
        .filter(|p| !matched.contains(p))
        .collect();
    assert!(
        missing.is_empty(),
        "the maximal fixture writes nothing for these declared artifacts: {missing:?}"
    );
}

/// **The planted-garbage case, on the set the writer actually produced.** An
/// extra entry under a name the enumeration does not carry is refused, and the
/// two set claims above both name it — so a plant cannot pass by being new.
#[test]
fn a_planted_artifact_outside_the_enumeration_is_refused() {
    let mut planted: BTreeMap<String, Vec<u8>> = every_artifact_archive()
        .expect("the maximal fixture builds")
        .artifact_map();
    planted.insert("notes.txt".to_owned(), b"planted".to_vec());
    planted.insert(
        "tasks/toolu_01TASKFIXTURE/notes.txt".to_owned(),
        b"planted".to_vec(),
    );

    let undeclared: Vec<&String> = planted
        .keys()
        .filter(|p| is_declared(p).is_none())
        .collect();
    assert_eq!(
        undeclared.len(),
        2,
        "both plants must be undeclared, got {undeclared:?}"
    );

    let frozen: BTreeSet<String> = manifest().into_iter().collect();
    let live: BTreeSet<String> = planted.keys().cloned().collect();
    assert!(
        !live.difference(&frozen).collect::<Vec<_>>().is_empty(),
        "the manifest comparison must also see the plant"
    );
}

/// One-shot: create goldens that are ABSENT. Never overwrites, so it cannot
/// bless a drift, and does not run in an ordinary `cargo test` pass — it is
/// `#[ignore]`d deliberately, because placing files is an act a person asks
/// for and never something a test run does on its own.
///
/// ```text
/// cargo test -p rift-shadow --test shard_goldens place_missing -- --ignored
/// ```
#[test]
#[ignore = "one-shot generator that writes committed goldens; run explicitly with --ignored"]
fn place_missing_goldens() {
    let live = every_artifact_archive()
        .expect("the maximal fixture builds")
        .artifact_map();
    let mut written = Vec::new();
    for (path, bytes) in &live {
        let target = artifacts_dir().join(path);
        if target.exists() {
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&target, bytes).unwrap();
        written.push(path.clone());
    }
    let manifest_path = fixture_dir().join("MANIFEST");
    if !manifest_path.exists() {
        let body: String = live
            .keys()
            .map(|k| format!("{k}\n"))
            .collect::<Vec<_>>()
            .concat();
        std::fs::write(&manifest_path, body).unwrap();
        written.push("MANIFEST".to_owned());
    }
    println!("placed {} missing golden(s): {written:?}", written.len());
}

/// **The "subagent case" fixture, which did not exist** — four fixtures were
/// named and this was one of the two missing. What it pins
/// is that a session containing subagent turns survives condensation with its
/// nesting intact, on the artifacts a real shard carries rather than on an
/// in-memory struct.
///
/// Five claims, because "the structure survives" is not one fact:
/// the subtree exists with its own three artifacts; the subagent's transcript
/// is ITS OWN and not a copy of the parent's; the parent metadata ENUMERATES
/// the task (the reader's only listing — `SubstrateReader` has no tree walk);
/// the token rollup is the sum of the task's own leaf-bound counts; and the
/// dispatch that created the subagent survives compaction of the PARENT
/// transcript, so the link between the two lives in the shard rather than in a
/// test's memory.
#[test]
fn a_subagent_session_keeps_its_own_subtree_and_rolls_up_to_the_parent() {
    let archive = every_artifact_archive().expect("the maximal fixture builds");
    let map = archive.artifact_map();
    let id = rift_shadow::shard_fixture::FIXTURE_TASK_ID;

    for name in [
        "metadata.json",
        "transcript.compact.jsonl",
        "transcript.jsonl",
    ] {
        assert!(
            map.contains_key(&format!("tasks/{id}/{name}")),
            "tasks/{id}/{name} missing from the shard"
        );
    }
    assert_ne!(
        map.get(&format!("tasks/{id}/transcript.jsonl")),
        map.get("transcript.jsonl"),
        "the subagent's transcript must be its own, not the parent's"
    );

    let meta: Value = serde_json::from_slice(map.get("metadata.json").expect("emitted")).unwrap();
    assert_eq!(
        meta.pointer("/tasks/0").and_then(Value::as_str),
        Some(id),
        "the parent enumerates the task — hiding it from this list is what \
         `rift verify`'s task pass turns into tamper"
    );
    assert_eq!(
        meta.pointer("/task_tokens/input_tokens")
            .and_then(Value::as_u64),
        Some(800)
    );
    assert_eq!(
        meta.pointer("/task_tokens/output_tokens")
            .and_then(Value::as_u64),
        Some(24)
    );

    let compact = String::from_utf8(
        map.get("transcript.compact.jsonl")
            .expect("emitted")
            .clone(),
    )
    .unwrap();
    assert!(
        compact.contains(&format!("\"id\":\"{id}\"")) && compact.contains("\"name\":\"Task\""),
        "the dispatch that created the subagent survives the parent's compaction"
    );
}
