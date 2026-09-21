//! What one turn should do with the entries it has, and the delta question
//! that produces them.

use super::held::NamedHold;
use super::rows::ManifestRow;

/// The most changed entries one manifest will consider naming. The policy
/// budget is the real ceiling; this only bounds the walk that finds the fitting
/// prefix, so a thousand-file delta cannot turn one prompt into a thousand
/// tokenizer passes while the agent waits.
const MAX_MANIFEST_ENTRIES: usize = 64;

/// What one turn should do. Pure — the decision is a function of the baseline,
/// the delta and the budget — so the table can be tested exhaustively without a
/// projection, and the parts that touch disk stay thin enough to read.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum TurnDecision {
    /// Nothing changed. This is the authorized silence — the *only* state
    /// in which the injector may say nothing — and the only non-injecting state in
    /// which the projection may be recorded as accounted for.
    Silent,
    /// A delta exists and no manifest fits the turn budget — not even the bare
    /// frame with its `(N more not shown)` counter.
    ///
    /// **This is not the authorized silence and must never be treated as it.**
    /// The budget is a ceiling (and the chokepoint would have
    /// rendered something here — [`rift_recall_serve::render_delta_manifest`]
    /// returns `None` only for "nothing changed"), so nothing *can* be said
    /// within it. What keeps that honest is that nothing is absorbed and
    /// nothing is accounted for either: the delta is still owed, and the first
    /// turn whose budget can carry it announces it. Sharing one variant with
    /// [`TurnDecision::Silent`] is how the two states came to share a stamp,
    /// which turned a one-turn deviation into a permanent one.
    Unfittable,
    /// First observation of this session: record where it starts and inject
    /// nothing. A manifest asserts that files *the agent has read* changed;
    /// one turn after the pointer Rift does not yet know what the agent holds,
    /// and a delta needs two observations before it is a delta rather than a
    /// guess.
    Bootstrap(Vec<String>),
    /// Inject `text` and fold `absorb` into the session's baseline.
    Inject {
        /// The rendered manifest, already inside the turn budget.
        text: String,
        /// The hashes this manifest accounted for — the entries it named, and
        /// the entries its "(N more not shown)" counter stood for. An entry
        /// the push guard **withheld** is not here: it was counted, and a
        /// count is not something the agent was told.
        absorb: Vec<String>,
        /// The [`NamedHold`]s of every entry the frame actually
        /// **named**, which is a strictly smaller list than `absorb`: an
        /// entry the guard withheld or the budget dropped reached the model
        /// as a count, and a count tells it nothing about a file.
        named: Vec<NamedHold>,
    },
}

/// The manifest that fits, and which of its caller's rows it reported.
///
/// Two facts, kept apart because they answer different questions. `named` is
/// what the frame actually *said* — the entries the push guard let through —
/// and drives what a window is recorded as holding. `kept` is where the
/// budget cut the list, so everything from it onward reached the agent as the
/// "(N more not shown)" counter, which the guide treats as a report in its own
/// right. Their union is what the turn accounted for; the gap between them is
/// exactly the withheld entries.
pub(super) struct FittedManifest {
    text: String,
    named: Vec<usize>,
    pub(super) kept: usize,
}

impl FittedManifest {
    /// The indices of every row this manifest reported, of `total` rows:
    /// the ones it rendered, plus the ones its closing counter stood for.
    fn reported(&self, total: usize) -> Vec<usize> {
        let mut out = self.named.clone();
        out.extend(self.kept..total);
        out
    }
}

