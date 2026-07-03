//! Meta-check: the store never interpolates a runtime *value* into SQL — all
//! value binding goes through `?` placeholders and the engine's parameter API
//! (`01-turso-integration` §security, the `all_queries_are_parameterized` gate;
//! `05-security` §5.7). This is a hermetic source scan over `crates/rift-store/src`
//! — no engine, no network — so a regression that builds a query by string-
//! interpolating a value fails CI.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::path::{Path, PathBuf};

/// SQL value-context tokens: an interpolation next to one of these is the smell we
/// forbid (e.g. `format!("... WHERE id = '{id}'")` or `VALUES ('{v}')`).
const VALUE_CONTEXTS: [&str; 4] = ["VALUES", "WHERE", "SET ", " = '"];

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_value_is_interpolated_into_sql() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&src, &mut files);
    assert!(!files.is_empty(), "found no source files to scan");

    let mut offenders = Vec::new();
    for file in &files {
        // The migration engine seeds `_schema_version` with the table *name* (a
        // validated `[A-Za-z0-9_]` registry constant, not runtime data) via a
        // reviewed identifier interpolation — exempt by filename.
        let is_migrate = file.file_name().is_some_and(|n| n == "migrate.rs");
        let text = std::fs::read_to_string(file).expect("read source");
        for (i, line) in text.lines().enumerate() {
            // The smell: a single-quoted interpolation (`'{`) inside a SQL value
            // context. `?`-placeholder binding never produces `'{`.
            let interpolated_quote = line.contains("'{");
            let in_value_ctx = VALUE_CONTEXTS.iter().any(|t| line.contains(t));
            if interpolated_quote && in_value_ctx && !is_migrate {
                offenders.push(format!("{}:{}", file.display(), i + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "value interpolated into SQL (use `?` placeholders): {offenders:?}"
    );
}
