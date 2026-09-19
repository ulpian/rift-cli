//! The `checkpoint` table's readers are **enumerated**, and adding one reddens
//! this file.
//!
//! # Why a source scan rather than a behavioural test
//!
//! `checkpoint.created_at` is not always a time: it carries an explicit
//! unknown-time sentinel whenever a shard records no time of its own, which is
//! every native shard. Measured 2026-08-18 across the 574 projections on the
//! author's machine — 336 rows sentinel, 364 rows a recorded time — so a reader
//! may neither assume it is always a time nor assume it never is. The
//! derivation that resolves it (recorded → the tightest lower bound derivable
//! from the sessions the shard archives → *unresolved*) **used to live in
//! `bins/rift/src/workflows.rs`, inside a bin-only package no crate could link
//! to. It does not any more.** It is
//! [`rift_store::window::CHECKPOINT_TIME_CTE`], and this crate now links it —
//! so the enumeration's job changed with it: it no longer records which sites
//! are stuck re-deriving the rule, it records **which arm each site takes and
//! whether it takes it through the shared derivation or around it**.
//!
//! That is also why there are now FOUR spellings to count, not two. A site that
//! reaches the table through the imported `CHECKPOINT_TIME_CTE` names neither
//! `"checkpoint"` nor `FROM checkpoint` in its own source, so the two original
//! spellings **cannot see it at all** — and a site that calls
//! `rift_store::window::gather` names none of those three, because the whole
//! checkpoint-anchored query cascade sits behind that one function. The fourth
//! spelling was added when `workflows.rs` arrived: it is the first site here
//! that reads the table without writing a line of SQL, and the guard as written
//! could not see it. **The list grew a column rather than a summary** —
//! collapsing four counts into "does this file touch the table" would answer
//! yes for every site and distinguish none of them.
//!
//! Said precisely, because the near-miss is the lesson: this particular change
//! *did* move the old counts — the reader it replaced spelled `"checkpoint"`,
//! so removing it reddened the enumeration anyway. That was a **coincidence of
//! the edit, not a property of the guard**. A CTE-based reader added beside the
//! existing ones, rather than in place of one, would have changed no counted
//! spelling and joined the crate silently — the guard's own stated blind spot,
//! arriving as a real instance one commit after it was written down.
//!
//! An enumeration nothing checks is a sentence, not a guard. So the list is
//! pinned here: a new reader of the `checkpoint` table
//! changes these counts and this test fails, and the only way to make it pass
//! is to join the enumeration and state which arm the new site takes.
//!
//! # What this covers, and what it does not
//!
//! It covers the table's **name** appearing in this crate's non-test source in
//! the two spellings the crate uses — a quoted table argument (`"checkpoint"`)
//! and an inline `FROM checkpoint` inside a SQL literal — **the identifier
//! `CHECKPOINT_TIME_CTE`, which reaches the table without naming it, and the
//! call `window::gather`, which reaches it without naming anything at all**. It
//! does **not** prove
//! anything about how a site treats the sentinel — that is each site's own
//! test — and it does not see a table name assembled at runtime from fragments.
//! Stated rather than implied: no such assembly exists in this crate today, and
//! this guard would not notice one.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

/// One enumerated site class: the file, and how many times each spelling of the
/// table name appears in its non-test source.
struct Enumerated {
    file: &'static str,
    /// `"checkpoint"` as a quoted argument — the `count` / `tally` / `total`
    /// callers.
    quoted: usize,
    /// `FROM checkpoint` inside a SQL literal.
    from_sql: usize,
    /// The imported `CHECKPOINT_TIME_CTE` — a reader that reaches the table
    /// **through the one shared derivation** and therefore never spells its
    /// name. Counted because the other two spellings cannot see it.
    via_cte: usize,
    /// A `window::gather` call — a reader that reaches the table through the
    /// **whole shared query cascade** and writes no SQL of its own, so it
    /// spells none of the three above. Counted for the same reason `via_cte`
    /// is: a spelling this guard cannot see is a site that joins silently.
    via_gather: usize,
    /// What the crate docs say about these sites; carried here so the two texts
    /// are read together when either changes.
    note: &'static str,
}

