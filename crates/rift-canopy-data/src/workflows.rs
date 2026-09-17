//! The workflow feeds: `rift dispatch` and `rift recap` as
//! Canopy cards.
//!
//! # One window arithmetic, two surfaces
//!
//! Everything here reads its facts through [`rift_store::window::gather`] — the
//! same function `bins/rift`'s verbs call, in a crate this one already depended
//! on. **Nothing in this module counts a checkpoint, ranks a file or rolls up an
//! attribution split of its own**, and that is the whole design constraint: a
//! card and a report that disagreed about the same repository would be worse
//! than either alone, and the only way to make disagreement impossible is to
//! have one implementation. `bins/rift`'s
//! `a_dispatch_card_and_the_dispatch_verb_agree_on_a_seeded_projection` drives
//! both sides over one seeded projection and asserts the numbers are equal — it
//! lives there because a binary can see both halves and this crate can only see
//! one.
//!
//! What this module *does* own is the three things a card owes that a document
//! does not: the **absence rules** (an unreadable projection and an empty
//! window are different sentences, and neither is a zero), the **escaping**
//! (every borrowed string leaves here through
//! [`rift_canopy::escape_for_terminal`], byte-identically to what the CLI
//! prints), and the **row budget** (a card carries the top rows and *counts*
//! what it dropped).
//!
//! # The window is unbounded, deliberately
//!
//! Both cards gather `Window::resolve(None, None)` — everything this repository
//! has. Two reasons, and the second is the one that matters: a bounded card
//! would need a clock, and a surface whose numbers depend on when it rendered
//! cannot be compared with itself or with the `rift dispatch` a user runs
//! beside it. The trailing-month view already exists as the activity card.
//!
//! # Citations
//!
//! `rift dispatch --json` and `rift recap --json` replace every payload-derived
//! string with `cite:<n>` and resolve the map on their human surface. A card is
//! a human surface reading the data plane directly, so it renders the value
//! itself — and it never prints or re-derives an ordinal, because two
//! authorities on what `cite:3` names is exactly one too many.

use std::path::{Path, PathBuf};

use rift_canopy::col3::{
    DispatchData, DispatchView, FileTouch, RecapData, RecapSavings, RecapView,
};
use rift_canopy::{escape_for_terminal, FeedPayload, FeedUpdate};
use rift_core::Result;
use rift_shadow_cost::fmt_usd_estimate;
use rift_store::window::{Window, WindowFacts};
use rift_store::{projection_db_for_family_under, recall_db_for_family_under, SqliteStore};

use crate::feeds::project_family;
use crate::store::runtime;

/// How many table rows a card carries.
///
/// The panel clips to its own height anyway; this bound is about what crosses
/// the bus — a repository with ten thousand touched paths must not push ten
/// thousand strings through a feed to render six of them. The *total* rides
/// beside the list ([`DispatchData::files_total`],
/// [`RecapData::split_total`]) so the card can say what it dropped instead of
/// quietly showing a prefix.
pub const MAX_CARD_ROWS: usize = 10;

/// Both workflow cards for one project, from **one** gather.
///
/// They are separate feeds — each coalesces under its own key and carries full
/// section state — but they answer the same window from the same facts, so
/// gathering twice would be two projection opens and, worse, two chances for
/// the two cards on one screen to disagree with each other.
#[must_use]
pub fn compute_workflow_cards(root: &Path, path: &str) -> (FeedUpdate, FeedUpdate) {
    let (dispatch, recap) = match read_window(root, path) {
        Some(facts) => (dispatch_view(&facts), recap_view(&facts, false)),
        // No readable projection is its own sentence, not an empty window.
        None => (DispatchView::NoProjection, RecapView::NoProjection),
    };
    (
        FeedUpdate {
            project: path.to_string(),
            payload: FeedPayload::Dispatch(dispatch),
        },
        FeedUpdate {
            project: path.to_string(),
            payload: FeedPayload::Recap(recap),
        },
    )
}

/// This project's `recall.db`, when it has one. `None` is an absent fact, and
/// this never creates the file — a card is a read.
fn recall_db_for(root: &Path, path: &str) -> Option<PathBuf> {
    let db = recall_db_for_family_under(root, &project_family(Path::new(path))?);
    db.exists().then_some(db)
}

