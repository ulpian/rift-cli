//! Fitting the map to the budget, and writing down what it announced.
//!
//! The two belong together because the fit decides which entries the frame
//! carried, and an announcement is only ever about an entry the frame carried:
//! what the guard withheld and the budget dropped reached the model as a
//! count, and a count is not a fact about a file.

use super::entries::{recovery_announcements, RecoveryEntry};

/// The characters one token of the boundary budget is worth.
///
/// The renderer fits to characters because the tokenizer is not its crate's;
/// the conversion is made here, once, and then **checked** against the real
/// count below — so this is a starting point for the fit, never the bound
/// itself.
const RECOVERY_CHARS_PER_TOKEN: usize = 4;

/// How many times the fit may narrow its character budget before it gives up
/// and says nothing. Each pass is one render and one tokenizer count over a
/// bounded list, and a fit that has not converged in this many passes is one
/// whose entries do not fit at all.
const MAX_RECOVERY_FITS: usize = 8;

/// The map, fitted to the boundary budget in **tokens**.
///
/// The renderer drops whole entries from the cold end to fit a character
/// budget; the budget that matters is the operator's, in tokens, so the
/// conversion is a starting point and the real tokenizer decides. Each pass
/// narrows the character budget in proportion to how far over it landed, which
/// converges in one or two passes on any real list; a list that has not fitted
/// after a few is one that cannot, and silence is the honest answer.
pub(super) fn fit_recovery_map(
    entries: &[rift_recall_serve::HeldEntry<'_>],
    omitted: usize,
    budget: u32,
) -> Option<(String, Vec<usize>)> {
    let budget_usize = usize::try_from(budget).unwrap_or(usize::MAX);
    let mut chars = budget_usize.saturating_mul(RECOVERY_CHARS_PER_TOKEN);
    for _ in 0..MAX_RECOVERY_FITS {
        let (text, named) = rift_recall_serve::render_recovery_map_naming(entries, omitted, chars)?;
        let counted = rift_tokens::count(&text);
        if counted <= budget {
            return Some((text, named));
        }
        let counted = usize::try_from(counted).unwrap_or(usize::MAX).max(1);
        let narrowed = chars.saturating_mul(budget_usize) / counted;
        if chars == 0 {
            return None;
        }
        chars = narrowed.min(chars.saturating_sub(1));
    }
    None
}

/// Write down what the map told this window it holds.
///
/// Only the entries the frame carried: the ones the guard withheld and the
/// budget dropped reached the model as a count, and a count is not a fact
/// about a file.
pub(super) fn announce_recovery(
    journal: &crate::window_journal::WindowJournal,
    engine: &rift_recall_policy::PolicyEngine<'_>,
    raw: &rift_shadow::hooks::RawHook,
    entries: &[RecoveryEntry],
    named: &[usize],
) {
    announce_holds(
        journal,
        engine,
        raw.turn.unwrap_or_default(),
        u64::try_from(raw.occurred_at_ms).unwrap_or_default(),
        recovery_announcements(entries, named),
    );
}

/// Write down what an injected frame told this window it holds.
///
/// One line per file, and only where the announcement is about **one run of
/// lines**: a held set in two pieces is priced by no single range, and the
/// standing rule for a fact Rift cannot state exactly is that it does not
/// state it at all. The failure direction is a saving nobody books.
///
/// The guard is asked here as well, of every path, and not because a caller is
/// expected to have forgotten it. A hold line is what a later booking claims a
/// saving against, so a line naming a path this repository does not serve
/// would put a denied file into the accounting whatever went wrong upstream —
/// a caller's own filter shifting a position, a frame built from a list nobody
/// filtered. One chokepoint, asked last, so there is no route around it.
///
/// Best-effort throughout — a line that cannot be written is an announcement
/// nothing will be booked against, which costs a figure and never a claim.
pub(in crate::hooks) fn announce_holds<'a>(
    journal: &crate::window_journal::WindowJournal,
    engine: &rift_recall_policy::PolicyEngine<'_>,
    turn: u32,
    at_ms: u64,
    entries: impl Iterator<
        Item = (
            &'a str,
            rift_core::Sha256Id,
            &'a [rift_agent::window_journal::Interval],
        ),
    >,
) {
    for (path, hash, runs) in entries {
        if engine.is_denied_path(path) {
            continue;
        }
        let [range] = runs else { continue };
        let _ = journal.append(&rift_agent::window_journal::Line::Hold {
            turn,
            path: path.to_owned(),
            hash,
            range: *range,
            at_ms,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{recall_repo, CC};
    use super::*;
    use crate::hooks::agent::{boundary_budget, boundary_ceiling};

    /// The budget is a ceiling measured with the real tokenizer, and the fit
    /// pays for it by dropping the cold end of the list into a count — never by
    /// cutting a line in half, and never by exceeding the budget.
    #[test]
    fn the_budget_drops_the_cold_end_into_a_count() {
        let paths: Vec<String> = (0..40)
            .map(|i| format!("src/module-{i:02}/file.rs"))
            .collect();
        let hash = "3f".repeat(32);
        let held = [(1u32, 500u32)];
        let entries: Vec<rift_recall_serve::HeldEntry<'_>> = paths
            .iter()
            .map(|path| rift_recall_serve::HeldEntry {
                path,
                hash: &hash,
                held: &held,
                verdict: rift_recall_serve::HeldVerdict::Unchanged,
            })
            .collect();
        let mut fitted = 0usize;
        for budget in [40u32, 120, 400, 800, 4000] {
            // Silence is a permitted answer — a budget that cannot carry even
            // the frame and one entry buys nothing by carrying half of one —
            // but an overrun never is.
            let Some((text, _)) = fit_recovery_map(&entries, 0, budget) else {
                continue;
            };
            fitted += 1;
            assert!(
                rift_tokens::count(&text) <= budget,
                "{} tokens over a {budget}-token budget",
                rift_tokens::count(&text)
            );
            let shown = text.lines().filter(|l| l.starts_with("- ")).count();
            assert!(shown >= 1, "at least one entry survives: {text}");
            if shown < paths.len() {
                assert!(
                    text.contains("not shown"),
                    "what was dropped is counted: {text}"
                );
            }
        }
        assert!(fitted >= 3, "the ordinary budgets fit a map");
        assert_eq!(
            fit_recovery_map(&entries, 0, 0),
            None,
            "a zero budget is the operator turning the map off"
        );
    }

    /// **A family that declares a bound on its context channel gets text that
    /// is inside it, and it is bought by dropping whole entries.**
    ///
    /// The bound is not the operator's budget and does not replace it: it is a
    /// second ceiling, and the rendering budget is the lower of the two in
    /// either direction. What must never happen is the text going out over the
    /// family's number for the client to deal with — the client keeps a head
    /// and a tail and spills the middle to a file, so the model reads a frame
    /// cut in two places instead of a shorter frame that is whole.
    #[test]
    fn text_is_fitted_to_the_declared_limit_by_dropping_entries() {
        let declared = rift_agent::respond::DECLARED_CONTEXT_LIMIT_TOKENS;
        assert_eq!(
            boundary_ceiling(&rift_core::AgentId::CODEX, "UserPromptSubmit"),
            Some(declared),
            "the number the installer wrote into this family's hook file"
        );
        assert_eq!(
            boundary_ceiling(&CC, "UserPromptSubmit"),
            None,
            "a family that bounds the channel by nothing bounds it by nothing"
        );
        // Lowered, never raised — in both directions, and unbounded where the
        // family declares no ceiling at all.
        assert_eq!(boundary_budget(400, Some(declared)), 400);
        assert_eq!(boundary_budget(declared + 2_800, Some(declared)), declared);
        assert_eq!(boundary_budget(declared + 2_800, None), declared + 2_800);
        assert_eq!(boundary_budget(0, Some(declared)), 0, "off stays off");

        // Twenty entries: a list the declared bound carries whole, so the
        // comparison below is about the bound and not about the list's size.
        let paths: Vec<String> = (0..20)
            .map(|i| format!("src/module-{i:02}/file.rs"))
            .collect();
        let hash = "3f".repeat(32);
        let held = [(1u32, 500u32)];
        let entries: Vec<rift_recall_serve::HeldEntry<'_>> = paths
            .iter()
            .map(|path| rift_recall_serve::HeldEntry {
                path,
                hash: &hash,
                held: &held,
                verdict: rift_recall_serve::HeldVerdict::Unchanged,
            })
            .collect();
        let whole_entries = |text: &str| -> usize {
            for line in text.lines().filter(|line| line.starts_with("- ")) {
                assert!(
                    paths.iter().any(|path| line.contains(path.as_str()))
                        && line.contains(&hash[..12]),
                    "an entry is named whole or not at all: {line}"
                );
            }
            text.lines().filter(|line| line.starts_with("- ")).count()
        };

        // A generous operator budget, bounded only by what the family declared.
        let (roomy, _) = fit_recovery_map(&entries, 0, boundary_budget(4_000, Some(declared)))
            .expect("the declared bound carries a map");
        assert!(rift_tokens::count(&roomy) <= declared, "{roomy}");
        let roomy_shown = whole_entries(&roomy);

        // The same list under a family that declared a much smaller one: fewer
        // entries, each of them whole, and the rest are a count.
        let (tight, _) = fit_recovery_map(&entries, 0, boundary_budget(4_000, Some(400)))
            .expect("a small bound still carries a map");
        assert!(rift_tokens::count(&tight) <= 400, "{tight}");
        let tight_shown = whole_entries(&tight);
        assert!(
            tight_shown < roomy_shown,
            "the smaller bound is paid for in entries: {tight_shown} vs {roomy_shown}"
        );
        assert!(
            tight_shown >= 1,
            "and it is paid, not defaulted away: {tight}"
        );
        assert!(
            tight.contains("not shown"),
            "what was dropped is counted: {tight}"
        );
    }

    /// The bound Rift declares for itself is above everything it means to send
    /// through the channel it bounds.
    ///
    /// It is one number written in one place, so the thing that can go wrong is
    /// not disagreement between two copies but a number chosen too small: a
    /// ceiling under a shipped budget would silently shrink that budget on one
    /// family only, and a ceiling under the session pointer would drop the one
    /// injection that is not fitted at all because it has no entries to drop.
    #[test]
    fn the_declared_bound_is_above_every_budget_and_text_it_bounds() {
        let declared = rift_agent::respond::DECLARED_CONTEXT_LIMIT_TOKENS;
        assert!(
            declared >= rift_recall_policy::DEFAULT_RECOVERY_BUDGET,
            "a post-compaction map fits the bound"
        );
        assert!(
            declared >= rift_recall_policy::DEFAULT_INJECTION_TURN_BUDGET,
            "a turn's manifest fits the bound"
        );
        assert!(
            rift_tokens::count(&crate::agent_help::session_pointer()) <= declared,
            "the pointer is a fixed text with nothing to drop, so it must fit"
        );
    }

    /// ★ **No hold line for a guarded path, whatever reached the writer.**
    /// The rule is asked of the one function that writes them, so a caller
    /// that built its list wrongly still cannot put a denied file into the
    /// accounting.
    #[test]
    fn a_hold_line_is_never_written_for_a_denied_path() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = recall_repo(tmp.path());
        std::fs::create_dir_all(repo.join(".rift").join("policies")).unwrap();
        std::fs::write(
            repo.join(".rift").join("policies").join("serve.toml"),
            "denied_paths = [\"private/**\"]\n",
        )
        .unwrap();
        let cfg = rift_config::EffectiveConfig::load(&repo.join(".rift")).unwrap();
        let engine = rift_recall_policy::PolicyEngine::new(&cfg)
            .with_serve_policy(rift_recall_policy::ServePolicy::load(&repo.join(".rift")));
        let journal = crate::window_journal::WindowJournal::open(&repo, "s-guard", None);
        let hash =
            rift_core::Sha256Id::parse_hex(&rift_shadow::sha256_hex(&[b"x".as_slice()])).unwrap();
        let runs = [rift_agent::window_journal::Interval::new(1, 30).unwrap()];
        announce_holds(
            &journal,
            &engine,
            1,
            10,
            [
                ("private/secret.txt", hash, runs.as_slice()),
                ("src.rs", hash, runs.as_slice()),
            ]
            .into_iter(),
        );
        let lines = journal.load().journal;
        assert_eq!(lines.standing_hold("private/secret.txt"), None);
        assert!(lines.standing_hold("src.rs").is_some());
    }
}