/// **The list is the deliverable.** Every touch of the `checkpoint` table in
/// this crate's non-test source, and what it does about the unknown-time
/// sentinel.
const SITES: &[Enumerated] = &[
    Enumerated {
        file: "bus.rs",
        quoted: 1,
        from_sql: 1,
        via_cte: 0,
        via_gather: 0,
        note: "count(\"checkpoint\") is a row count and is sentinel-blind by \
               construction (it never reads the time column). The SELECT fills \
               CaptureSummary::last_checkpoint_at_ms and is OPEN: on this \
               repository's projection it answers -1, a sentinel typed as an \
               epoch-millisecond instant. Nothing renders that field today, \
               which is the only reason a sentinel is not on screen.",
    },
    Enumerated {
        file: "feeds/activity.rs",
        quoted: 1,
        from_sql: 0,
        via_cte: 2,
        via_gather: 0,
        note: "the thirty-day strip's checkpoint bars are built through \
               CHECKPOINT_TIME_CTE (the import, and the format! that prefixes \
               it), so a shard that records no time of its own is placed by the \
               sessions it archives instead of being dropped. It was \
               tally(\"checkpoint\", \"created_at\", …) until 2026-08-19, which \
               bucketed the raw column and therefore placed the -1 sentinel in \
               1969 — on a repository of native shards the row was \
               STRUCTURALLY ZERO and the whole population became OffStrip. \
               CLOSED for the sentinel now. total(\"checkpoint\") is still the \
               row count behind OffStrip, and OffStrip stays OPEN for cause: it \
               cannot say whether a row is genuinely unresolvable or merely \
               older than the window.",
    },
    Enumerated {
        file: "viewer/catalogue.rs",
        quoted: 0,
        from_sql: 1,
        via_cte: 2,
        via_gather: 0,
        note: "the Recall Viewer catalogue imports and prefixes \
               CHECKPOINT_TIME_CTE, then treats its effective_at >= 0 as the \
               session-derived tier below a shard-recorded time. The FROM \
               occurrence is worth naming because it is the first of its kind \
               here: the catalogue also runs SELECT ... FROM \
               checkpoint_worktree, a DIFFERENT v14 relation that happens to \
               share the prefix this guard matches on. It reads no time \
               column at all -- checkpoint_id and worktree_name -- so the \
               sentinel question does not arise for it, and its own absence \
               arm is worktree provenance's: a checkpoint with no row badges unknown, \
               never main. N/A for the sentinel. Recorded rather than \
               exempted, because a guard that starts skipping prefix matches \
               stops seeing the readers it was built for.",
    },
    Enumerated {
        file: "viewer/detail.rs",
        quoted: 1,
        from_sql: 0,
        via_cte: 0,
        via_gather: 0,
        note: "the checkpoint summary card's quoted checkpoint occurrence is a \
               display-fact label, not a table reader; it is counted because \
               this lexical guard deliberately does not parse Rust syntax. The \
               card reads no store at all: its time comes from the catalogue \
               entry, which the catalogue module already resolved through the \
               CTE. N/A for the sentinel.",
    },
    Enumerated {
        file: "workflows.rs",
        quoted: 0,
        from_sql: 0,
        via_cte: 0,
        via_gather: 1,
        note: "the dispatch/recap cards call rift_store::window::gather, which \
               runs the whole checkpoint-anchored cascade behind \
               CHECKPOINT_TIME_CTE. It takes the SAME arm the verbs take, \
               because it is literally the same function: a recorded time \
               wins; a shard without one is placed at MAX(session.started_at) \
               over the sessions it archives and reported as DERIVED, which \
               the card labels rather than passing off as a record; a shard \
               with neither is unresolved -- excluded from bounded windows, \
               counted repository-wide, and rendered as its own figure so \
               'excluded' never reads as 'absent'. The card gathers the \
               UNBOUNDED window, which is the one window an unresolved \
               checkpoint belongs to, so the exclusion arm is unreachable from \
               here today and the derived arm is the one on screen. CLOSED for \
               the sentinel. This site is why the enumeration has a fourth \
               spelling at all: it reads the table without naming it, without \
               naming the CTE, and without writing SQL.",
    },
];

/// This crate's `src` directory.
fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every `.rs` file under `src`, recursively.
fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// A file's **non-test** source with line comments removed.
///
/// Truncated at the first `#[cfg(test)]` because a test module legitimately
/// names the table to seed it, and a seed is not a reader. Comments are
/// stripped because this file and the crate docs both name the table in prose.
fn scannable(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap();
    let body = text.split("#[cfg(test)]").next().unwrap_or("").to_owned();
    body.lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Non-overlapping occurrences of `needle`.
fn occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn every_reader_of_the_checkpoint_table_is_on_the_enumerated_list() {
    let root = src_root();
    let mut files = Vec::new();
    rs_files(&root, &mut files);
    assert!(!files.is_empty(), "the scan found no source to read");

    let mut unexpected = Vec::new();
    for path in &files {
        // The src-relative path, not the bare file name: a card of the data
        // plane is one file of a directory module, and two directories may
        // hold a reader of the same name.
        let name = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let body = scannable(path);
        let quoted = occurrences(&body, "\"checkpoint\"");
        let from_sql = occurrences(&body, "FROM checkpoint");
        let via_cte = occurrences(&body, "CHECKPOINT_TIME_CTE");
        let via_gather = occurrences(&body, "window::gather");
        let listed = SITES.iter().find(|s| s.file == name);
        match listed {
            Some(site) => {
                assert_eq!(
                    (quoted, from_sql, via_cte, via_gather),
                    (site.quoted, site.from_sql, site.via_cte, site.via_gather),
                    "{name} touches the checkpoint table a different number of \
                     times than the enumeration records. The list is the \
                     deliverable, not the count: add the new site to SITES here \
                     AND to the crate docs, saying which arm it takes for a row \
                     whose time is the unknown sentinel.\n  enumerated: {}",
                    site.note
                );
            }
            None => {
                if quoted > 0 || from_sql > 0 || via_cte > 0 || via_gather > 0 {
                    unexpected.push(format!(
                        "{name}: {quoted} quoted, {from_sql} FROM, {via_cte} via CTE, \
                         {via_gather} via gather"
                    ));
                }
            }
        }
    }
    assert!(
        unexpected.is_empty(),
        "a file outside the enumeration reads the checkpoint table: {unexpected:?}. \
         Every such site must join SITES and the crate docs before it lands — \
         `checkpoint.created_at` is not always a time."
    );
}

#[test]
fn the_enumeration_names_files_that_exist() {
    // The other direction: a site that is deleted must not leave a stale entry
    // asserting a file into existence.
    for site in SITES {
        let path = src_root().join(site.file);
        assert!(
            path.exists(),
            "the enumeration names {} but this crate no longer has it",
            site.file
        );
    }
}