/// Gather the unbounded window over this project's own projection, then ask its
/// policy which touched paths are sensitive. `None` when there is no readable
/// projection — which is a different fact from an empty one.
fn read_window(root: &Path, path: &str) -> Option<WindowFacts> {
    let db = projection_db_for_family_under(root, &project_family(Path::new(path))?);
    if !db.exists() {
        return None;
    }
    let store = SqliteStore::open(&db).ok()?;
    let rt = runtime().ok()?;
    let window = Window::resolve(None, None).ok()?;
    let recall = recall_db_for(root, path);
    let mut facts = rt
        .block_on(window_facts(&store, recall.as_deref(), &window))
        .ok()?;
    apply_policy(&mut facts, Path::new(path));
    Some(facts)
}

/// [`rift_store::window::gather`] over an already-open projection — the seam
/// the cross-surface test drives, so that test exercises this crate's real
/// read rather than a copy of it.
///
/// # Errors
/// Propagates projection read errors. A missing recall ledger is an absent
/// fact, not an error.
pub async fn window_facts(
    capture: &SqliteStore,
    recall_db: Option<&Path>,
    window: &Window,
) -> Result<WindowFacts> {
    rift_store::window::gather(capture, recall_db, window).await
}

/// Ask this repository's policy which touched paths are sensitive.
///
/// [`rift_store::window::gather`] leaves the field `None` on purpose — storage
/// does not read project policy — and every surface fills it in before
/// rendering. The *filter* is three lines; the answer it wraps is
/// `rift_config::EffectiveConfig::is_sensitive_path`, which is the single
/// implementation both surfaces call. A config that cannot be read falls back
/// to the secure default, whose built-in sensitive set is non-droppable, so the
/// answer degrades toward flagging more rather than less.
pub fn apply_policy(facts: &mut WindowFacts, project_root: &Path) {
    let cfg = rift_config::EffectiveConfig::load(&project_root.join(".rift"))
        .or_else(|_| rift_config::EffectiveConfig::secure_default());
    let Ok(cfg) = cfg else {
        facts.sensitive_paths = Some(Vec::new());
        return;
    };
    facts.sensitive_paths = Some(
        facts
            .files
            .iter()
            .filter(|(p, _)| cfg.is_sensitive_path(p))
            .map(|(p, _)| p.clone())
            .collect(),
    );
}

/// The dispatch card over gathered facts — a pure projection of
/// [`WindowFacts`], so there is nothing here that could count differently from
/// the report.
#[must_use]
pub fn dispatch_view(facts: &WindowFacts) -> DispatchView {
    if facts.checkpoints.is_empty() && facts.sessions.is_empty() {
        return DispatchView::NoActivity;
    }
    // `None` = the policy was never asked (no readable config). It stays
    // `None` all the way to the card, which renders "sensitive unasked" —
    // an unasked answer must not become the zero that claims a clean window
    // (the same honesty rung the priced savings estimate stands on).
    let sensitive = facts.sensitive_paths.as_deref();
    let files: Vec<FileTouch> = facts
        .files
        .iter()
        .take(MAX_CARD_ROWS)
        .map(|(path, count)| FileTouch {
            label: escape_for_terminal(path),
            checkpoints: *count,
            sensitive: sensitive.is_some_and(|s| s.iter().any(|p| p == path)),
        })
        .collect();
    DispatchView::Data(DispatchData {
        checkpoints: count(facts.checkpoints.len()),
        derived: count(facts.checkpoints.iter().filter(|c| c.derived).count()),
        unresolved: u64::try_from(facts.unresolved_checkpoints.max(0)).unwrap_or(u64::MAX),
        sessions: count(facts.sessions.len()),
        open_sessions: count(facts.open_sessions.len()),
        agent_lines: facts.attribution.agent_lines,
        mixed_lines: facts.attribution.mixed_lines,
        human_lines: facts.attribution.human_lines,
        confidence_milli: facts.attribution.confidence_milli,
        files_total: count(facts.files.len()),
        files,
        sensitive: sensitive.map(|s| count(s.len())),
    })
}

