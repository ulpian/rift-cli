//! End-to-end: the four workflow verbs over rows real hooks wrote — each one
//! byte-identical across two processes, `recap` reporting what is there, and
//! a bounded window excluding what the unbounded one reports.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::{
    agent_read_hook, archive_head, rift_json, run_rift_stdout, seed_live_session_repo, sync_env,
};

/// **Determinism as a property of INVOCATIONS, not of one process — for ALL
/// FOUR workflow verbs.**
///
/// The exit criterion is "same window ⇒ byte-identical `--json`", and
/// the only test that spoke to it called the verb twice inside one process
/// (`workflows/data_tests.rs`). That is `f(x) == f(x)` over warm statics: it
/// cannot see a `HashMap` iteration order that is seeded per process, a
/// `SystemTime` read on a cold path, an address-dependent sort, or a value
/// cached on first use. Two processes can.
///
/// **It covered two verbs of four**, and `workflows/data_tests.rs` said its own
/// gap was closed by "`review_and_investigate_…`" — a sentence naming a test
/// that covers two DIFFERENT verbs from the two it was standing in for. That
/// sentence is corrected alongside this widening; a claim nothing would redden
/// is a claim nobody has tested.
/// `dispatch` and `recap` are driven here now, so all four are.
///
/// **The binary is the freshly built one, structurally.** `common::rift_cmd`
/// spawns the path Cargo exports as `CARGO_BIN_EXE_<name>`, which it defines
/// only after building this package's `[[bin]]` targets for this test run — so
/// the stale-binary hazard (a suite driving a binary it never relinked) cannot
/// arise through this door, and `only_the_chokepoint_names_the_binary` keeps it
/// the only door. (That guard is LEXICAL, and it caught this very paragraph
/// when it first spelled the variable out in full.)
///
/// The non-vacuity controls matter as much as the equalities: a test that only
/// asserts "twice the same" passes just as well for a verb that always answers
/// the same thing, which is precisely what `rift review` did when its data layer
/// was reading a sentinel and answering zero.
#[test]
fn every_workflow_verb_is_byte_identical_across_two_processes() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cache) = captured_repo(tmp.path());
    let env = sync_env(&cache);
    assert_the_fixture_holds_a_checkpoint(&repo, &env);

    // `rift review --json`, twice, in two processes.
    let review_args = ["review", "--profile", "full", "--json"];
    assert_eq!(
        verb(&repo, &env, &review_args),
        verb(&repo, &env, &review_args),
        "two `rift review` processes disagreed"
    );
    // …and a different profile is a different document, so the equality above
    // is not the equality of a constant.
    assert_ne!(
        verb(&repo, &env, &review_args),
        verb(&repo, &env, &["review", "--profile", "risk", "--json"]),
        "two profiles produced one document"
    );

    // `rift investigate`, twice, in two processes. The seed is an error
    // signature: an identifier by shape alone, so this asserts the verb rather
    // than the fixture's paths.
    let seed = format!("sha256:{}", "a".repeat(64));
    let other = format!("sha256:{}", "b".repeat(64));
    let inv = ["investigate", seed.as_str(), "--json"];
    assert_eq!(
        verb(&repo, &env, &inv),
        verb(&repo, &env, &inv),
        "two `rift investigate` processes disagreed"
    );
    // The scaffold is written by the FIRST run and never overwritten, so the
    // second run reporting the same path is part of the property, not a
    // coincidence of ordering.
    assert!(repo.join("findings.md").exists());

    // A different seed is a different investigation — which was only true of
    // the RENDER until the pack key started carrying the seed's hash.
    let a: serde_json::Value = serde_json::from_str(&verb(&repo, &env, &inv)).unwrap();
    let b: serde_json::Value = serde_json::from_str(&verb(
        &repo,
        &env,
        &["investigate", other.as_str(), "--json"],
    ))
    .unwrap();
    assert_ne!(
        a["pack"]["pack_key"], b["pack"]["pack_key"],
        "two seeds shared one pack key"
    );
}

/// The other two verbs of the four, under the same property and the same
/// non-vacuity controls: `dispatch` — the verb whose document the whole
/// workflow family is built on, and the one the sibling test was named after
/// not covering — and `recap` on both of its axes.
#[test]
fn dispatch_and_recap_are_byte_identical_across_two_processes() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cache) = captured_repo(tmp.path());
    let env = sync_env(&cache);
    assert_the_fixture_holds_a_checkpoint(&repo, &env);

    let dispatch_args = ["dispatch", "--json"];
    assert_eq!(
        verb(&repo, &env, &dispatch_args),
        verb(&repo, &env, &dispatch_args),
        "two `rift dispatch` processes disagreed"
    );
    // …and a BOUNDED window is a different document, so the equality above is
    // not the equality of a constant. `--until 0` is before every fact this
    // fixture holds, which also makes the sentinel arm observable: see
    // `a_bounded_recap_excludes_what_the_unbounded_one_reports`.
    assert_ne!(
        verb(&repo, &env, &dispatch_args),
        verb(&repo, &env, &["dispatch", "--until", "0", "--json"]),
        "a bounded window produced the unbounded document"
    );

    // `rift recap --json`, twice, on both axes: the agent split and the model
    // split take different branches of the screen, so one axis cannot certify
    // the other.
    for axis in [vec!["recap", "--json"], vec!["recap", "--model", "--json"]] {
        assert_eq!(
            verb(&repo, &env, &axis),
            verb(&repo, &env, &axis),
            "two `rift recap` processes disagreed on {axis:?}"
        );
    }
    assert_ne!(
        verb(&repo, &env, &["recap", "--json"]),
        verb(&repo, &env, &["recap", "--model", "--json"]),
        "two axes produced one document"
    );
}