/// The turn's decision table.
///
/// What a manifest absorbs is everything it **reported**, which is more than
/// what it named and less than what it looked at. The closing counter is
/// itself a report — "3 more not shown" tells the agent the delta is bigger
/// than the list — and the hint names the verb that returns the rest in full,
/// so absorbing only the named entries would re-announce the remainder every
/// turn, spending the agent's context to repeat itself.
///
/// **A withheld entry is not a report.** The push guard refuses a span it
/// cannot render as a neutral path, and the frame counts it — `2 withheld as
/// unsafe to inject`, naming nothing. The agent is told a number; it is not
/// told which file, and no verb it can run will lead it back to one. Folding
/// that entry's revision into the baseline would record the file as accounted
/// for and the next turn would say nothing about it, so the delta would be
/// owed forever and silently. Each row therefore carries its own
/// [`ManifestRow::accounts`], and only the rows the frame reported — rendered,
/// or counted into "more not shown" — contribute theirs.
///
/// Nothing is absorbed when nothing is injected, so a delta Rift could not fit
/// is still owed rather than quietly forgotten — which is why that state is
/// [`TurnDecision::Unfittable`] and not [`TurnDecision::Silent`]. Owing it in
/// the baseline is only half of it: the caller must also decline to *account*
/// for a projection it never announced, or the debt survives one turn and the
/// skip survives forever (`an_unfittable_delta_records_no_stamp_and_stays_owed`).
///
/// **Announce-once is decided, not accidental** (settled 2026-08-06). The
/// question raised against this design was whether a file the
/// budget dropped is *lost* to the agent once it has been counted. It is not:
/// the counter announces once, the pull hint accompanies every announcement,
/// and the unabsorbed detail stays available in full through `diff-since` for
/// as long as it is a delta. Re-announcing it each turn would be the louder
/// behavior, not the more honest one — it spends the agent's context to repeat
/// a fact it was already given and can still fetch.
///
/// `bootstrap_absorb` is the one list that is not derived from the rows: the
/// first observation of a session records where it *starts*, which is not an
/// announcement at all and so has nothing to withhold.
pub(super) fn decide_turn(
    baseline: Option<&[String]>,
    rows: &[ManifestRow],
    bootstrap_absorb: &[String],
    budget: u32,
) -> TurnDecision {
    if baseline.is_none() {
        return TurnDecision::Bootstrap(bootstrap_absorb.to_vec());
    }
    if rows.is_empty() {
        // The silence property: a known state that did not move is answered
        // with no injection at all, not with an empty manifest. "Did not move"
        // is now the conjunction of both sources — the projection reported no
        // delta against the baseline AND every revision the session holds is
        // still the revision in the checkout — which is a stricter test than
        // the one the property was written against, never a weaker one.
        return TurnDecision::Silent;
    }
    fit_manifest(rows, budget).map_or(TurnDecision::Unfittable, |fitted| {
        let reported = fitted.reported(rows.len());
        TurnDecision::Inject {
            text: fitted.text,
            absorb: reported
                .iter()
                .filter_map(|at| rows.get(*at))
                .flat_map(|row| row.accounts.iter().cloned())
                .collect(),
            // A row with no held revision announces nothing about one: the
            // baseline knows the file moved and the session's own journal does
            // not say what it holds of it.
            named: fitted
                .named
                .iter()
                .filter_map(|at| rows.get(*at))
                .filter(|row| !row.hash.is_empty())
                .map(|row| (row.path.clone(), row.hash.clone()))
                .collect(),
        }
    })
}

/// The largest prefix of `rows` whose rendered manifest fits `budget` tokens,
/// or `None` if not even the frame does.
///
/// The budget is a **ceiling**, measured with `rift-tokens` on the exact string
/// that would be injected — not on an estimate of it. The walk is a plain
/// step-down rather than a binary search because BPE counts are not monotone
/// under concatenation, and at 64 entries the certainty is cheaper than the
/// argument for the shortcut.
///
/// An entry that carries a revision and its changed ranges costs about eight
/// tokens more than a bare path, so a budget carries fewer of them; the
/// remainder is counted in the same clause it always was. What is dropped
/// first is the entries themselves, from the cold end — a row keeps its ranges
/// or does not appear, because an entry stripped of the facts that make it
/// actionable is a path the agent must re-read anyway.
///
/// The hint is derived from the prefix that actually fits, not from the whole
/// list: it names a file the manifest is showing, so an agent that runs it is
/// asking about something it was just told about.
fn fit_manifest(rows: &[ManifestRow], budget: u32) -> Option<FittedManifest> {
    let mut keep = rows.len().min(MAX_MANIFEST_ENTRIES);
    loop {
        let shown = rows.get(..keep).unwrap_or(&[]);
        let omitted = rows.len().saturating_sub(keep);
        let entries: Vec<rift_recall_serve::ManifestEntry<'_>> =
            shown.iter().map(ManifestRow::entry).collect();
        let hint = manifest_hint(shown);
        if let Some((text, named)) =
            rift_recall_serve::render_delta_manifest_naming(&entries, omitted, &hint)
        {
            if rift_tokens::count(&text) <= budget {
                return Some(FittedManifest {
                    text,
                    named,
                    kept: keep,
                });
            }
        }
        if keep == 0 {
            return None;
        }
        keep = keep.saturating_sub(1);
    }
}