/// The recap card over gathered facts. `by_model` picks the split axis, exactly
/// as `rift recap --model` does; the grouping itself is the store's one
/// `session_split`.
#[must_use]
pub fn recap_view(facts: &WindowFacts, by_model: bool) -> RecapView {
    if facts.sessions.is_empty() && facts.checkpoints.is_empty() {
        return RecapView::NoActivity;
    }
    let split = facts.session_split(by_model);
    RecapView::Data(RecapData {
        sessions: count(facts.sessions.len()),
        checkpoints: count(facts.checkpoints.len()),
        tokens: u64::try_from(facts.tokens.total().max(0)).unwrap_or(u64::MAX),
        // No priced cost fact in the window means no figure at all. A `~$0.00`
        // drawn over an unpriced window is a fabricated number, and the ladder
        // that governs the recall chip (the priced savings estimate rung 4) governs this too.
        cost_label: (facts.cost_micros > 0).then(|| fmt_usd_estimate(facts.cost_micros)),
        by_model,
        split_total: count(split.len()),
        split: split
            .iter()
            .take(MAX_CARD_ROWS)
            .map(|(key, n)| (escape_for_terminal(key), count(*n)))
            .collect(),
        savings: facts.savings.map(|s| RecapSavings {
            avoided_upper_bound: u64::try_from(s.avoided_upper_bound.max(0)).unwrap_or(u64::MAX),
            injected: u64::try_from(s.injected.max(0)).unwrap_or(u64::MAX),
        }),
    })
}

