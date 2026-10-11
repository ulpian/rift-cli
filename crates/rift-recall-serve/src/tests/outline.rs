//! The outline writer, exercised where the redaction gate can mint the bytes
//! it takes: an outline is only ever bound from redacted bytes, and only for a
//! code path the serve policy serves.

use rift_core::{RedactedBytes, SecretBytes};
use rift_recall_ledger::validity_store::{
    bind_read_outlines, put_file_outline, resolve, token_counts, OutlineBatch, OutlineBindCounts,
    OutlineCounts, OutlineSource, MAX_OUTLINES_PER_BATCH, MAX_OUTLINE_SOURCE_BYTES,
};
use rift_recall_substrate::worktree::sha256_hex;
use rift_recall_validity::{Validity, WorktreeSnapshot};
use rift_store::{CanonicalStore, PlainCipher, SqliteStore};

use super::fixtures::*;

const OUTLINED: &str = "pub struct A {\n    x: u32,\n}\n\nfn b() -> u32 {\n    1\n}\n";

/// `bytes` through the one redaction gate.
pub(super) fn redacted(bytes: &[u8]) -> RedactedBytes {
    rift_shadow_redact::redact(&SecretBytes::new(bytes.to_vec()))
        .unwrap()
        .redacted
}

/// Bind `bytes` at `path` through the real writer, counting tokens by byte
/// length so the figures are known in advance.
pub(super) fn bind_outline(store: &SqliteStore, path: &str, bytes: &[u8]) -> Option<String> {
    bind_outline_refusing(store, path, bytes, &|_| false)
}

fn bind_outline_refusing(
    store: &SqliteStore,
    path: &str,
    bytes: &[u8],
    refuses_path: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let source = redacted(bytes);
    let counts = OutlineCounts::measure(path, &source, &byte_count, "cl100k_base").unwrap_or(
        OutlineCounts {
            tokenizer: "cl100k_base",
            whole_file: 1,
            definitions: Vec::new(),
        },
    );
    let file = OutlineSource {
        path,
        source: &source,
        policy_version: 1,
        refuses_path,
    };
    block(put_file_outline(store, &PlainCipher, &file, &counts)).unwrap()
}

/// A line holding an access-key-shaped literal, assembled at run time so no
/// key-shaped literal sits in a tracked file for the census to count.
fn key_line() -> String {
    let key = ["AK", "IA", "Z7Q2XK4LMNOP3RST"].concat();
    format!("const KEY: &str = \"{key}\";\n")
}

fn byte_count(text: &str) -> u32 {
    u32::try_from(text.len()).unwrap()
}

fn snapshot(path: &str, bytes: &[u8]) -> WorktreeSnapshot {
    let mut snap = WorktreeSnapshot::new();
    snap.insert(path.to_owned(), sha256_hex(bytes));
    snap
}

fn rows(store: &SqliteStore, table: &str) -> i64 {
    block(store.count(table)).unwrap()
}

#[test]
fn an_outline_binds_with_the_cost_of_each_definition_beside_it() {
    let store = open_store();
    let key = bind_outline(&store, "src/a.rs", OUTLINED.as_bytes()).unwrap();
    let verdict = block(resolve(
        &store,
        &PlainCipher,
        &snapshot("src/a.rs", OUTLINED.as_bytes()),
        &key,
    ))
    .unwrap();
    let Validity::Valid(summary) = verdict else {
        panic!("a fresh outline resolves valid");
    };
    assert_eq!(
        summary.digest(),
        b"7 lines, 54 bytes, 2 top-level defs\nstruct A L1-L3\nfn b L5-L7\n"
    );
    let counts = block(token_counts(&store, &key)).unwrap();
    let spans: Vec<(u32, u32, u32, u32)> = counts
        .iter()
        .map(|c| (c.ordinal, c.line_lo, c.line_hi, c.tokens))
        .collect();
    assert_eq!(spans, [(0, 1, 7, 54), (1, 1, 3, 29), (2, 5, 7, 24)]);
    assert!(counts.iter().all(|c| c.tokenizer == "cl100k_base"));
}