/// The pull command the manifest names, spelled for the first entry that can
/// carry one: a path the guard will render and a revision the pull verb can
/// resolve a basis from.
///
/// A manifest whose entries are all baseline-only has no revision to offer, and
/// is answered with the verb that needs none — today's hint, unchanged.
fn manifest_hint(rows: &[ManifestRow]) -> String {
    let named = rows.iter().find(|row| {
        row.hash.len() >= 16 && rift_recall_serve::neutral_pushed_span(&row.path).is_some()
    });
    named.map_or_else(
        || crate::agent_help::manifest_pull_hint("", ""),
        |row| crate::agent_help::manifest_pull_hint(&row.path, &row.hash),
    )
}

/// The delta the projection reports against `known`: changed paths with their
/// new content hashes, in the journal's own rank order, plus whether that list
/// is the whole delta ([`delta_is_complete`]). `None` means the question could
/// not be asked, which is answered like any other unavailable answer — with
/// silence.
pub(super) async fn changed_since(
    store: &rift_store::SqliteStore,
    repo: &std::path::Path,
    scope: &rift_recall_types::Scope,
    ancestry: &dyn rift_recall_types::AncestryOracle,
    known: &[String],
) -> Option<(Vec<(String, String)>, bool)> {
    // Through the same argv → params → decode path the skill surface uses, so
    // the push tier is a caller of the one dispatcher rather than a second
    // implementation of it.
    let verb = rift_recall_serve::ToolName::DiffSince.verb_name();
    let params = serde_json::json!({ "known_hashes": known, "token_budget": 0 });
    let (tool, req) = rift_recall_serve::decode_verb(verb, &params).ok()?;
    // No provenance on this path, and the call graph is what says so: the
    // push tier composes a manifest while a person waits, and the story behind
    // a delta is a join across two projections. The manifest names the change;
    // the frame it renders names the command that tells the rest.
    let resp = crate::memory::tool_call_raw(
        store,
        repo,
        scope,
        ancestry,
        &rift_recall_types::NoProvenance,
        tool,
        &req,
    )
    .await
    .ok()?;
    let changed = resp
        .payload
        .get("changed")?
        .as_array()?
        .iter()
        .filter_map(|e| {
            let path = e.get("path")?.as_str()?.to_owned();
            let hash = e.get("new_sha256")?.as_str()?.to_owned();
            Some((path, hash))
        })
        .collect();
    Some((changed, delta_is_complete(&resp.payload)))
}

