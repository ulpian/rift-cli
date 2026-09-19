//! The search verb: the paths in this checkout a query is about, ranked by the
//! binary, screened and rendered here — and the matching lines, through the
//! trust chokepoint.

use std::collections::BTreeSet;

use rift_recall_events::MemdEvent;
use rift_recall_policy::TurnBudget;
use rift_recall_types::{path_is_safe, OmitReason, Trust};
use rift_recall_validity::trust::{neutral_inline_span, IntoRawBody, PathStatus, TrustedPayload};
use serde_json::{json, Value};

use super::{fit_to_cap, manifest_cap};
use crate::envelope::omit_tag;
use crate::{
    IndexState, Search, SearchLine, SearchPath, ServeDispatcher, ToolError, ToolName, ToolRequest,
    ToolResponse, TAG_BASELINE_NOT_DECLARED, TAG_DECLARED_BASELINE, TAG_INDEX_PENDING,
};

/// What [`ServeDispatcher::screen_search`] left of one engine answer: the rows
/// that may be rendered, and how many were kept out and why.
struct Screened {
    /// Ranked paths, rendered.
    paths: Vec<Value>,
    /// Matching lines, rendered.
    rows: Vec<Value>,
    /// How many entries the renderer would not render.
    withheld: usize,
    /// How many distinct paths this repository's policy kept out.
    denied: usize,
}

/// An answered search as the fitter drops from it: a spine and a tail.
///
/// The droppable sequence is spine-first: a caller that asked for lines is
/// answered with lines, and the ranked paths are how they were found. Dropping
/// from the tail therefore costs the cheaper half of whichever question was
/// actually asked.
struct Answered {
    content: bool,
    spine: Vec<Value>,
    tail: Vec<Value>,
    total: usize,
    withheld: usize,
    denied: usize,
    /// What the avoided figure is measured against — a declared listing, or
    /// nothing at all for a content search.
    reason: &'static str,
}

