//! Optional: frecency-ranked fuzzy file search.
//!
//! The production backend is `fff-search` (a warm-index, sub-10ms file finder);
//! it plugs in behind the [`FileSearch`] seam. This crate provides a pure,
//! in-memory subsequence fuzzy matcher with **frecency** ranking (access score +
//! recency score), so the search logic is testable and there is a usable
//! fallback when the warm index is absent.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]
#![allow(
    clippy::module_name_repetitions,
    clippy::doc_markdown,
    clippy::cast_precision_loss
)]

/// A file in the index with its frecency signals.
#[derive(Clone, Debug, PartialEq)]
pub struct FileEntry {
    /// Repo-relative path.
    pub path: String,
    /// Access frequency score (higher = more used).
    pub access_score: f64,
    /// Recency score (higher = more recently touched).
    pub recency_score: f64,
}

/// A ranked search hit.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// The matched path.
    pub path: String,
    /// The combined relevance score (match quality × frecency).
    pub score: f64,
}

/// A file-search backend.
pub trait FileSearch {
    /// Search for `query`, returning hits best-first.
    fn search(&self, query: &str) -> Vec<Hit>;
}

/// Case-insensitive subsequence match. Returns a quality score in `(0,1]` (denser
/// matches — fewer gaps — score higher), or `None` if `query` is not a
/// subsequence of `path`.
#[must_use]
pub fn subsequence_score(query: &str, path: &str) -> Option<f64> {
    if query.is_empty() {
        return Some(0.5);
    }
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let p: Vec<char> = path.to_lowercase().chars().collect();
    let mut qi = 0;
    let mut first = None;
    let mut last = 0;
    for (i, c) in p.iter().enumerate() {
        if q.get(qi) == Some(c) {
            if first.is_none() {
                first = Some(i);
            }
            last = i;
            qi += 1;
        }
    }
    if qi != q.len() {
        return None;
    }
    let span = (last - first.unwrap_or(0) + 1) as f64;
    // density = matched chars / span; tighter (closer to contiguous) → higher.
    Some(q.len() as f64 / span)
}

/// An in-memory frecency-ranked index (the fallback backend).
#[derive(Default)]
pub struct MemoryIndex {
    entries: Vec<FileEntry>,
}

impl MemoryIndex {
    /// A new empty index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an entry.
    #[must_use]
    pub fn with(mut self, entry: FileEntry) -> Self {
        self.entries.push(entry);
        self
    }
}

impl FileSearch for MemoryIndex {
    fn search(&self, query: &str) -> Vec<Hit> {
        let mut hits: Vec<Hit> = self
            .entries
            .iter()
            .filter_map(|e| {
                let quality = subsequence_score(query, &e.path)?;
                // frecency multiplies match quality so a frequently/recently used
                // file with an equal match wins.
                let frecency = 1.0 + e.access_score + e.recency_score;
                Some(Hit {
                    path: e.path.clone(),
                    score: quality * frecency,
                })
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(core::cmp::Ordering::Equal)
        });
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_matches_and_scores() {
        assert!(subsequence_score("mn", "main.rs").is_some());
        assert!(subsequence_score("xyz", "main.rs").is_none());
        // contiguous match scores higher than spread-out
        let tight = subsequence_score("mai", "main.rs").unwrap();
        let loose = subsequence_score("mis", "main.rs").unwrap();
        assert!(tight > loose);
    }

    #[test]
    fn frecency_breaks_ties() {
        let idx = MemoryIndex::new()
            .with(FileEntry {
                path: "src/main.rs".into(),
                access_score: 0.0,
                recency_score: 0.0,
            })
            .with(FileEntry {
                path: "src/main.rs.bak".into(),
                access_score: 5.0,
                recency_score: 5.0,
            });
        let hits = idx.search("main");
        // both match; the high-frecency one ranks first
        assert_eq!(hits.first().unwrap().path, "src/main.rs.bak");
    }

    #[test]
    fn no_match_yields_no_hits() {
        let idx = MemoryIndex::new().with(FileEntry {
            path: "lib.rs".into(),
            access_score: 1.0,
            recency_score: 1.0,
        });
        assert!(idx.search("zzz").is_empty());
    }
}
