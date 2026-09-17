//! The seam, and the engine behind it.
//!
//! A search is three questions asked in order, and the order is the whole
//! design:
//!
//! 1. **Which paths is this query even about?** The query grammar's
//!    constraints — an extension, a glob, a path segment, an exclusion, a git
//!    status — filter the index. A constraint the engine cannot interpret
//!    filters nothing, because a query nobody understood must not silently
//!    return an empty answer.
//! 2. **How well does each remaining path match the letters typed?** That is
//!    the fuzzy matcher's job and only its job: a Smith-Waterman score over the
//!    repository-relative path.
//! 3. **Which of the good matches does this developer actually work on?** That
//!    is Rift's, and it is the reason this crate exists rather than a call to a
//!    fuzzy finder. Frecency across the worktree family, the file's own age,
//!    and what git says is in flight right now are added to the match score.
//!
//! The weights are chosen so that a materially better name match always wins —
//! one more matched character is worth more than every warmth bonus put
//! together — while among comparable names the file the family has been living
//! in comes first.
//!
//! **Determinism.** Same index, same facts, same query, same order, on every
//! run: the score is integer arithmetic over inputs the caller supplies (the
//! clock included, as `now_ms`), the candidate list is the index's own sorted
//! order, and ties break on the path. Nothing here reads a clock, a store or
//! the network, and the ranking is a pure function of its arguments.

use fff_query_parser::{FFFQuery, FileSearchConfig, FuzzyQuery};
use neo_frizbee::{Config as FuzzyConfig, Match};

use crate::content::{self, ContentHit, ContentQuery};
use crate::frecency::FrecencyFacts;
use crate::git::StatusFacts;
use crate::index::{FileEntry, PathIndex};
use crate::query;

/// The longest query this engine will look at.
///
/// A query is something a person or a model typed; past this it is a payload,
/// and the answer to a payload is no answer rather than a walk of the index
/// with a kilobyte of pattern.
pub const MAX_QUERY_BYTES: usize = 1_024;

/// What one matched character of a path is worth.
///
/// The fuzzy matcher scores a character at 12 before its own bonuses, so a
/// weight of 16 puts one extra matched character (192) above the sum of every
/// warmth bonus this engine can add (at most 180). Name match decides; warmth
/// orders the paths that match comparably well.
const FUZZY_WEIGHT: u64 = 16;

/// Bonus for a path whose name the query matched exactly.
const EXACT_BONUS: u64 = 96;

/// What one recorded access is worth, and how many of them count.
const ACCESS_POINTS: u64 = 4;
/// The point past which more accesses say nothing new.
const ACCESS_CAP: u64 = 12;

/// How recently an agent last touched a file, and what that is worth.
///
/// Buckets rather than a curve: a bucket is an integer comparison, it cannot
/// disagree with itself across two runs a millisecond apart, and the claim it
/// encodes ("this hour", "today", "this week") is the only claim the underlying
/// facts actually support.
const RECENCY_BUCKETS: [(u64, u64); 5] = [
    (5 * 60_000, 60),
    (60 * 60_000, 45),
    (24 * 60 * 60_000, 30),
    (7 * 24 * 60 * 60_000, 15),
    (30 * 24 * 60 * 60_000, 5),
];

/// How much less a file's own age counts than an agent having opened it.
///
/// A file is modified by tools, generators and merges; a read ledger row is
/// somebody deciding to look at it. Both are recency, and only one of them is
/// evidence of interest.
const MTIME_RECENCY_DIVISOR: u64 = 3;

/// What git status is worth.
///
/// The file the developer has open right now is the one they are asking about,
/// but the signal is a tie-breaker: an untracked scratch file must not outrank
/// the source file whose name was actually typed.
const CHANGED_BONUS: u64 = 30;
/// An untracked file is in flight too, but it is also every stray download.
const UNTRACKED_BONUS: u64 = 10;

/// One request for ranked paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathQuery {
    /// The query as typed, in the finder's grammar.
    pub text: String,
    /// How many hits to return. The total is reported separately, so a caller
    /// can say "8 of 214" without holding 214 paths.
    pub limit: usize,
    /// The instant recency is measured from, supplied rather than read, so a
    /// ranking is reproducible and a test never freezes a clock.
    pub now_ms: u64,
}

