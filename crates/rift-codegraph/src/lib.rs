//! Optional: a code-relationship graph (the retrieval plane).
//!
//! The production backend is the `codegraph` crate (a graph DB) fed by
//! tree-sitter language variants; it plugs in behind the [`CodeGraph`] seam. This
//! crate provides a pure, in-memory graph with the queries that matter for
//! retrieval — `callers`, `callees`, and transitive `impact` (blast radius) — so
//! the relationship logic is testable without the full engine.
//!
//! It lets an agent answer "who calls this?" with a graph query instead of a
//! grep/Read discovery loop (the *don't fetch it* layer).
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// An edge kind in the code graph.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EdgeKind {
    /// `from` calls `to`.
    Calls,
    /// `from` imports `to`.
    Imports,
    /// `from` contains `to` (file → symbol).
    Contains,
}

/// The code-relationship query surface.
pub trait CodeGraph {
    /// Symbols that directly call `symbol`.
    fn callers(&self, symbol: &str) -> Vec<String>;
    /// Symbols `symbol` directly calls.
    fn callees(&self, symbol: &str) -> Vec<String>;
    /// The transitive blast radius: everything that (transitively) calls `symbol`.
    fn impact(&self, symbol: &str) -> BTreeSet<String>;
}

/// An in-memory code graph.
#[derive(Default)]
pub struct MemoryGraph {
    // call edges: caller -> {callees}
    calls: BTreeMap<String, BTreeSet<String>>,
    // reverse call edges: callee -> {callers}
    called_by: BTreeMap<String, BTreeSet<String>>,
}

impl MemoryGraph {
    /// A new empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an edge. Only `Calls` edges populate the call graph here; `Imports`/
    /// `Contains` are accepted for completeness but not traversed by the call
    /// queries.
    pub fn add_edge(&mut self, from: &str, to: &str, kind: EdgeKind) {
        if kind == EdgeKind::Calls {
            self.calls
                .entry(from.to_string())
                .or_default()
                .insert(to.to_string());
            self.called_by
                .entry(to.to_string())
                .or_default()
                .insert(from.to_string());
        }
    }
}

impl CodeGraph for MemoryGraph {
    fn callers(&self, symbol: &str) -> Vec<String> {
        self.called_by
            .get(symbol)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn callees(&self, symbol: &str) -> Vec<String> {
        self.calls
            .get(symbol)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn impact(&self, symbol: &str) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(symbol.to_string());
        while let Some(sym) = queue.pop_front() {
            if let Some(callers) = self.called_by.get(&sym) {
                for c in callers {
                    if seen.insert(c.clone()) {
                        queue.push_back(c.clone());
                    }
                }
            }
        }
        seen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> MemoryGraph {
        // main -> handler -> charge -> retry ; audit -> charge
        let mut g = MemoryGraph::new();
        g.add_edge("main", "handler", EdgeKind::Calls);
        g.add_edge("handler", "charge", EdgeKind::Calls);
        g.add_edge("charge", "retry", EdgeKind::Calls);
        g.add_edge("audit", "charge", EdgeKind::Calls);
        g
    }

    #[test]
    fn callers_and_callees() {
        let g = graph();
        assert_eq!(g.callees("handler"), vec!["charge".to_string()]);
        let mut callers = g.callers("charge");
        callers.sort();
        assert_eq!(callers, vec!["audit".to_string(), "handler".to_string()]);
    }

    #[test]
    fn impact_is_transitive_blast_radius() {
        let g = graph();
        // changing `retry` impacts charge, handler, audit, main (everyone upstream)
        let impact = g.impact("retry");
        assert!(impact.contains("charge"));
        assert!(impact.contains("handler"));
        assert!(impact.contains("audit"));
        assert!(impact.contains("main"));
    }

    #[test]
    fn leaf_symbol_has_no_callers() {
        let g = graph();
        assert!(g.callers("main").is_empty());
        assert!(g.impact("main").is_empty());
    }
}