/// A `usize` count as the view-models carry it.
fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_store::window::{AttributedFile, CheckpointFact, Savings, SessionFact, TokenClasses};

    fn facts() -> WindowFacts {
        let mut f = WindowFacts {
            checkpoints: vec![
                CheckpointFact {
                    checkpoint_id: "c1".to_owned(),
                    at: Some(2_000),
                    derived: false,
                },
                CheckpointFact {
                    checkpoint_id: "c2".to_owned(),
                    at: Some(5_000),
                    derived: true,
                },
            ],
            unresolved_checkpoints: 1,
            sessions: vec![
                SessionFact {
                    session_id: "s1".to_owned(),
                    agent: "claude-code".to_owned(),
                    model: "opus".to_owned(),
                    started_at: 1_000,
                },
                SessionFact {
                    session_id: "s2".to_owned(),
                    agent: "codex".to_owned(),
                    model: "sonnet".to_owned(),
                    started_at: 5_000,
                },
            ],
            open_sessions: vec!["s2".to_owned()],
            attributed: vec![
                AttributedFile {
                    checkpoint_id: "c1".to_owned(),
                    path: "src/a.rs".to_owned(),
                    agent_added: 20,
                    human_added: 2,
                    human_modified_agent: 1,
                    agent_removed_by_human: 0,
                    confidence_milli: 900,
                },
                AttributedFile {
                    checkpoint_id: "c2".to_owned(),
                    path: ".env".to_owned(),
                    agent_added: 5,
                    human_added: 1,
                    human_modified_agent: 1,
                    agent_removed_by_human: 1,
                    confidence_milli: 500,
                },
            ],
            tokens: TokenClasses {
                uncached_input: 100,
                cached_input: 200,
                cache_creation: 30,
                output: 60,
            },
            cost_micros: 1_250,
            savings: Some(Savings {
                avoided_upper_bound: 1_000,
                injected: 40,
                returned: 150,
            }),
            ..WindowFacts::default()
        };
        // `files` and `attribution` are derived, never hand-written: the card
        // must be a projection of the store's own rollup.
        f.restrict_to(&["c1".to_owned(), "c2".to_owned()].into_iter().collect());
        f
    }

    #[test]
    fn the_dispatch_card_is_a_projection_of_the_windows_own_rollup() {
        let mut f = facts();
        apply_policy(&mut f, Path::new("/no/such/repo"));
        let DispatchView::Data(d) = dispatch_view(&f) else {
            panic!("seeded facts must produce data");
        };
        assert_eq!(d.checkpoints, 2);
        assert_eq!(d.derived, 1, "and it is reported as derived");
        assert_eq!(d.unresolved, 1);
        assert_eq!(d.sessions, 2);
        assert_eq!(d.open_sessions, 1);
        assert_eq!((d.agent_lines, d.mixed_lines, d.human_lines), (25, 2, 3));
        assert_eq!(d.confidence_milli, Some(700), "(900+500)/2");
        assert_eq!(d.files_total, 2);
        // Ranked by touch count then path — the window's own total order.
        assert_eq!(d.files[0].label, "\".env\"");
        assert!(d.files[0].sensitive, "the secure default flags .env");
        assert!(!d.files[1].sensitive);
        assert_eq!(d.sensitive, Some(1));
    }

    #[test]
    fn the_recap_card_takes_the_axis_it_was_asked_for() {
        let f = facts();
        let RecapView::Data(d) = recap_view(&f, false) else {
            panic!("data");
        };
        assert_eq!(d.sessions, 2);
        assert_eq!(d.tokens, 390);
        // The money authority's own string — a sub-cent window renders as
        // "less than a cent", never as a fabricated zero.
        assert_eq!(d.cost_label.as_deref(), Some("~<$0.01"));
        assert!(!d.by_model);
        assert_eq!(
            d.split,
            vec![
                ("\"claude-code\"".to_owned(), 1),
                ("\"codex\"".to_owned(), 1)
            ]
        );
        let RecapView::Data(d) = recap_view(&f, true) else {
            panic!("data");
        };
        assert!(d.by_model);
        assert_eq!(
            d.split,
            vec![("\"opus\"".to_owned(), 1), ("\"sonnet\"".to_owned(), 1)]
        );
        let s = d.savings.expect("the fixture carries a ledger");
        assert_eq!(s.avoided_upper_bound, 1_000);
        assert_eq!(s.injected, 40);
    }

    /// An empty window is the honest-empty state, and a repository with no
    /// ledger reports absence rather than a zero.
    #[test]
    fn an_empty_window_is_empty_and_an_absent_ledger_is_absent() {
        let empty = WindowFacts::default();
        assert_eq!(dispatch_view(&empty), DispatchView::NoActivity);
        assert_eq!(recap_view(&empty, false), RecapView::NoActivity);

        let mut f = facts();
        f.savings = None;
        f.cost_micros = 0;
        let RecapView::Data(d) = recap_view(&f, false) else {
            panic!("data");
        };
        assert!(d.savings.is_none(), "absent, not a zero");
        assert!(d.cost_label.is_none(), "no priced fact, no figure");
    }

    /// A window whose files nobody asked policy about must not read as "nothing
    /// is sensitive" — the card shows what was computed, and `gather` computes
    /// `None`.
    #[test]
    fn an_unasked_policy_answer_flags_nothing_and_claims_nothing() {
        let f = facts();
        assert_eq!(f.sensitive_paths, None, "gather leaves it unasked");
        let DispatchView::Data(d) = dispatch_view(&f) else {
            panic!("data");
        };
        assert_eq!(d.sensitive, None, "unasked is not zero");
        assert!(d.files.iter().all(|x| !x.sensitive));
    }

    /// An ASKED policy that flags nothing is `Some(0)` — the clean window the
    /// unasked case must never be mistaken for.
    #[test]
    fn an_asked_empty_answer_is_some_zero() {
        let mut f = facts();
        f.sensitive_paths = Some(Vec::new());
        let DispatchView::Data(d) = dispatch_view(&f) else {
            panic!("data");
        };
        assert_eq!(d.sensitive, Some(0));
        assert!(d.files.iter().all(|x| !x.sensitive));
    }

    /// Borrowed strings leave this crate escaped, and the row budget counts
    /// what it dropped instead of showing a silent prefix.
    #[test]
    fn labels_are_escaped_and_the_row_budget_is_declared() {
        let mut f = WindowFacts {
            attributed: (0..MAX_CARD_ROWS + 5)
                .map(|i| AttributedFile {
                    checkpoint_id: format!("c{i}"),
                    path: format!("src/f{i}\u{1b}[31m.rs"),
                    agent_added: 1,
                    human_added: 0,
                    human_modified_agent: 0,
                    agent_removed_by_human: 0,
                    confidence_milli: 500,
                })
                .collect(),
            ..WindowFacts::default()
        };
        let ids = (0..MAX_CARD_ROWS + 5).map(|i| format!("c{i}")).collect();
        f.restrict_to(&ids);
        f.checkpoints = vec![CheckpointFact {
            checkpoint_id: "c0".to_owned(),
            at: Some(1),
            derived: false,
        }];
        let DispatchView::Data(d) = dispatch_view(&f) else {
            panic!("data");
        };
        assert_eq!(d.files.len(), MAX_CARD_ROWS);
        assert_eq!(d.files_total, (MAX_CARD_ROWS + 5) as u64, "the total rides");
        for file in &d.files {
            assert!(!file.label.contains('\u{1b}'), "raw ESC: {}", file.label);
            assert!(file.label.contains("\\u{1b}"), "escaped: {}", file.label);
        }
    }
}