/// One ranked path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathHit {
    /// The repository-relative path.
    pub path: String,
    /// Its total score. Comparable within one answer and meaningless across
    /// two: it is a sum of weights, not a probability.
    pub score: u64,
}

/// A ranked answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ranked {
    /// The best `limit` paths, best first.
    pub hits: Vec<PathHit>,
    /// How many paths matched in total, before the limit was applied. This is
    /// the number a listing answer quotes, so it is counted rather than
    /// estimated.
    pub total: usize,
}

/// What a caller may ask a search engine.
///
/// A trait because the engine is a seam: the callers — a verb, a hook arm, the
/// detached worker — are written against this and never against the crates
/// behind it, which is what keeps the fuzzy matcher, the query grammar and the
/// content matcher replaceable without touching a caller.
pub trait SearchEngine {
    /// Rank the paths this checkout holds against a query.
    fn rank_paths(&self, query: &PathQuery) -> Ranked;

    /// The lines matching a literal, in files of `root` this engine may open.
    ///
    /// Separate from ranking because it is a different cost and a different
    /// risk: ranking touches no file content at all, and this one reads bytes
    /// an agent may later be shown.
    fn content_lines(&self, root: &std::path::Path, query: &ContentQuery) -> Vec<ContentHit>;
}

/// The engine over the index, the family's frecency facts and git status.
#[derive(Clone, Debug)]
pub struct NativeEngine {
    index: PathIndex,
    frecency: FrecencyFacts,
    status: StatusFacts,
}

impl NativeEngine {
    /// An engine over an index alone: no history, no status.
    ///
    /// This is the cold shape, and it is deliberately usable: a caller with no
    /// budget to collect facts still gets fuzzy ranking over the right paths.
    #[must_use]
    pub fn new(index: PathIndex) -> Self {
        Self {
            index,
            frecency: FrecencyFacts::new(),
            status: StatusFacts::default(),
        }
    }

    /// Rank with what the family has read.
    #[must_use]
    pub fn with_frecency(mut self, frecency: FrecencyFacts) -> Self {
        self.frecency = frecency;
        self
    }

    /// Rank with what is in flight in this checkout.
    #[must_use]
    pub fn with_status(mut self, status: StatusFacts) -> Self {
        self.status = status;
        self
    }

    /// The index this engine answers from.
    #[must_use]
    pub fn index(&self) -> &PathIndex {
        &self.index
    }

    /// What git says about this checkout, as this engine was told it.
    #[must_use]
    pub fn status(&self) -> &StatusFacts {
        &self.status
    }

    /// The warmth part of a path's score.
    fn warmth(&self, entry: &FileEntry, now_ms: u64) -> u64 {
        let facts = self.frecency.get(&entry.path);
        let accesses = u64::from(facts.accesses).min(ACCESS_CAP) * ACCESS_POINTS;
        let read = recency_points(now_ms, facts.last_at_ms);
        let touched = recency_points(now_ms, entry.mtime_ms) / MTIME_RECENCY_DIVISOR;
        let status = if self.status.is_changed(&entry.path) {
            CHANGED_BONUS
        } else if self.status.is_untracked(&entry.path) {
            UNTRACKED_BONUS
        } else {
            0
        };
        accesses + read + touched + status
    }

    /// The paths a query's constraints leave in play.
    fn candidates<'a>(&'a self, parsed: &FFFQuery<'_>) -> Vec<&'a FileEntry> {
        self.index
            .entries()
            .iter()
            .filter(|entry| query::keeps(&parsed.constraints, &entry.path, &self.status))
            .collect()
    }
}

impl SearchEngine for NativeEngine {
    fn rank_paths(&self, query: &PathQuery) -> Ranked {
        if query.text.len() > MAX_QUERY_BYTES {
            return Ranked::default();
        }
        let parsed = FFFQuery::parse(&query.text, FileSearchConfig);
        let candidates = self.candidates(&parsed);
        let haystacks: Vec<&str> = candidates.iter().map(|entry| entry.path.as_str()).collect();
        let matched = fuzzy(&parsed.fuzzy_query, &haystacks);

        let mut hits: Vec<PathHit> = matched
            .into_iter()
            .filter_map(|(position, score, exact)| {
                let entry = *candidates.get(position)?;
                let mut total = u64::from(score) * FUZZY_WEIGHT + self.warmth(entry, query.now_ms);
                if exact {
                    total += EXACT_BONUS;
                }
                Some(PathHit {
                    path: entry.path.clone(),
                    score: total,
                })
            })
            .collect();
        // Highest first, and by path where two paths scored the same: the
        // index is sorted, so this is a total order over the answer and two
        // runs cannot disagree about it.
        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
        let total = hits.len();
        hits.truncate(query.limit);
        Ranked { hits, total }
    }