impl ServeDispatcher<'_> {
    /// Answer a query with the paths in this checkout it is about, and — when
    /// the caller asked — the lines inside them that match.
    ///
    /// The pull side of the listing answer the hook path pushes: an agent that
    /// would have run a glob and read six files to find one runs this instead,
    /// and where no hook can answer for it — Codex, Antigravity, a person at a
    /// terminal — this is how it asks the same question.
    ///
    /// Four things happen here and nowhere else on this path:
    ///
    /// 1. **The policy screens the paths, not the caller.** A denied path is
    ///    not named and its lines are not read out; a sensitive path may still
    ///    be *named* — a name is not its contents — while its exact lines are
    ///    reached through the audited `retrieve_exact`, which asks for a
    ///    reason, or not at all. What the screens kept out is counted and
    ///    stated.
    /// 2. **A path renders as a neutral span or not at all.** The paths are the
    ///    part of this answer Rift composed rather than the part the caller
    ///    asked for, so they go out through the same renderer every pushed span
    ///    does, and one that will not render is counted rather than quoted.
    /// 3. **A line goes through the trust chokepoint.** Its provenance is a
    ///    tracked file of this checkout that the member's index holds — the
    ///    index excludes what git ignores and what policy calls sensitive, and
    ///    the engine drops what git does not track — so it is a code fact, and
    ///    what is checked here rather than trusted is the shape of the name it
    ///    arrived under. A `TrustedPayload` is the only way its bytes leave
    ///    this crate, and an unsafe name cannot build one.
    /// 4. **The figures are sealed at the accounting seam**, against the
    ///    declared listing baseline — and against nothing at all for a content
    ///    search, which has no declared native cost in this tree.
    pub(crate) fn search(
        &self,
        req: &ToolRequest,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        let Some(search) = req.search.as_ref() else {
            // The wire decoder cannot open an index, so a request that arrived
            // without one was not resolved by anybody. Answering it would mean
            // this crate going looking for a work tree.
            return Err(ToolError::Internal(
                "search: no resolved index on the request".to_owned(),
            ));
        };
        // An unreadable policy is not a permissive one: with no deny list to
        // consult, the honest answer is no answer rather than every path.
        if self.policy.is_fail_closed() {
            return Ok(Self::search_without_an_answer(
                search,
                "refused",
                omit_tag(OmitReason::PolicyDeniedExact),
                budget,
            ));
        }
        let IndexState::Answered { hits, total, lines } = &search.index else {
            return Ok(Self::search_without_an_answer(
                search,
                "pending",
                TAG_INDEX_PENDING,
                budget,
            ));
        };
        let answered = Answered::new(search.content, *total, self.screen_search(hits, lines));
        let n = answered.spine.len().saturating_add(answered.tail.len());
        let cap = manifest_cap(req, budget);
        let (payload, _kept) =
            fit_to_cap(cap, n, &|kept, truncated| answered.view(kept, truncated));
        Ok(Self::respond(
            ToolName::Search,
            payload,
            search.declared_baseline,
            budget,
            move |returned, avoided| MemdEvent::SearchServed {
                estimated_tokens_returned: returned,
                estimated_tokens_avoided: avoided,
            },
        ))
    }

    /// The two screens every search answer passes, applied once and counted.
    ///
    /// Kept apart from [`Self::search`] because it is the whole of what this
    /// crate decides about a search: everything else there is shaping and
    /// accounting. A path the repository denies is not named and its lines are
    /// not read out; a sensitive path may still be *named* — a name is not its
    /// contents — while its exact lines are reached through the audited
    /// `retrieve_exact`, which asks for a reason, or not at all. A name that is
    /// not a repository artifact, or that the renderer will not render, is
    /// counted rather than quoted, and a line is never attributed to a name the
    /// answer would not print. Both counts are stated in the answer, because a
    /// silently shorter list reads as "there is no such file" and sends the
    /// agent back to reading its way to one.
    fn screen_search(&self, hits: &[SearchPath], lines: &[SearchLine]) -> Screened {
        let mut denied: BTreeSet<&str> = BTreeSet::new();
        let mut withheld = 0usize;
        let mut paths: Vec<Value> = Vec::new();
        for hit in hits {
            if self.policy.is_denied_path(&hit.path) {
                denied.insert(hit.path.as_str());
                continue;
            }
            match renderable_path(&hit.path) {
                Some(span) => paths.push(json!({
                    "path": span,
                    "status": status_word(hit.status),
                })),
                None => withheld = withheld.saturating_add(1),
            }
        }
        let mut rows: Vec<Value> = Vec::new();
        for line in lines {
            if self.policy.is_denied_path(&line.path)
                || self.policy.exact_source_required(&line.path)
            {
                denied.insert(line.path.as_str());
                continue;
            }
            let Some(span) = renderable_path(&line.path) else {
                withheld = withheld.saturating_add(1);
                continue;
            };
            // Provenance decides, never the caller: an anomalous name is not a
            // repository artifact, it fails closed to untrusted, and an
            // untrusted class cannot become a `TrustedPayload` at all.
            let Ok(body) = TrustedPayload::new(
                matched_line_trust(&line.path),
                line.text.clone().into_bytes(),
            ) else {
                withheld = withheld.saturating_add(1);
                continue;
            };
            rows.push(json!({
                "path": span,
                "line": line.line,
                "text": body.into_raw_body(),
            }));
        }
        Screened {
            paths,
            rows,
            withheld,
            denied: denied.len(),
        }
    }

    /// A search that carries no ranked answer: the index is not built yet, or
    /// the policy could not be read.
    ///
    /// Neither is silence and neither claims anything — the baseline is zero
    /// whatever the caller's shape was, because nothing was replaced — and both
    /// are audited like any other serve, since a deferral that booked no row
    /// would be a serve nobody could count.
    fn search_without_an_answer(
        search: &Search,
        state: &'static str,
        reason: &'static str,
        budget: &mut TurnBudget,
    ) -> ToolResponse {
        let payload = json!({
            "tool": "search",
            "state": state,
            "content": search.content,
            "paths": Vec::<Value>::new(),
            "lines": Vec::<Value>::new(),
            "total": 0,
            "truncated": false,
            "omitted": 0,
            "withheld": 0,
            "denied": 0,
            "reason": reason,
        });
        Self::respond(ToolName::Search, payload, 0, budget, move |returned, _| {
            MemdEvent::SearchServed {
                estimated_tokens_returned: returned,
                estimated_tokens_avoided: 0,
            }
        })
    }
}