/// A repository whose rows real hooks wrote: one agent tool call BEFORE the
/// commit, so the window holds something `recap` reads and `dispatch` does not
/// (`tool_mix` comes from `tool_call`, which no other fact here would fill),
/// then one archive. A report verb refuses an uninitialized repository outright
/// (`error: not_initialized`), so the marker directory is part of the fixture
/// rather than an incantation. Returns `(repo, cache)`.
fn captured_repo(tmp: &Path) -> (PathBuf, PathBuf) {
    let repo = seed_live_session_repo(tmp);
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    let cache = tmp.join("cache");
    let env = sync_env(&cache);
    agent_read_hook(&repo, "e2e-live-1", 1, &repo.join("a.txt"), &env);
    archive_head(&repo, &env);
    (repo, cache)
}

/// The fixture must actually hold a checkpoint, or every equality about a
/// verb's document is about the empty report.
fn assert_the_fixture_holds_a_checkpoint(repo: &Path, env: &[(&str, &str)]) {
    let d = rift_json(repo, &["dispatch", "--json"], env);
    assert!(
        d["checkpoints"]["count"].as_u64().unwrap_or(0) >= 1,
        "the fixture must hold a checkpoint: {d}"
    );
}

/// One verb, one process: its stdout, which must be an answer.
fn verb(repo: &Path, env: &[(&str, &str)], args: &[&str]) -> String {
    let (code, out) = run_rift_stdout(repo, args, env);
    assert_eq!(code, 0, "{args:?}: {out}");
    assert!(!out.is_empty(), "{args:?} produced nothing");
    out
}

/// **`rift recap` over a repository that really holds rows** — the verb had
/// zero integration tests, and the historical trap is exactly the one a
/// vacuous fixture would walk into.
///
/// Native checkpoints now record `created_at` from the capture event's own
/// time; every shard archived BEFORE that fix carries the
/// `CREATED_AT_UNKNOWN` sentinel (`-1`) forever, and for those a BOUNDED
/// window still filters the sentinel: the checkpoint is placed by its
/// sessions or reported as unresolved — the derivation that is the only
/// reason `rift dispatch` stopped answering `checkpoints: 0` on a repository
/// holding 103. So this drives the shipped binary over a repository whose rows
/// were written by real hooks, and asserts the counts are non-zero rather than
/// asserting the shape of an empty document.
#[test]
fn recap_reports_the_rows_real_hooks_wrote() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cache) = captured_repo(tmp.path());
    let env = sync_env(&cache);

    let (code, out) = run_rift_stdout(&repo, &["recap", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let d: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(d["schema_version"], 2, "the screened contract: {d}");
    assert!(
        d["sessions"].as_u64().unwrap_or(0) >= 1,
        "recap reported no session on a repo whose hooks wrote one: {d}"
    );
    assert!(
        d["checkpoints"].as_u64().unwrap_or(0) >= 1,
        "recap reported no checkpoint on a repo holding one: {d}"
    );
    assert!(
        d["tool_mix"].as_array().is_some_and(|a| !a.is_empty()),
        "the `Read` hook wrote a tool_call and recap did not see it: {d}"
    );
    assert!(
        d["split"]["counts"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "recap split nothing: {d}"
    );
    // The agent axis rides verbatim — `claude-code` is a registered adapter
    // token — which is the half of `crate::screen` a hostile fixture cannot
    // show.
    assert_eq!(d["split"]["by"], "agent");
    assert_eq!(d["split"]["counts"][0]["key"], "claude-code");

    // The human render is not the JSON, and it is not empty either.
    let (hcode, human) = run_rift_stdout(&repo, &["recap"], &env);
    assert_eq!(hcode, 0, "{human}");
    assert!(
        human.contains("recap  sessions") && human.contains("claude-code"),
        "the human recap must resolve what the document counts: {human}"
    );
}

/// **The window bound, documented from the surface that exposes it.**
///
/// A checkpoint this fixture archives carries its capture event's own
/// `created_at` (the `-1` production write is closed), and `--until 1`
/// ends the window at epoch+1ms — before every fact this fixture holds — so
/// the bounded recap must report zero where the unbounded one reports rows,
/// and the two must not be the same document. (Shards archived before the
/// fix carry the sentinel forever; for those rows the window still places
/// through sessions or reports unresolved, and the sentinel arm keeps its
/// own pins in `workflows::data_tests`.)
#[test]
fn a_bounded_recap_excludes_what_the_unbounded_one_reports() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cache) = captured_repo(tmp.path());
    let env = sync_env(&cache);

    let read = |args: &[&str]| -> serde_json::Value {
        let (code, out) = run_rift_stdout(&repo, args, &env);
        assert_eq!(code, 0, "{args:?}: {out}");
        serde_json::from_str(&out).unwrap()
    };
    let unbounded = read(&["recap", "--json"]);
    let bounded = read(&["recap", "--since", "0", "--until", "1", "--json"]);

    assert!(
        unbounded["sessions"].as_u64().unwrap_or(0) >= 1,
        "the control must be non-empty: {unbounded}"
    );
    assert_eq!(
        bounded["sessions"], 0,
        "a window ending at epoch+1ms holds no session: {bounded}"
    );
    assert_eq!(
        bounded["checkpoints"], 0,
        "…and no checkpoint: its recorded capture time sits outside a window \
         that ends at epoch+1ms: {bounded}"
    );
    assert!(
        bounded["tool_mix"].as_array().is_some_and(Vec::is_empty),
        "the tool histogram is anchored to its own start times: {bounded}"
    );
}
