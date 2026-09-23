//! `rift recap` — offline usage analytics over the governance tables.
//!
//! Its own module because it is its own document with its own contract
//! version and its own [`Screen`]: two verbs that shared a version number
//! would have to move together forever, and two documents that shared a
//! citation table would resolve the same ordinal differently depending on
//! which verb printed it.

use std::path::Path;

use rift_core::Result;
use rift_store::{CanonicalStore, SqliteStore};
use serde_json::json;

use super::{gather, window_json, Planes, Window, WindowFacts};
use crate::cli::Outcome;
use crate::screen::{Closed, Screen};

/// The `rift recap` JSON contract version. Its own, not dispatch's: two verbs
/// that shared a version number would have to move together forever.
///
/// **2 since the free-text screen** ([`crate::screen`]). `tool_mix[].tool`,
/// `serve_calls[].verb` and `split.counts[].key` still hold strings, but a
/// value Rift has no closed vocabulary for now arrives as `cite:<n>` and
/// resolves on the human render. The keys and the types are unchanged, which is
/// exactly why the version has to move: a pinned consumer would otherwise read
/// a citation as a tool name and never fail.
pub(crate) const RECAP_SCHEMA_VERSION: u32 = 2;

/// `rift recap` — offline usage analytics over the governance tables.
///
/// Fully local, which is the comparison worth making: the tool this replaces
/// requires a login to answer the same question.
///
/// # Errors
/// [`RiftError`](rift_core::RiftError) on a projection read failure or an unusable window.
pub(crate) async fn recap(
    store: &SqliteStore,
    root: &Path,
    since: Option<&str>,
    until: Option<&str>,
    by_model: bool,
) -> Result<Outcome> {
    recap_in(&Planes::open(store, root)?, since, until, by_model).await
}

/// `rift recap` over already-resolved planes.
///
/// # Errors
/// As [`recap`].
pub(crate) async fn recap_in(
    planes: &Planes<'_>,
    since: Option<&str>,
    until: Option<&str>,
    by_model: bool,
) -> Result<Outcome> {
    let window = Window::resolve(since, until)?;
    let facts = gather(planes, &window).await?;
    let mix = tool_mix(planes.capture, &window).await?;
    let calls = serve_calls(planes.recall_db.as_deref(), &window).await?;
    let split = split_by(&facts, by_model);
    let worktrees = crate::provenance::worktree_spread(
        planes.capture,
        &facts
            .checkpoints
            .iter()
            .map(|c| c.checkpoint_id.clone())
            .collect::<Vec<_>>(),
    )
    .await;
    // The recap's own screen. It is NOT dispatch's: a citation is document-local
    // (see [`Screen`]), so two documents sharing a table would mean an ordinal
    // that resolved differently depending on which verb printed it.
    let mut screen = Screen::default();
    let cited = cite_recap(&mut screen, &mix, calls.as_deref(), &split, by_model);
    let doc = recap_json(&window, &facts, &worktrees, &cited);
    Ok(Outcome::ok(
        render_recap(&facts, &mix, &split, by_model, &screen, &worktrees),
        doc,
    ))
}

/// A tool-name histogram over a window.
///
/// # Errors
/// Propagates projection read errors.
async fn tool_mix(store: &SqliteStore, window: &Window) -> Result<Vec<(String, i64)>> {
    let mut out = Vec::new();
    for r in store
        .query(
            "SELECT tool_name, COUNT(*) AS n FROM tool_call \
             WHERE started_at >= ?1 AND started_at <= ?2 \
             GROUP BY tool_name ORDER BY n DESC, tool_name",
            &window.bounds(),
        )
        .await?
    {
        out.push((r.get_text(0)?, r.get_i64(1)?));
    }
    Ok(out)
}

/// Serve counts per `memd` verb, or `None` when this repository has no ledger.
///
/// The verb is the fact: `memd.manifest_served` and `memd.manifest_injected`
/// are a pull and a push and are never summed into one "serves" number here,
/// because that sum is what made injected spend look like credit once already.
async fn serve_calls(db: Option<&Path>, window: &Window) -> Result<Option<Vec<(String, i64)>>> {
    let Some(db) = db else { return Ok(None) };
    let store = SqliteStore::open_read_only(db)?;
    let mut out = Vec::new();
    for r in store
        .query(
            "SELECT verb, COUNT(*) AS n FROM memd_audit_event \
             WHERE recorded_at >= ?1 AND recorded_at <= ?2 \
             GROUP BY verb ORDER BY verb",
            &window.bounds(),
        )
        .await?
    {
        out.push((r.get_text(0)?, r.get_i64(1)?));
    }
    Ok(Some(out))
}