#[test]
fn binding_the_same_bytes_twice_writes_nothing_new() {
    let store = open_store();
    let first = bind_outline(&store, "src/a.rs", OUTLINED.as_bytes()).unwrap();
    let second = bind_outline(&store, "src/a.rs", OUTLINED.as_bytes()).unwrap();
    assert_eq!(first, second);
    assert_eq!(rows(&store, "summary_binding"), 1);
    assert_eq!(rows(&store, "summary_token_count"), 3);
}

#[test]
fn a_one_byte_change_binds_under_another_key_and_the_old_one_goes_stale() {
    let store = open_store();
    let old = bind_outline(&store, "src/a.rs", OUTLINED.as_bytes()).unwrap();
    let edited = OUTLINED.replace("1\n", "2\n");
    let new = bind_outline(&store, "src/a.rs", edited.as_bytes()).unwrap();
    assert_ne!(old, new);
    let verdict = block(resolve(
        &store,
        &PlainCipher,
        &snapshot("src/a.rs", edited.as_bytes()),
        &old,
    ))
    .unwrap();
    assert!(matches!(verdict, Validity::Invalid { .. }));
}

#[test]
fn counts_that_do_not_match_the_outline_are_refused_before_any_write() {
    let store = open_store();
    let source = redacted(OUTLINED.as_bytes());
    let mut counts =
        OutlineCounts::measure("src/a.rs", &source, &byte_count, "cl100k_base").unwrap();
    counts.definitions.pop();
    let file = OutlineSource {
        path: "src/a.rs",
        source: &source,
        policy_version: 1,
        refuses_path: &|_| false,
    };
    assert!(block(put_file_outline(&store, &PlainCipher, &file, &counts)).is_err());
    assert_eq!(rows(&store, "summary_binding"), 0);
    assert_eq!(rows(&store, "summary_token_count"), 0);
}

#[test]
fn a_path_the_policy_denies_or_marks_sensitive_is_never_outlined() {
    let store = open_store();
    let refused = |path: &str| path.starts_with("secrets/");
    assert!(
        bind_outline_refusing(&store, "secrets/keys.rs", OUTLINED.as_bytes(), &refused).is_none()
    );
    assert_eq!(rows(&store, "summary_binding"), 0);
    assert_eq!(rows(&store, "summary_token_count"), 0);
    assert!(bind_outline_refusing(&store, "src/a.rs", OUTLINED.as_bytes(), &refused).is_some());
}

#[test]
fn a_file_the_redaction_gate_masked_binds_its_cleared_bytes_and_never_resolves_valid() {
    let store = open_store();
    let raw = format!("{}\nfn use_key() {{}}\n", key_line());
    let raw = raw.as_bytes();
    let cleared = redacted(raw);
    assert_ne!(cleared.as_slice(), raw, "the gate masks the key");
    let key = bind_outline(&store, "src/key.rs", raw).unwrap();
    let verdict = block(resolve(
        &store,
        &PlainCipher,
        &snapshot("src/key.rs", raw),
        &key,
    ))
    .unwrap();
    assert!(
        !matches!(verdict, Validity::Valid(_)),
        "a binding over masked bytes never stands for the checkout's bytes"
    );
}

proptest::proptest! {
    /// A file outside the outlined extensions never binds, whatever its
    /// bytes: no binding row and no count row.
    #[test]
    fn no_file_outside_the_outlined_extensions_binds(
        name in "[a-z]{1,8}\\.(toml|txt|json|yaml|lock|cfg)",
        source in proptest::collection::vec(0u8..255, 0..64),
    ) {
        let store = open_store();
        proptest::prop_assert!(bind_outline(&store, &name, &source).is_none());
        proptest::prop_assert_eq!(rows(&store, "summary_binding"), 0);
        proptest::prop_assert_eq!(rows(&store, "summary_token_count"), 0);
    }
}

/// A checkout holding `files`, and the batch that outlines out of it, with
/// `refused` paths turned away by policy.
fn batch_over<'a>(
    checkout: &'a Checkout,
    refuses: &'a dyn Fn(&str) -> bool,
    redact: &'a dyn Fn(Vec<u8>) -> Option<RedactedBytes>,
) -> OutlineBatch<'a> {
    OutlineBatch {
        work_tree: checkout.path(),
        policy_version: 1,
        refuses_path: refuses,
        redact,
        count: &byte_count,
        tokenizer: "cl100k_base",
    }
}