    fn content_lines(&self, root: &std::path::Path, query: &ContentQuery) -> Vec<ContentHit> {
        // Two filters, both here rather than at the caller. The index is what
        // says a path is neither ignored nor sensitive — a path that is not in
        // it was never searchable, however it reached this call — and git is
        // what says it is tracked, so a stray file in the work tree cannot put
        // its contents in front of a model.
        let searchable = ContentQuery {
            paths: query
                .paths
                .iter()
                .filter(|path| self.index.contains(path) && !self.status.is_untracked(path))
                .cloned()
                .collect(),
            ..query.clone()
        };
        content::search(root, &searchable)
    }
}

/// The fuzzy half: which candidates matched the typed letters, and how well.
///
/// A multi-part query is an intersection — every part has to match — with the
/// scores added, which is what makes `canopy layout` narrower than either word
/// and not merely the union of two rankings.
fn fuzzy(query: &FuzzyQuery<'_>, haystacks: &[&str]) -> Vec<(usize, u32, bool)> {
    let config = FuzzyConfig {
        // Sorting is this engine's job, and it is not by this score.
        sort: false,
        ..FuzzyConfig::default()
    };
    match query {
        // Constraints only ("*.rs", "/src/"): everything that survived the
        // filter is an equally good answer, and warmth orders it.
        FuzzyQuery::Empty => (0..haystacks.len()).map(|i| (i, 0, false)).collect(),
        FuzzyQuery::Text(needle) => neo_frizbee::match_list(needle, haystacks, &config)
            .into_iter()
            .map(|m| (m.index as usize, u32::from(m.score), m.exact))
            .collect(),
        FuzzyQuery::Parts(parts) => {
            let mut running: Option<Vec<(usize, u32, bool)>> = None;
            for part in parts {
                let matches: Vec<Match> = neo_frizbee::match_list(part, haystacks, &config);
                running = Some(match running {
                    None => matches
                        .into_iter()
                        .map(|m| (m.index as usize, u32::from(m.score), m.exact))
                        .collect(),
                    Some(previous) => intersect(&previous, &matches),
                });
            }
            running.unwrap_or_default()
        }
    }
}

/// Keep only the candidates both parts matched, adding their scores.
fn intersect(previous: &[(usize, u32, bool)], matches: &[Match]) -> Vec<(usize, u32, bool)> {
    previous
        .iter()
        .filter_map(|(position, score, exact)| {
            let found = matches.iter().find(|m| m.index as usize == *position)?;
            Some((
                *position,
                score + u32::from(found.score),
                *exact && found.exact,
            ))
        })
        .collect()
}

