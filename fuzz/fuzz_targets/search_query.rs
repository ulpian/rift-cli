#![no_main]
//! Fuzz target for the two untrusted inputs of native search: the query an
//! agent types, and the index file it could write.
//!
//! Both are untrusted, for different reasons. A query is a string a model
//! composed, arriving through a hook payload or a verb's argv; the index is a
//! file in `.rift/state/`, which lives in a work tree that the agent being
//! served can write to. Neither may panic, hang, or allocate without bound —
//! the ranking runs where a hook is deciding, so a panic here is a tool call
//! that dies rather than one that is left alone.
//!
//! What is asserted beyond "no panic":
//!
//! * a ranked answer never invents a path — every hit is a path that was in
//!   the index — and never returns more than the limit, while the total it
//!   reports is at least what it returned;
//! * ranking is a function: the same query over the same index gives the same
//!   answer, which is the property every fixture and every pinned order
//!   downstream depends on;
//! * a decoded index round-trips to the same bytes, so a file that decoded
//!   cannot re-encode into a different one.

use libfuzzer_sys::fuzz_target;
use rift_recall_search::engine::MAX_QUERY_BYTES;
use rift_recall_search::{NativeEngine, PathIndex, PathQuery, SearchEngine};

/// The paths every query is ranked against: a small, fixed corpus with the
/// shapes a query can key on — nesting, extensions, dotfiles, a name that is a
/// prefix of another, and one that is not ASCII.
const PATHS: [&str; 8] = [
    "Cargo.toml",
    ".github/workflows/ci.yml",
    "crates/rift-canopy/src/layout.rs",
    "crates/rift-canopy/src/layout_grid.rs",
    "crates/rift-canopy-data/src/bus.rs",
    "docs/guide/02-configuration.md",
    "bins/rift/src/main.rs",
    "notes/日本語.md",
];

fuzz_target!(|data: &[u8]| {
    let Some((&selector, bytes)) = data.split_first() else {
        return;
    };
    if selector % 2 == 0 {
        // The index file: bytes an agent could have written into the work tree.
        if let Some(index) = PathIndex::decode(bytes) {
            assert_eq!(
                PathIndex::decode(&index.encode()),
                Some(index),
                "a decoded index re-encoded into a different one"
            );
        }
        return;
    }

    let bytes = &bytes[..bytes.len().min(MAX_QUERY_BYTES + 64)];
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };
    let index = PathIndex::new(
        rift_recall_search::IndexKey::default(),
        PATHS
            .iter()
            .enumerate()
            .map(|(i, path)| rift_recall_search::FileEntry {
                path: (*path).to_owned(),
                mtime_ms: i as u64,
            })
            .collect(),
    )
    .expect("a fixed eight-path index is under every cap");
    let engine = NativeEngine::new(index);
    let query = PathQuery {
        text: text.to_owned(),
        limit: 4,
        now_ms: 1_757_000_000_000,
    };

    let ranked = engine.rank_paths(&query);
    assert!(ranked.hits.len() <= query.limit, "over the limit");
    assert!(ranked.total >= ranked.hits.len(), "total below what it returned");
    for hit in &ranked.hits {
        assert!(
            PATHS.contains(&hit.path.as_str()),
            "a ranked answer named a path that was not in the index"
        );
    }
    assert_eq!(engine.rank_paths(&query), ranked, "ranking is not a function");
});