fn gate(bytes: Vec<u8>) -> Option<RedactedBytes> {
    Some(redacted(&bytes))
}

fn read_of(path: &str, bytes: &[u8]) -> (String, String) {
    (path.to_owned(), sha256_hex(bytes))
}

#[test]
fn a_batch_binds_what_the_session_read_and_counts_everything_else() {
    let store = open_store();
    let secret = key_line();
    let secret = secret.as_bytes();
    let checkout = Checkout::holding(&[
        ("src/a.rs", OUTLINED.as_bytes()),
        ("src/moved.rs", b"fn now() {}\n"),
        ("secrets/k.rs", OUTLINED.as_bytes()),
        ("README.md", b"# hi\n"),
        ("src/key.rs", secret),
    ]);
    let refuses = |path: &str| path.starts_with("secrets/");
    let batch = batch_over(&checkout, &refuses, &gate);
    let reads = [
        read_of("src/a.rs", OUTLINED.as_bytes()),
        read_of("src/a.rs", OUTLINED.as_bytes()),
        read_of("src/moved.rs", b"fn then() {}\n"),
        read_of("src/gone.rs", b"fn gone() {}\n"),
        read_of("secrets/k.rs", OUTLINED.as_bytes()),
        read_of("README.md", b"# hi\n"),
        read_of("src/key.rs", secret),
    ];
    let counts = block(bind_read_outlines(&store, &PlainCipher, &batch, &reads));
    assert_eq!(
        counts,
        OutlineBindCounts {
            bound: 2,
            changed: 2,
            refused: 2,
            ..OutlineBindCounts::default()
        }
    );
    assert_eq!(rows(&store, "summary_binding"), 2);
    let again = block(bind_read_outlines(&store, &PlainCipher, &batch, &reads));
    assert_eq!((again.bound, again.already_bound), (0, 2));
    assert_eq!(rows(&store, "summary_token_count"), 5);
}

#[test]
fn a_batch_stops_writing_at_its_cap_and_skips_oversize_files() {
    let store = open_store();
    let cap = usize::try_from(MAX_OUTLINES_PER_BATCH).unwrap();
    let files: Vec<(String, Vec<u8>)> = (0..=cap)
        .map(|at| {
            (
                format!("src/f{at:03}.rs"),
                format!("fn f{at}() {{}}\n").into_bytes(),
            )
        })
        .collect();
    let big = vec![b'x'; usize::try_from(MAX_OUTLINE_SOURCE_BYTES).unwrap() + 1];
    let mut holding: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .collect();
    holding.push(("src/big.rs", &big));
    let checkout = Checkout::holding(&holding);
    let reads: Vec<(String, String)> = holding
        .iter()
        .map(|(path, bytes)| read_of(path, bytes))
        .collect();
    let refuses = |_: &str| false;
    let batch = batch_over(&checkout, &refuses, &gate);
    let counts = block(bind_read_outlines(&store, &PlainCipher, &batch, &reads));
    assert_eq!(counts.bound, MAX_OUTLINES_PER_BATCH);
    assert_eq!(counts.deferred, 1);
    assert_eq!(counts.oversize, 1);
    let next = block(bind_read_outlines(&store, &PlainCipher, &batch, &reads));
    assert_eq!(
        (next.bound, next.already_bound),
        (1, MAX_OUTLINES_PER_BATCH)
    );
}

#[test]
fn a_failing_redaction_gate_is_counted_and_writes_nothing() {
    let store = open_store();
    let checkout = Checkout::holding(&[("src/a.rs", OUTLINED.as_bytes())]);
    let refuses = |_: &str| false;
    let refusing_gate = |_: Vec<u8>| None;
    let batch = batch_over(&checkout, &refuses, &refusing_gate);
    let reads = [read_of("src/a.rs", OUTLINED.as_bytes())];
    let counts = block(bind_read_outlines(&store, &PlainCipher, &batch, &reads));
    assert_eq!(counts.failed, 1);
    assert_eq!(rows(&store, "summary_binding"), 0);
}
