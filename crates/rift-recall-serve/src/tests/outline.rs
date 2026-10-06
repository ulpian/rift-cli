//! The outline writer, exercised where the redaction gate can mint the bytes
//! it takes: an outline is only ever bound from redacted bytes, and only for a
//! code path the serve policy serves.

use rift_core::{RedactedBytes, SecretBytes};
use rift_recall_ledger::validity_store::{
    put_file_outline, resolve, token_counts, OutlineCounts, OutlineSource,
};
use rift_recall_substrate::worktree::sha256_hex;
use rift_recall_validity::{FileOutline, Validity, WorktreeSnapshot};
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
    let counts = FileOutline::of(path, source.as_slice()).map_or_else(
        || OutlineCounts {
            tokenizer: "cl100k_base",
            whole_file: 1,
            definitions: Vec::new(),
        },
        |outline| {
            let text = std::str::from_utf8(source.as_slice()).unwrap();
            OutlineCounts::measure(&outline, text, &byte_count, "cl100k_base")
        },
    );
    let file = OutlineSource {
        path,
        source: &source,
        policy_version: 1,
        redaction_version: 1,
        refuses_path,
    };
    block(put_file_outline(store, &PlainCipher, &file, &counts)).unwrap()
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
    let outline = FileOutline::of("src/a.rs", source.as_slice()).unwrap();
    let mut counts = OutlineCounts::measure(&outline, OUTLINED, &byte_count, "cl100k_base");
    counts.definitions.pop();
    let file = OutlineSource {
        path: "src/a.rs",
        source: &source,
        policy_version: 1,
        redaction_version: 1,
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
    let raw = b"const KEY: &str = \"AKIAZ7Q2XK4LMNOP3RST\";\n\nfn use_key() {}\n";
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
    /// A file outside the code extensions never binds, whatever its bytes:
    /// no binding row and no count row.
    #[test]
    fn no_file_outside_the_code_extensions_binds(
        name in "[a-z]{1,8}\\.(md|toml|txt|json|yaml|lock|cfg)",
        source in proptest::collection::vec(0u8..255, 0..64),
    ) {
        let store = open_store();
        proptest::prop_assert!(bind_outline(&store, &name, &source).is_none());
        proptest::prop_assert_eq!(rows(&store, "summary_binding"), 0);
        proptest::prop_assert_eq!(rows(&store, "summary_token_count"), 0);
    }
}