/// Session counts per agent, or per model — **the store's derivation, not a
/// second one here**.
///
/// It was eleven lines of `BTreeMap` grouping in this file until Canopy's recap
/// panel needed the same answer. Two groupings over one `sessions` list is the
/// shape that lets a card and a document disagree about the same window, so the
/// rollup moved beside `aggregate` in [`rift_store::window`] and this is the
/// call. Sorting (by key, never by count) is stated there.
fn split_by(facts: &WindowFacts, by_model: bool) -> Vec<(String, usize)> {
    facts.session_split(by_model)
}

/// The recap document's free-text fields, screened, and the axis the split was
/// asked for.
struct CitedRecap {
    mix: Vec<(String, i64)>,
    calls: Option<Vec<(String, i64)>>,
    split: Vec<(String, usize)>,
    /// `"model"` or `"agent"` — the axis, as the document names it.
    by: &'static str,
}

/// Screen every free-text field the recap document carries, in the order the
/// document renders them, so an ordinal is a function of the window rather than
/// of `json!`'s evaluation order.
///
/// The split's key is an agent token or a model name depending on the axis, so
/// the closed thing it is drawn from depends on the axis too. Passing
/// `AgentToken` for a model would admit any string a registry token happens to
/// equal; passing `Nothing` for an agent would cite `claude-code`.
fn cite_recap(
    screen: &mut Screen,
    mix: &[(String, i64)],
    calls: Option<&[(String, i64)]>,
    split: &[(String, usize)],
    by_model: bool,
) -> CitedRecap {
    let split_closed = if by_model {
        Closed::Nothing
    } else {
        Closed::AgentToken
    };
    CitedRecap {
        mix: mix
            .iter()
            .map(|(name, n)| (screen.cite(Closed::Nothing, name), *n))
            .collect(),
        calls: calls.map(|c| {
            c.iter()
                .map(|(verb, n)| (screen.cite(Closed::Nothing, verb), *n))
                .collect()
        }),
        split: split
            .iter()
            .map(|(k, n)| (screen.cite(split_closed, k), *n))
            .collect(),
        by: if by_model { "model" } else { "agent" },
    }
}

/// The savings RATE, and its denominator is named in the key it lands under: a
/// rate over "tokens that were spent" is a different number from a rate over
/// "tokens that would have been", and an unlabelled percentage is the shape a
/// claim hides in.
///
/// The ratio itself is the ledger's guarded one — the same function the
/// dashboard's share and `avoidable_context_rate` go through — so this verb's
/// figure differs from theirs in its DENOMINATOR and in nothing else. It used
/// to divide inline behind a zero guard of its own, which is the "two
/// authorities for one number" shape that eventually produces a figure nobody
/// can reconcile.
fn savings_rate_percent(facts: &WindowFacts) -> Option<f64> {
    facts.savings.and_then(|s| {
        let spent = u64::try_from(facts.tokens.total()).unwrap_or(0);
        let avoided = u64::try_from(s.avoided_upper_bound).unwrap_or(0);
        let denom = spent.saturating_add(avoided);
        (denom > 0).then(|| {
            let r = rift_recall_ledger::savings_rate(avoided, denom);
            (r * 1000.0).round() / 10.0
        })
    })
}

