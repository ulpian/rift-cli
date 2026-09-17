//! The two halves, together: `rift`'s side of the seam driving the child it
//! re-spawns — this very binary, on its hidden `hooks doc` route.
//!
//! Everything else about the child is tested against stand-in scripts, which
//! is the right instrument for "what does the parent do when the child lies".
//! This file is the other question — does the thing cargo actually built
//! answer the thing `rift` actually spawns — and it lives here because the
//! binary under test is the one that converts.
#![cfg(feature = "doc")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::Path;
use std::time::Duration;

use rift_recall_doc::{
    output_ceiling, ChildProcessConverter, ConvertCaps, DocConverter, DocError, DEFAULT_BUDGET,
    MIN_OUTPUT_BYTES,
};

mod common;

const CSV: &[u8] = b"name,cost\nalpha,1\nbeta,2\n";

/// A budget no spawn can plausibly miss, for the assertions that are about
/// conversion rather than about time.
const PATIENT: Duration = Duration::from_secs(30);

/// The route the binary re-spawns itself on. Spelled here, beside the tests,
/// so a route that moved would fail this file rather than be followed by it.
const ROUTE: [&str; 2] = ["hooks", "doc"];

/// The converter production builds — the binary under test, on the route —
/// named through the shared chokepoint's own spelling of the binary. The
/// route opens no store and discovers no repository, so the working
/// directory a spawn inherits cannot leak into anything.
fn converter() -> ChildProcessConverter {
    ChildProcessConverter::new(common::rift_bin(), ROUTE)
}

/// ★ The behaviour the hook path depends on: a document that is not in the
/// sidecar cache is converted by the re-spawned child inside the deadline,
/// and when it is not — a cold process, a loaded machine, a parser that will
/// not finish — the caller gets a refusal it can fall back on rather than a
/// wait, a hang or a panic. Both arms are asserted, because on a hook path the
/// second one is the one that protects the developer.
#[test]
fn a_cache_miss_converts_through_the_respawned_binary_within_the_deadline_or_falls_back() {
    let converter = converter();
    let caps = ConvertCaps::default();

    // Arm 1: the conversion itself, with time taken out of the question.
    let markdown = converter
        .clone()
        .with_budget(PATIENT)
        .convert(CSV, "csv", &caps)
        .expect("the child converts a document it can read");
    assert!(markdown.text.contains("| name | cost |"), "{markdown:?}");
    assert!(markdown.text.contains("| beta | 2 |"), "{markdown:?}");
    assert_eq!(markdown.sheets.len(), 1);
    assert_eq!(markdown.sheets[0].rows, 2);

    // Arm 2: the production budget. Either it converted, or it fell back —
    // and the fallback is a refusal, never a panic and never a wait. Which of
    // the two happens depends on the machine, so the test asserts the property
    // that must hold on both. This arm is also the latency canary for a child
    // that is now the whole binary: a deadline here on an idle machine is a
    // route that got slower than the budget it runs under.
    assert_eq!(converter.budget(), DEFAULT_BUDGET);
    let started = std::time::Instant::now();
    match converter.convert(CSV, "csv", &caps) {
        Ok(markdown) => assert!(markdown.text.contains("| alpha | 1 |"), "{markdown:?}"),
        Err(error) => assert_eq!(error, DocError::Engine { code: "deadline" }),
    }
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "a conversion on the hook path took {:?}",
        started.elapsed()
    );

    // Arm 3: a budget nothing can meet, so the fallback is not left to the
    // machine's mood. A deadline that had to be waited out would be a hook
    // that blocks the developer, which is the one thing forbidden here.
    assert_eq!(
        converter
            .with_budget(Duration::ZERO)
            .convert(CSV, "csv", &caps),
        Err(DocError::Engine { code: "deadline" })
    );
}

/// ★ The converter spawns this very binary and nothing on `PATH`: the program
/// a self-spawn names is the running executable by its real path, the prefix
/// is the route, and the route answers with a real conversion.
///
/// The self-spawn is asked in this test process, so it names the test harness
/// rather than `rift` — which is the point: the rule is "the executable that
/// is running", whatever it is, and the second half drives the real binary on
/// the same route to show the route is the one that converts.
#[test]
fn the_converter_spawns_this_very_binary_and_nothing_on_path() {
    let named = ChildProcessConverter::self_exec(ROUTE).expect("a test binary can name itself");
    let exe = std::env::current_exe().unwrap();
    assert_eq!(
        named.program(),
        std::fs::canonicalize(&exe).unwrap_or(exe).as_path()
    );
    assert!(named.program().is_file());
    assert_eq!(named.budget(), DEFAULT_BUDGET);
    assert_eq!(
        named.prefix_args(),
        &[
            std::ffi::OsString::from("hooks"),
            std::ffi::OsString::from("doc")
        ]
    );

    // And the real binary, on that route, is a converter and not merely a
    // program that accepts the arguments.
    assert!(converter()
        .with_budget(PATIENT)
        .convert(CSV, "csv", &ConvertCaps::default())
        .is_ok());
}