/// The bucket a moment falls into, in points.
///
/// A fact with no time at all — a projection count, a file whose modification
/// time the filesystem would not give — scores nothing rather than scoring as
/// though it had just happened.
fn recency_points(now_ms: u64, at_ms: u64) -> u64 {
    if at_ms == 0 {
        return 0;
    }
    let age = now_ms.saturating_sub(at_ms);
    RECENCY_BUCKETS
        .iter()
        .find_map(|(window, points)| (age <= *window).then_some(*points))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::IndexKey;
    use std::path::Path;

    const NOW_MS: u64 = 1_757_000_000_000;

    fn index(paths: &[(&str, u64)]) -> PathIndex {
        PathIndex::new(
            IndexKey {
                head_oid: "a".repeat(40),
                newest_dir_mtime_ms: 1,
            },
            paths
                .iter()
                .map(|(path, mtime_ms)| FileEntry {
                    path: (*path).to_owned(),
                    mtime_ms: *mtime_ms,
                })
                .collect(),
        )
        .unwrap()
    }

    fn query(text: &str, limit: usize) -> PathQuery {
        PathQuery {
            text: text.to_owned(),
            limit,
            now_ms: NOW_MS,
        }
    }

    fn paths(ranked: &Ranked) -> Vec<&str> {
        ranked.hits.iter().map(|hit| hit.path.as_str()).collect()
    }

    fn corpus() -> PathIndex {
        index(&[
            ("crates/rift-canopy/src/layout.rs", NOW_MS - 90_000),
            ("crates/rift-canopy/src/lib.rs", NOW_MS - 400_000),
            ("crates/rift-canopy-data/src/bus.rs", NOW_MS - 5_000),
            ("docs/guide/03-layout.md", NOW_MS - 900_000_000),
            ("bins/rift/src/main.rs", NOW_MS - 900_000_000),
            ("target-notes/layout.txt", NOW_MS - 900_000_000),
        ])
    }

    fn warmed() -> FrecencyFacts {
        let mut facts = FrecencyFacts::new();
        for _ in 0..4 {
            facts.record("crates/rift-canopy/src/layout.rs", NOW_MS - 120_000);
        }
        facts.record("docs/guide/03-layout.md", NOW_MS - 20 * 24 * 60 * 60_000);
        facts
    }

    /// One index, one set of facts, one query, one order — every time, and
    /// whatever order the walk happened to hand the paths over in.
    ///
    /// The order is what a caller pins to: an answer that reshuffled between
    /// two identical calls would make "the top eight" a different eight for
    /// the agent that repeated its question, and would make every fixture that
    /// measures this lever unrepeatable.
    #[test]
    fn ranking_is_deterministic_for_a_fixed_index_and_query() {
        let engine = NativeEngine::new(corpus()).with_frecency(warmed());
        let first = engine.rank_paths(&query("layout", 8));
        assert_eq!(
            paths(&first),
            vec![
                "crates/rift-canopy/src/layout.rs",
                "docs/guide/03-layout.md",
                "target-notes/layout.txt",
            ],
            "the pinned order"
        );
        assert_eq!(first.total, 3);

        for _ in 0..8 {
            assert_eq!(engine.rank_paths(&query("layout", 8)), first);
        }

        // The same paths offered to the index in a different order are the
        // same index, so they are the same answer.
        let reversed = {
            let mut entries: Vec<FileEntry> = corpus().entries().to_vec();
            entries.reverse();
            PathIndex::new(corpus().key().clone(), entries).unwrap()
        };
        assert_eq!(
            NativeEngine::new(reversed)
                .with_frecency(warmed())
                .rank_paths(&query("layout", 8)),
            first
        );
    }

    /// The compounding claim, on disk: a file read in a *sibling* worktree of
    /// the same family ranks warmer in this one.
    ///
    /// Both candidates match the query identically — same file name, same
    /// depth, same length — so the fuzzy score cannot be what separates them,
    /// and the one that wins is the one the tie-break would otherwise lose.
    /// Take the sibling away and the order flips back, which is what makes
    /// this a test of the family rather than of the sort.
    #[test]
    fn a_file_read_in_a_sibling_worktree_ranks_warmer_here() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().join("main");
        let sibling = dir.path().join("feature");
        let warm = "crates/two/src/bus.rs";
        let cold = "crates/one/src/bus.rs";

        let rows = std::fs::create_dir_all(crate::reads_dir(&sibling)).map(|()| {
            let mut body = String::new();
            for at_ms in [NOW_MS - 60_000, NOW_MS - 50_000] {
                body.push_str(&read_row(warm, at_ms));
                body.push('\n');
            }
            body
        });
        std::fs::write(crate::reads_dir(&sibling).join("bbbb.jsonl"), rows.unwrap()).unwrap();
        std::fs::create_dir_all(crate::reads_dir(&here)).unwrap();

        let corpus = index(&[(cold, NOW_MS - 900_000_000), (warm, NOW_MS - 900_000_000)]);
        let members: Vec<&Path> = vec![here.as_path(), sibling.as_path()];

        let with_family = NativeEngine::new(corpus.clone())
            .with_frecency(FrecencyFacts::from_members(&members))
            .rank_paths(&query("bus.rs", 8));
        assert_eq!(
            paths(&with_family),
            vec![warm, cold],
            "the sibling's reads did not warm this checkout"
        );

        let alone = NativeEngine::new(corpus)
            .with_frecency(FrecencyFacts::from_members(&[here.as_path()]))
            .rank_paths(&query("bus.rs", 8));
        assert_eq!(
            paths(&alone),
            vec![cold, warm],
            "without the sibling the tie breaks on the path"
        );
        assert_eq!(
            alone.hits[0].score, alone.hits[1].score,
            "the two candidates must match the query equally, or this test \
             would be measuring the fuzzy score rather than the family"
        );
    }

    fn read_row(path: &str, at_ms: u64) -> String {
        rift_agent::read_ledger::Line::Read {
            turn: 1,
            path: path.to_owned(),
            hash: rift_core::Sha256Id::from_bytes([3; 32]),
            range: rift_agent::read_ledger::Interval::new(1, 20).unwrap(),
            total_lines: 20,
            at_ms,
        }
        .to_json_line()
    }

    /// A better name match beats a warmer file: warmth orders comparable
    /// matches, it does not overrule the letters the model typed.
    #[test]
    fn a_better_name_match_outranks_a_warmer_file() {
        let mut facts = FrecencyFacts::new();
        for _ in 0..40 {
            facts.record("crates/rift-canopy-data/src/bus.rs", NOW_MS - 1_000);
        }
        let engine = NativeEngine::new(corpus()).with_frecency(facts);
        assert_eq!(
            paths(&engine.rank_paths(&query("layout.rs", 3))).first(),
            Some(&"crates/rift-canopy/src/layout.rs")
        );
    }

    /// Git status is a tie-break the engine only has if a caller collected it,
    /// and every git-status constraint is answered from the same facts.
    #[test]
    fn what_is_in_flight_ranks_above_what_is_not() {
        let corpus = index(&[
            ("src/one_alpha.rs", NOW_MS - 900_000_000),
            ("src/two_alpha.rs", NOW_MS - 900_000_000),
        ]);
        let cold = NativeEngine::new(corpus.clone()).rank_paths(&query("alpha", 8));
        assert_eq!(
            paths(&cold),
            vec!["src/one_alpha.rs", "src/two_alpha.rs"],
            "the two candidates match the query equally"
        );
        assert_eq!(cold.hits[0].score, cold.hits[1].score);

        let status = StatusFacts {
            modified: std::collections::BTreeSet::from(["src/two_alpha.rs".to_owned()]),
            ..StatusFacts::default()
        };
        let engine = NativeEngine::new(corpus.clone()).with_status(status);
        assert_eq!(
            paths(&engine.rank_paths(&query("alpha", 8))),
            vec!["src/two_alpha.rs", "src/one_alpha.rs"]
        );
        assert_eq!(
            paths(&engine.rank_paths(&query("alpha status:modified", 8))),
            vec!["src/two_alpha.rs"]
        );
        assert_eq!(
            paths(&NativeEngine::new(corpus).rank_paths(&query("alpha status:modified", 8))),
            Vec::<&str>::new(),
            "a status constraint with no status facts matches nothing in flight"
        );
    }

    /// The limit bounds what is returned; the total counts what matched, since
    /// that is the number an answer quotes back to the agent.
    #[test]
    fn the_limit_bounds_the_answer_and_the_total_counts_the_matches() {
        let engine = NativeEngine::new(corpus());
        let ranked = engine.rank_paths(&query("layout", 1));
        assert_eq!(ranked.hits.len(), 1);
        assert_eq!(ranked.total, 3);
        assert_eq!(engine.rank_paths(&query("layout", 0)).hits.len(), 0);
    }

    /// A query past the cap is a payload, not a question, and is answered with
    /// nothing rather than with a walk of the index.
    #[test]
    fn an_oversized_query_is_not_answered() {
        let engine = NativeEngine::new(corpus());
        let ranked = engine.rank_paths(&query(&"a".repeat(MAX_QUERY_BYTES + 1), 8));
        assert_eq!(ranked, Ranked::default());
    }

    /// Content search opens only what the index holds and git tracks. Both
    /// filters live here, not at the caller: a path that is not in the index
    /// was never searchable however it reached this call, and an untracked
    /// file is a stray in the work tree rather than part of the repository.
    #[test]
    fn content_search_opens_only_indexed_tracked_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        for name in ["src/tracked.rs", "src/untracked.rs", "src/ignored.rs"] {
            std::fs::write(root.join(name), "fn secret_token() {}\n").unwrap();
        }
        // The index holds two of the three: the walk never offered the third.
        let corpus = index(&[("src/tracked.rs", 0), ("src/untracked.rs", 0)]);
        let engine = NativeEngine::new(corpus).with_status(StatusFacts {
            untracked: std::collections::BTreeSet::from(["src/untracked.rs".to_owned()]),
            ..StatusFacts::default()
        });
        let asked = crate::content::ContentQuery {
            needle: "secret_token".to_owned(),
            paths: vec![
                "src/tracked.rs".to_owned(),
                "src/untracked.rs".to_owned(),
                "src/ignored.rs".to_owned(),
            ],
            ..crate::content::ContentQuery::default()
        };
        let hits = engine.content_lines(root, &asked);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "src/tracked.rs");
    }

    /// A query that is nothing but constraints has no letters to score, so
    /// every path the filter left is an equally good answer and warmth is the
    /// whole order.
    ///
    /// This is the shape `*.rs` and `!docs` arrive in, and the alternative —
    /// treating "no fuzzy query" as "nothing matched" — would answer a legal
    /// query with an empty list.
    #[test]
    fn a_query_of_constraints_alone_is_ordered_by_warmth() {
        let engine = NativeEngine::new(corpus()).with_frecency(warmed());
        let ranked = engine.rank_paths(&query("*.rs", 8));
        assert_eq!(
            paths(&ranked),
            vec![
                "crates/rift-canopy/src/layout.rs",
                "crates/rift-canopy-data/src/bus.rs",
                "crates/rift-canopy/src/lib.rs",
                "bins/rift/src/main.rs",
            ],
            "every Rust file, warmest first"
        );
        assert_eq!(ranked.total, 4);
        // The engine hands back the facts it was built with, which is how a
        // caller renders "(modified)" beside a path without collecting status
        // a second time.
        assert_eq!(engine.index().entries().len(), 6);
        assert!(engine.index().contains("docs/guide/03-layout.md"));
        assert_eq!(engine.status(), &StatusFacts::default());
    }

    /// A multi-word query is an intersection, not a union: every word has to
    /// match, and the scores add.
    ///
    /// That is what makes `canopy layout` a narrower question than either word
    /// alone rather than a longer answer than both.
    #[test]
    fn a_multi_word_query_intersects_its_parts_and_adds_their_scores() {
        let engine = NativeEngine::new(index(&[
            ("crates/rift-canopy/src/layout.rs", 0),
            ("crates/rift-canopy/src/theme.rs", 0),
            ("docs/layout.md", 0),
        ]));
        let both = engine.rank_paths(&query("canopy layout", 8));
        assert_eq!(
            paths(&both),
            vec!["crates/rift-canopy/src/layout.rs"],
            "only the path both words matched"
        );

        let one_word = engine.rank_paths(&query("canopy", 8));
        assert_eq!(
            paths(&one_word),
            vec![
                "crates/rift-canopy/src/layout.rs",
                "crates/rift-canopy/src/theme.rs",
            ],
            "one word alone is the wider question"
        );
        assert!(
            both.hits[0].score > one_word.hits[0].score,
            "the parts' scores add: {} is not more than {}",
            both.hits[0].score,
            one_word.hits[0].score
        );
        assert!(both.hits[0].score > engine.rank_paths(&query("layout", 8)).hits[0].score);
        assert_eq!(
            engine.rank_paths(&query("canopy nowhere", 8)),
            Ranked::default(),
            "a word nothing matched empties the intersection"
        );
    }

    /// A path the query named outright carries the exact bonus on top of its
    /// match, because a model that typed a file name has told the engine which
    /// file it means.
    #[test]
    fn a_path_the_query_names_outright_takes_the_exact_bonus() {
        let engine = NativeEngine::new(index(&[("bus.rs", 0), ("crates/one/src/bus.rs", 0)]));
        let ranked = engine.rank_paths(&query("bus.rs", 8));
        assert_eq!(
            paths(&ranked),
            vec!["bus.rs", "crates/one/src/bus.rs"],
            "the path that is the query comes first"
        );
        assert!(
            ranked.hits[0].score >= ranked.hits[1].score + EXACT_BONUS,
            "the exact match is not carrying its bonus: {} against {}",
            ranked.hits[0].score,
            ranked.hits[1].score
        );
    }

    /// An untracked file is in flight too — but it is also every stray
    /// download, so it is worth less than a file git is tracking a change to.
    #[test]
    fn an_untracked_file_is_warm_and_still_ranks_under_a_changed_one() {
        let corpus = index(&[
            ("src/one_alpha.rs", 0),
            ("src/two_alpha.rs", 0),
            ("src/three_alpha.rs", 0),
        ]);
        let status = StatusFacts {
            modified: std::collections::BTreeSet::from(["src/three_alpha.rs".to_owned()]),
            untracked: std::collections::BTreeSet::from(["src/two_alpha.rs".to_owned()]),
            ..StatusFacts::default()
        };
        let engine = NativeEngine::new(corpus).with_status(status);
        let ranked = engine.rank_paths(&query("alpha", 8));
        assert_eq!(
            paths(&ranked),
            vec!["src/three_alpha.rs", "src/two_alpha.rs", "src/one_alpha.rs",],
            "changed, then untracked, then the file nothing is happening to"
        );
        assert_eq!(
            ranked.hits[1].score - ranked.hits[2].score,
            UNTRACKED_BONUS,
            "an untracked file is warmer by exactly the untracked bonus"
        );
        assert!(engine.status().is_untracked("src/two_alpha.rs"));
    }

    fn shuffled(mut entries: Vec<FileEntry>, seed: u64) -> Vec<FileEntry> {
        // Fisher-Yates over a small deterministic generator: proptest supplies
        // the seed, so a walk order that broke the ranking is reproducible
        // from the case proptest prints rather than from a lucky rerun.
        let mut state = seed | 1;
        for i in (1..entries.len()).rev() {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let j = usize::try_from(state >> 33).unwrap_or(0) % (i + 1);
            entries.swap(i, j);
        }
        entries
    }

    proptest::proptest! {
        /// ★ The order a walk happened to hand the paths over in is not part
        /// of the answer.
        ///
        /// The index sorts what it is given, the candidate list is the index's
        /// own order, and ties break on the path — so the same paths, the same
        /// facts and the same query are the same ranking whatever order the
        /// filesystem, a rebuild or a merge of two walks produced. This is the
        /// claim every fixture that measures this lever rests on: an answer
        /// that reshuffled between two identical calls would make "the top
        /// eight" a different eight for the agent that repeated its question.
        ///
        /// The claim is carried twice over — the index sorts whatever it is
        /// handed, and the answer's own sort breaks equal scores on the path —
        /// and this property is what fails if a change ever leaves neither.
        #[test]
        fn ranking_is_the_same_answer_however_the_walk_ordered_the_paths(
            paths in proptest::collection::btree_map(
                "[a-z]{1,5}(/[a-z]{1,5}){0,2}\\.(rs|md|toml)",
                0u64..4_000_000_000_000,
                1..24,
            ),
            seed in proptest::prelude::any::<u64>(),
            asked in proptest::sample::select(vec![
                "layout", "bus.rs", "*.rs", "canopy layout", "/src/", "a", "",
            ]),
        ) {
            let entries: Vec<FileEntry> = paths
                .iter()
                .map(|(path, mtime_ms)| FileEntry {
                    path: path.clone(),
                    mtime_ms: *mtime_ms,
                })
                .collect();
            let key = IndexKey {
                head_oid: "b".repeat(40),
                newest_dir_mtime_ms: 7,
            };
            let mut facts = FrecencyFacts::new();
            for (i, path) in paths.keys().enumerate() {
                if i % 3 == 0 {
                    facts.record(path, NOW_MS - 60_000 * (i as u64 + 1));
                }
            }
            let one_order = NativeEngine::new(
                PathIndex::new(key.clone(), entries.clone()).unwrap(),
            )
            .with_frecency(facts.clone());
            let another = NativeEngine::new(
                PathIndex::new(key, shuffled(entries, seed)).unwrap(),
            )
            .with_frecency(facts);

            let asked = query(asked, 8);
            let answer = one_order.rank_paths(&asked);
            proptest::prop_assert_eq!(
                &answer,
                &another.rank_paths(&asked),
                "two walk orders of one path set answered differently"
            );
            proptest::prop_assert_eq!(
                &answer,
                &one_order.rank_paths(&asked),
                "the same engine answered its own question twice differently"
            );
        }
    }

    /// A path nothing is known about scores its match and no warmth, and an
    /// empty index answers everything with nothing.
    #[test]
    fn an_empty_index_answers_emptily() {
        let engine = NativeEngine::new(PathIndex::default());
        assert_eq!(engine.rank_paths(&query("layout", 8)), Ranked::default());
    }
}