/// The `--json` document. Every list is already sorted by its query, so the
/// same window serializes byte-identically.
fn recap_json(
    window: &Window,
    facts: &WindowFacts,
    worktrees: &[String],
    cited: &CitedRecap,
) -> serde_json::Value {
    let rate = savings_rate_percent(facts);
    json!({
        "schema_version": RECAP_SCHEMA_VERSION,
        "window": window_json(window),
        "sessions": facts.sessions.len(),
        "checkpoints": facts.checkpoints.len(),
        "worktrees": worktrees,
        "tokens": {
            "uncached_input": facts.tokens.uncached_input,
            "cached_input": facts.tokens.cached_input,
            "cache_creation": facts.tokens.cache_creation,
            "output": facts.tokens.output,
            "total": facts.tokens.total(),
        },
        "cost": { "amount_micros": facts.cost_micros },
        "tool_mix": cited.mix.iter().map(|(name, n)| json!({ "tool": name, "calls": n }))
            .collect::<Vec<_>>(),
        "serve_calls": cited.calls.as_ref().map_or(json!(null), |c| c.iter()
            .map(|(verb, n)| json!({ "verb": verb, "calls": n })).collect()),
        "split": {
            "by": cited.by,
            "counts": cited.split.iter().map(|(k, n)| json!({ "key": k, "sessions": n }))
                .collect::<Vec<_>>(),
        },
        "savings": facts.savings.map_or(json!({ "available": false }), |s| json!({
            "available": true,
            "tokens_avoided_estimated_upper_bound": s.avoided_upper_bound,
            "tokens_injected_spend": s.injected,
            "rate_percent_of_avoided_plus_spent": rate,
        })),
    })
}

/// The human recap. Tool names and agent names are escaped for the same reason
/// paths are: an agent-namespaced tool name is not Rift's own text.
///
/// It takes the RAW `mix` and `split` rather than the cited ones, because this
/// is a human-only surface and it is where a citation is meant to resolve. The
/// [`Screen`]'s map rides at the end so an operator holding a `--json` document
/// beside this one can read `cite:3` back.
pub(super) fn render_recap(
    facts: &WindowFacts,
    mix: &[(String, i64)],
    split: &[(String, usize)],
    by_model: bool,
    screen: &Screen,
    worktrees: &[String],
) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "recap  sessions {}  checkpoints {}  tokens {}",
        facts.sessions.len(),
        facts.checkpoints.len(),
        facts.tokens.total()
    );
    let _ = writeln!(
        s,
        "  worktrees   {}",
        crate::provenance::spread_text(worktrees)
    );
    let _ = writeln!(s, "  by {}:", if by_model { "model" } else { "agent" });
    for (key, n) in split {
        let _ = writeln!(
            s,
            "    {n:>4}  {}",
            crate::provenance::safe_path(key.as_str())
        );
    }
    if !mix.is_empty() {
        let _ = writeln!(s, "  tools:");
        for (name, n) in mix.iter().take(10) {
            let _ = writeln!(
                s,
                "    {n:>4}  {}",
                crate::provenance::safe_path(name.as_str())
            );
        }
    }
    screen.push_map(&mut s);
    s
}

#[cfg(test)]
mod tests {
    use rift_store::window::SessionFact;

    use super::*;

    #[test]
    fn a_recap_split_counts_sessions_by_the_axis_it_was_asked_for() {
        let facts = WindowFacts {
            sessions: vec![
                SessionFact {
                    session_id: "s1".into(),
                    agent: "claude-code".into(),
                    model: "opus".into(),
                    started_at: 1,
                },
                SessionFact {
                    session_id: "s2".into(),
                    agent: "claude-code".into(),
                    model: "sonnet".into(),
                    started_at: 2,
                },
                SessionFact {
                    session_id: "s3".into(),
                    agent: "codex".into(),
                    model: "opus".into(),
                    started_at: 3,
                },
            ],
            ..WindowFacts::default()
        };
        assert_eq!(
            split_by(&facts, false),
            vec![("claude-code".to_owned(), 2), ("codex".to_owned(), 1)]
        );
        assert_eq!(
            split_by(&facts, true),
            vec![("opus".to_owned(), 2), ("sonnet".to_owned(), 1)]
        );
    }

    #[test]
    fn a_hostile_agent_name_cannot_reach_the_rendered_recap() {
        // An agent name is written by whatever wrote the session row; it is not
        // Rift's own text, and the recap prints it.
        let hostile = "\u{1b}]0;pwned\u{7}claude";
        let facts = WindowFacts {
            sessions: vec![SessionFact {
                session_id: "s1".into(),
                agent: hostile.to_owned(),
                model: hostile.to_owned(),
                started_at: 1,
            }],
            ..WindowFacts::default()
        };
        let mix = vec![(hostile.to_owned(), 3)];
        let split = split_by(&facts, false);
        let out = render_recap(&facts, &mix, &split, false, &Screen::default(), &[]);
        for needle in ["\u{1b}", "\u{7}"] {
            assert!(
                !out.contains(needle),
                "a control sequence reached the recap"
            );
        }
        assert!(out.contains("claude"), "the name is still readable");
    }
}