/// Whether a `diff_since` payload reported the **whole** delta, or a prefix of
/// it that its own response cap truncated.
///
/// `diff_since` fits its payload to a token cap by dropping droppable entries
/// from the tail of `[changed…, unchanged…]`, so unchanged paths go first. Two
/// facts follow and together they decide it from the response alone: if any
/// `unchanged` entry survived, every `changed` entry did; and if `omitted` is
/// zero, nothing was dropped at all. Anything else means changed entries were
/// left behind — a delta this turn did not see, let alone account for.
fn delta_is_complete(payload: &serde_json::Value) -> bool {
    let omitted = payload
        .get("omitted")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let unchanged_shown = payload
        .get("unchanged")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    omitted == 0 || unchanged_shown > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const HINT: &str = "rift recall diff-since --json";

    /// `n` changed entries with distinct, well-formed content hashes.
    fn delta(n: usize) -> Vec<(String, String)> {
        (0..n)
            .map(|i| (format!("crates/c{i}/src/lib.rs"), format!("{i:064x}")))
            .collect()
    }

    fn hashes(d: &[(String, String)]) -> Vec<String> {
        d.iter().map(|(_, h)| h.clone()).collect()
    }

    /// The rows a projection-reported delta makes: a path, the revision the
    /// checkout has, and nothing the session's own reads had to say — the
    /// shape every entry had before entries carried a revision.
    fn rows(d: &[(String, String)]) -> Vec<ManifestRow> {
        d.iter()
            .map(|(path, current)| ManifestRow::from_baseline(path, current))
            .collect()
    }

    /// The decision table, stated as a table. Each row is a state the turn can
    /// be in, and the two that inject nothing are the ones worth being exact
    /// about: a session with no baseline yet, and a baseline that did not move.
    #[test]
    fn the_turn_decision_table_holds() {
        let d = delta(3);
        // No baseline: record where the session starts, say nothing. A manifest
        // claims files *the agent has read* changed, and Rift does not know
        // that yet.
        assert_eq!(
            decide_turn(None, &rows(&d), &hashes(&d), 400),
            TurnDecision::Bootstrap(hashes(&d))
        );
        // Same again with an empty delta — the bootstrap is about the missing
        // baseline, not about the delta's size.
        assert_eq!(
            decide_turn(None, &[], &[], 400),
            TurnDecision::Bootstrap(vec![])
        );
        // The silence property: a known state that did not move. This is the
        // ONE state the injector is authorized to stay silent for.
        assert_eq!(decide_turn(Some(&[]), &[], &[], 400), TurnDecision::Silent);
        // A delta that no manifest can carry is a different state with the
        // same visible behaviour, and it must be a different value — what the
        // caller may record about it is the opposite (see
        // `an_unfittable_delta_records_no_stamp_and_stays_owed`).
        assert_eq!(
            decide_turn(Some(&[]), &rows(&d), &hashes(&d), 0),
            TurnDecision::Unfittable,
            "a budget of zero cannot carry a delta"
        );
        assert_eq!(
            decide_turn(Some(&[]), &rows(&d), &hashes(&d), 5),
            TurnDecision::Unfittable,
            "nor can any budget under the bare frame's own cost"
        );
        // …and an empty delta is Silent at every budget, including the ones
        // that could carry nothing: it is the delta that decides this, not the
        // ceiling.
        for budget in [0, 5, 400] {
            assert_eq!(
                decide_turn(Some(&[]), &[], &[], budget),
                TurnDecision::Silent,
                "budget {budget}"
            );
        }
        // A real delta against a real baseline: inject, and absorb everything
        // the manifest accounted for.
        let TurnDecision::Inject { text, absorb, .. } =
            decide_turn(Some(&[]), &rows(&d), &hashes(&d), 400)
        else {
            panic!("a delta against a baseline must inject");
        };
        assert_eq!(absorb, hashes(&d));
        assert!(text.contains("crates/c0/src/lib.rs"), "{text}");
        assert!(text.contains(HINT), "{text}");
        assert!(
            !text.contains("more not shown"),
            "nothing was dropped: {text}"
        );
    }

    /// The silence property in the form the PRD gate names it: a byte-identical
    /// known state produces no injection, however many turns pass over it.
    #[test]
    fn an_unmoved_known_state_never_injects() {
        let d = delta(2);
        let TurnDecision::Inject { absorb, .. } =
            decide_turn(Some(&[]), &rows(&d), &hashes(&d), 400)
        else {
            panic!("the first delta injects");
        };
        // Next turn the agent holds those hashes, so the projection reports no
        // change and the manifest is silent — for every later turn too.
        for _ in 0..5 {
            assert_eq!(
                decide_turn(Some(&absorb), &[], &[], 400),
                TurnDecision::Silent
            );
        }
    }

    /// **A withheld entry is counted, never accounted for.**
    ///
    /// The push guard refuses a path it cannot render as a neutral span, and
    /// the frame says so as a number naming nothing. The agent is therefore
    /// not told which file moved and has no verb that would lead it to one, so
    /// folding that revision into the baseline would mark it announced and the
    /// delta would be owed in silence from then on. It stays owed instead: the
    /// next turn, against the baseline this one wrote, injects it again.
    #[test]
    fn a_withheld_entry_is_counted_and_stays_owed() {
        let shown = ("crates/c0/src/lib.rs".to_owned(), format!("{:064x}", 1));
        // Nothing hostile: a path that is simply not ASCII, which is the case
        // the guard's own docs name and the one a real repository hits.
        let withheld = (
            "crates/caf\u{e9}/src/lib.rs".to_owned(),
            format!("{:064x}", 2),
        );
        let rows = vec![
            ManifestRow::from_baseline(&shown.0, &shown.1),
            ManifestRow::from_baseline(&withheld.0, &withheld.1),
        ];

        let TurnDecision::Inject { text, absorb, .. } = decide_turn(Some(&[]), &rows, &[], 400)
        else {
            panic!("a delta against a baseline must inject");
        };
        assert!(text.contains("withheld as unsafe to inject"), "{text}");
        assert!(text.contains(&shown.0), "{text}");
        assert!(
            !text.contains("caf"),
            "a withheld path is counted, never quoted: {text}"
        );
        assert_eq!(
            absorb,
            vec![shown.1.clone()],
            "only the entry the agent was actually told about is accounted for"
        );

        // The consequence, stated as the next turn: the withheld revision is
        // still a delta against the baseline this turn wrote, so it is
        // announced again rather than swallowed.
        assert!(matches!(
            decide_turn(Some(&absorb), &rows, &[], 400),
            TurnDecision::Inject { .. }
        ));
    }

    /// The other side of the same rule: what the closing counter stood for IS
    /// reported, so it is absorbed. The counter tells the agent the delta is
    /// bigger than the list and the hint names the verb that returns the rest;
    /// re-announcing it every turn would spend its context to repeat itself.
    #[test]
    fn what_the_counter_stood_for_is_still_accounted_for() {
        let d = delta(30);
        let rows = rows(&d);
        let fitted = fit_manifest(&rows, 120).expect("a small manifest fits 120 tokens");
        assert!(fitted.kept < rows.len(), "the budget dropped entries");
        let TurnDecision::Inject { absorb, .. } = decide_turn(Some(&[]), &rows, &[], 120) else {
            panic!("a delta against a baseline must inject");
        };
        let mut expected = hashes(&d);
        expected.sort();
        let mut got = absorb;
        got.sort();
        assert_eq!(got, expected, "named and counted alike are accounted for");
    }

    /// The budget is a **ceiling**, not a target: for any entry list and any
    /// budget, what gets injected is measured by the real tokenizer and is
    /// never over it — and when nothing fits, nothing is injected.
    #[test]
    fn a_manifest_never_exceeds_the_turn_budget() {
        use proptest::prelude::*;

        proptest!(|(n in 0usize..40, budget in 0u32..500)| {
            let d = delta(n);
            if let Some(fitted) = fit_manifest(&rows(&d), budget) {
                let cost = rift_tokens::count(&fitted.text);
                prop_assert!(cost <= budget, "manifest cost {cost} over budget {budget}");
            }
        });
    }

    /// Dropping from the tail is what the budget buys, and it is reported: the
    /// entries that did not fit are counted, never silently lost.
    #[test]
    fn what_the_budget_drops_is_counted() {
        let d = delta(30);
        let rows = rows(&d);
        let fitted = fit_manifest(&rows, 120).expect("a small manifest fits 120 tokens");
        let text = fitted.text;
        assert!(rift_tokens::count(&text) <= 120);
        assert!(text.contains("more not shown"), "{text}");
        assert!(
            text.contains("crates/c0/src/lib.rs"),
            "rank order is kept: {text}"
        );
        // A budget too small for even the frame injects nothing at all — and
        // the boundary is MEASURED rather than asserted, because the range of
        // budgets that reach that state is exactly what makes it reachable in
        // production. Below the bare frame's own cost, nothing fits; at it,
        // the counter alone does.
        assert!(fit_manifest(&rows, 1).is_none());
        let frame = rift_recall_serve::render_delta_manifest(&[], rows.len(), HINT)
            .expect("a counter with no entries is still a manifest");
        let floor = rift_tokens::count(&frame);
        assert!((2..=200).contains(&floor), "frame floor {floor}");
        assert!(
            fit_manifest(&rows, floor - 1).is_none(),
            "one token under the frame carries nothing"
        );
        let at_floor = fit_manifest(&rows, floor).expect("the frame alone fits its own cost");
        assert_eq!(
            at_floor.text, frame,
            "and at the floor exactly the frame fits"
        );
        // …and the entries behind that counter are still REPORTED: the frame
        // said how many there were, which is what the announce-once rule
        // treats as telling the agent.
        assert_eq!(at_floor.kept, 0);
        assert_eq!(at_floor.reported(rows.len()).len(), rows.len());
    }

    /// The completeness predicate, over the shapes `diff_since` can return.
    ///
    /// It is decided from the response alone, and the reason it can be is the
    /// drop order: unchanged paths are sacrificed before changed ones.
    #[test]
    fn a_truncated_delta_is_recognised_from_the_response_alone() {
        let c = |v: serde_json::Value| delta_is_complete(&v);
        assert!(
            c(serde_json::json!({ "changed": [1, 2], "unchanged": [], "omitted": 0 })),
            "nothing omitted is complete"
        );
        assert!(
            c(serde_json::json!({ "changed": [1], "unchanged": [2], "omitted": 3 })),
            "an unchanged entry survived, so every changed entry did"
        );
        assert!(
            !c(serde_json::json!({ "changed": [1], "unchanged": [], "omitted": 1 })),
            "drops with no unchanged survivor ate into the changed list"
        );
        assert!(
            !c(serde_json::json!({ "changed": [], "unchanged": [], "omitted": 9 })),
            "a cap that kept nothing is the extreme case, not an exception"
        );
        // A payload missing the fields fully is treated as complete: that is
        // the shape a non-truncating tool returns, and the precondition's job
        // is not to invent a backlog either.
        assert!(c(serde_json::json!({ "changed": [] })));
    }
}