/// No program is not an error the developer ever sees: the caller is handed a
/// refusal, leaves the read untouched, and the agent opens the file it asked
/// for. Nothing is written, nothing is retried, nothing is logged at them.
#[test]
fn a_missing_program_leaves_the_read_untouched() {
    let absent = Path::new(common::rift_bin()).with_file_name("rift-that-was-never-built");
    assert!(!absent.exists());
    let started = std::time::Instant::now();
    assert_eq!(
        ChildProcessConverter::new(&absent, ROUTE).convert(CSV, "csv", &ConvertCaps::default()),
        Err(DocError::Engine {
            code: "helperMissing"
        })
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// The command line the seam spawns, over a real file, end to end: the route
/// reads the document itself when it is given a path, prints one object, and
/// says in its exit status which kind of answer it was.
#[test]
fn the_route_converts_a_path_it_is_given_and_prints_one_object() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("costs.csv");
    std::fs::write(&path, CSV).unwrap();

    let out = common::rift_cmd(tmp.path())
        .args(["hooks", "doc", "convert"])
        .arg(&path)
        .arg("--json")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(
        text.starts_with(r#"{"pages":null,"sheets":[{"name":"","rows":2"#),
        "{text}"
    );
    assert!(text.contains(r#""status":"ok""#), "{text}");

    // Without `--json` it is a converter a human can read, and the Markdown is
    // the whole of standard output.
    let out = common::rift_cmd(tmp.path())
        .args(["hooks", "doc", "convert"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8(out.stdout)
        .unwrap()
        .contains("| beta | 2 |"));

    // A document it cannot read is a refusal with a reason and a non-zero
    // status, and still exactly one object.
    let broken = tmp.path().join("broken.docx");
    std::fs::write(&broken, b"not a document").unwrap();
    let out = common::rift_cmd(tmp.path())
        .args(["hooks", "doc", "convert"])
        .arg(&broken)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.starts_with(r#"{"code":"engine","engine_code":"#),
        "{text}"
    );

    // And an invocation that never gets as far as a conversion says so on
    // standard error, with nothing on standard output to mistake for an answer.
    let out = common::rift_cmd(tmp.path())
        .args(["hooks", "doc", "convert", "-", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert!(!out.stderr.is_empty(), "{out:?}");
}

/// ★ The case a separate process was kept for, over a real document rather
/// than a script pretending to be one: a zip-shaped file of 846 bytes whose
/// single row is thirty thousand columns wide, which converts to 630 kB.
///
/// The input cap cannot see this coming — the file is under it — so the answer
/// is refused on its way back, and the parent never holds it. Which half
/// refuses is deliberately not asserted: the child declines to send an answer
/// past the ceiling its caller declared, and the caller refuses to read past
/// the same ceiling if it is sent one anyway (`child.rs` tests that half
/// against a stand-in that ignores the limit). What matters here is that a
/// real expanding document costs one refusal.
#[test]
fn a_document_that_expands_past_the_ceiling_is_refused_and_never_read_whole() {
    let bytes = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/rift-recall-doc/fixtures/expansion.ods"),
    )
    .expect("the expansion fixture");
    assert!(bytes.len() < 1024, "{} bytes", bytes.len());

    let caps = ConvertCaps {
        max_input_bytes: 4096,
        ..ConvertCaps::default()
    };
    assert_eq!(output_ceiling(&caps), MIN_OUTPUT_BYTES);
    let started = std::time::Instant::now();
    assert_eq!(
        converter()
            .with_budget(PATIENT)
            .convert(&bytes, "ods", &caps),
        Err(DocError::Engine {
            code: "resourceLimit"
        })
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the expanding document was waited out: {:?}",
        started.elapsed()
    );

    // And it really does expand: under a ceiling that fits it, the same 846
    // bytes convert to more than eight ceilings of Markdown. Without this arm
    // the refusal above could be a fixture that simply failed to parse.
    let markdown = converter()
        .with_budget(PATIENT)
        .convert(&bytes, "ods", &ConvertCaps::default())
        .expect("the same document, under a ceiling that fits its answer");
    let ceilings = markdown.text.len() as u64 / MIN_OUTPUT_BYTES;
    assert!(
        ceilings > 8,
        "{ceilings} ceilings of Markdown from {} bytes",
        bytes.len()
    );
}