impl Answered {
    fn new(content: bool, total: usize, screened: Screened) -> Self {
        let Screened {
            paths,
            rows,
            withheld,
            denied,
        } = screened;
        let (spine, tail) = if content {
            (rows, paths)
        } else {
            (paths, rows)
        };
        let reason = if content {
            TAG_BASELINE_NOT_DECLARED
        } else {
            TAG_DECLARED_BASELINE
        };
        Self {
            content,
            spine,
            tail,
            total,
            withheld,
            denied,
            reason,
        }
    }

    /// The answer with the first `kept` droppable entries, spine first.
    fn view(&self, kept: usize, truncated: bool) -> Value {
        let spine_kept = kept.min(self.spine.len());
        let tail_kept = kept.saturating_sub(self.spine.len());
        let a = self.spine.get(..spine_kept).unwrap_or(&[]);
        let b = self.tail.get(..tail_kept).unwrap_or(&[]);
        let (lines, paths) = if self.content { (a, b) } else { (b, a) };
        let n = self.spine.len().saturating_add(self.tail.len());
        json!({
            "tool": "search",
            "state": "answered",
            "content": self.content,
            "paths": paths,
            "lines": lines,
            "total": self.total,
            "truncated": truncated,
            "omitted": n.saturating_sub(kept),
            "withheld": self.withheld,
            "denied": self.denied,
            "reason": self.reason,
        })
    }
}

/// The word a search answer states a path's git status as.
///
/// Rift's own fixed vocabulary, read off the one enum the pushed frame renders
/// from, so the JSON a caller parses and the sentence a model reads can never
/// name two different sets of states.
fn status_word(status: PathStatus) -> &'static str {
    match status {
        PathStatus::Tracked => "tracked",
        PathStatus::Modified => "modified",
        PathStatus::Staged => "staged",
        PathStatus::Untracked => "untracked",
    }
}

/// A path as a search answer may render it, or `None`.
///
/// Two screens, in this order. The first is structural: an absolute path, one
/// that climbs, or one carrying a control character is not a repository
/// artifact at all, and the citation pass drops one for the same reason — a
/// newline-bearing path could forge a line at any placement. The second is the
/// renderer's, because a path in this answer is a span Rift composed rather
/// than one the caller handed back, and it is judged like every other.
fn renderable_path(path: &str) -> Option<String> {
    path_is_safe(path).then(|| neutral_inline_span(path))?
}

/// The trust class of a line a content search matched.
///
/// A hit's provenance is fixed before it reaches this crate: it is a line of a
/// file the member's path index holds — which excludes what git ignores and
/// what policy calls sensitive — that git tracks, which is the same provenance
/// as any other blob in the work tree. So it is a code fact, exactly as an
/// exact retrieval of the same file would be, and it is served as one rather
/// than digested: an agent that ran the content search this replaces would have
/// been shown these bytes, and withholding a line for the shape of its own text
/// would be a false negative on a search rather than a defence.
///
/// What is checked here rather than trusted is the *name*: an unsafe path fails
/// closed to untrusted, which cannot become a [`TrustedPayload`] at all.
fn matched_line_trust(path: &str) -> Trust {
    if path_is_safe(path) {
        Trust::CodeFact
    } else {
        Trust::ToolOutputUntrusted
    }
}
