//! The "by worktree" axis: a family figure decomposed by member, and the
//! aligned table both modals draw it as.

use super::format::fmt_count;
use super::tokens::TokensView;
use super::Col3;

/// One member's slice of a family figure.
///
/// The family total is the default everywhere; this is the row that makes it
/// auditable. The slice is a **count**, not a rate: it is never tweened and
/// never re-derived at draw time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorktreeSlice {
    /// The member's admin name (`main` for the repository's main work tree).
    ///
    /// `None` is **unknown** — a session with no `session_worktree`
    /// observation, which is every session captured before worktree provenance. Surfaces
    /// print `unknown` for it, never `main` and never blank (worktree provenance);
    /// Sealed shards are never rewritten, so the history that would have named
    /// it can never be back-filled.
    pub name: Option<String>,
    /// The slice's tokens.
    pub tokens: u64,
}

/// A family figure decomposed by worktree — the "by worktree" axis
/// (the family view).
///
/// It is carried **inside** the view-model whose total it decomposes, so a
/// surface can never pair one read's total with another read's rows: the two
/// are computed in one pass over one set of facts, and
/// [`Self::total`] is that same total by construction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorktreeAxis {
    /// One row per observed member, plus the `unknown` row when any fact
    /// carries no observation. Ordered largest first; equal rows put a named
    /// member before `unknown` and then sort by name — a total order, so the
    /// disclosure is a golden and not a hash walk.
    pub rows: Vec<WorktreeSlice>,
    /// Tokens that carry no session key **at all**, and so belong to the
    /// family and to no member.
    ///
    /// This is not "unknown": an unknown row is a session whose worktree was
    /// never observed, while this is a fact that has no session to observe.
    /// Recall serves are the live case — no serve relation records a session
    /// (`memd_audit_event`, `serve_call_event`, `serve_session_event` hold
    /// none), so the savings figure is the family's and the modal says so.
    pub family_only: u64,
}

impl WorktreeAxis {
    /// The decomposed total: every row plus the family-only remainder. Equal
    /// to the figure this axis decomposes — the property the feed's
    /// `the_breakdown_sums_to_the_family_total` pins.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.rows
            .iter()
            .map(|r| r.tokens)
            .fold(self.family_only, u64::saturating_add)
    }

    /// Whether there is nothing to disclose (no rows and no family-only
    /// remainder) — the axis of a figure that does not exist yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.family_only == 0
    }
}

/// worktree provenance's word for a fact whose worktree was never observed. Every
/// surface prints this, never `main` and never a blank.
pub const UNKNOWN_WORKTREE: &str = "unknown";

/// The label for a fact that carries no session key at all, and so belongs to
/// the family and to no member.
const FAMILY_ONLY: &str = "family-only";

/// The axis's rows as aligned `name  count tok` lines, `unknown` and
/// `family-only` spelled as themselves. Shared by both surfaces that carry
/// the axis, so one table reads the same way in each.
fn worktree_rows(axis: &WorktreeAxis) -> Vec<String> {
    let mut names: Vec<String> = axis
        .rows
        .iter()
        .map(|s| {
            s.name
                .clone()
                .unwrap_or_else(|| UNKNOWN_WORKTREE.to_owned())
        })
        .collect();
    let mut counts: Vec<String> = axis.rows.iter().map(|s| fmt_count(s.tokens)).collect();
    if axis.family_only > 0 {
        names.push(FAMILY_ONLY.to_owned());
        counts.push(fmt_count(axis.family_only));
    }
    let name_w = names.iter().map(|n| n.chars().count()).max().unwrap_or(0);
    let tok_w = counts.iter().map(|c| c.chars().count()).max().unwrap_or(0);
    names
        .iter()
        .zip(&counts)
        .map(|(name, count)| format!("{name:<name_w$}  {count:>tok_w$} tok"))
        .collect()
}

/// The Tokens detail's "by worktree" body — the family
/// total, one row per member, and the rule that put each fact in its row.
///
/// Pure over the token view-model: the rows were computed by the data plane
/// in the same pass as the total they sum to, so this function assembles
/// lines and never divides anything. Honest absence has its own words — a
/// total with no decomposition is said, never drawn as an empty table.
#[must_use]
pub fn worktree_axis_lines(col3: &Col3) -> Vec<String> {
    let TokensView::Data(d) = &col3.tokens else {
        return vec![
            "no token facts in scope yet — there is no total to".to_owned(),
            "decompose.".to_owned(),
        ];
    };
    let axis = &d.by_worktree;
    if axis.is_empty() {
        return vec![
            "no worktree facts in scope: this projection records".to_owned(),
            "no session for the token history it holds, so the".to_owned(),
            "total cannot be split.".to_owned(),
        ];
    }
    let mut lines = vec![
        format!("family total  {} tok", fmt_count(axis.total())),
        String::new(),
    ];
    lines.extend(worktree_rows(axis));
    lines.push(String::new());
    lines.push("each session counts under the LAST worktree it was".to_owned());
    lines.push("observed in; `unknown` is a session with no".to_owned());
    lines.push("observation at all — history captured before".to_owned());
    lines.push("worktree provenance, and sealed history is".to_owned());
    lines.push("never relabelled.".to_owned());
    lines
}

/// The savings modal's "by worktree" block (the family view).
///
/// Every avoided token lands under `family-only` today, and the modal says
/// exactly why: no serve relation records the session that was served
/// (`memd_audit_event`, `serve_call_event` and `serve_session_event` hold no
/// session key), so no serve can be attributed to a member without inventing
/// the attribution. The wording follows the data: if a future relation does
/// carry the key, member rows appear and the family-only sentence goes with
/// the empty remainder.
pub(super) fn savings_worktree_lines(axis: &WorktreeAxis) -> Vec<String> {
    if axis.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![String::new(), "by worktree".to_owned()];
    lines.extend(worktree_rows(axis));
    if axis.rows.is_empty() {
        lines.push("no serve relation records the session that was".to_owned());
        lines.push("served, so no serve can be attributed to one".to_owned());
        lines.push("member — this figure is the family's.".to_owned());
    } else {
        lines.push("`unknown` is a session with no worktree".to_owned());
        lines.push("observation; `family-only` is a serve with no".to_owned());
        lines.push("session key at all (the family view).".to_owned());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::fixtures::tokens_col3;

    /// the family view + worktree provenance: the Tokens detail's body names each
    /// member and says what `unknown` means. Honest absence has its own
    /// words — an axis with nothing in it draws no empty table.
    #[test]
    fn the_tokens_detail_body_discloses_the_rule_that_put_each_fact_in_its_row() {
        let mut col3 = tokens_col3(1_115.0);
        if let TokensView::Data(d) = &mut col3.tokens {
            d.by_worktree = WorktreeAxis {
                rows: vec![
                    WorktreeSlice {
                        name: Some("main".to_owned()),
                        tokens: 750,
                    },
                    WorktreeSlice {
                        name: Some("swt-d".to_owned()),
                        tokens: 300,
                    },
                    WorktreeSlice {
                        name: None,
                        tokens: 65,
                    },
                ],
                family_only: 0,
            };
        }
        let body = worktree_axis_lines(&col3).join("\n");
        assert!(body.contains("family total  1,115 tok"), "{body}");
        assert!(body.contains("main     750 tok"), "{body}");
        assert!(body.contains("swt-d    300 tok"), "{body}");
        assert!(body.contains("unknown   65 tok"), "{body}");
        assert!(
            body.contains("the LAST worktree it was"),
            "the rule is stated, not implied: {body}"
        );
        assert!(body.contains("never relabelled."), "{body}");
        // Absence, in both of its shapes.
        assert!(worktree_axis_lines(&Col3::default())
            .join(" ")
            .contains("no token facts in scope"));
        let mut bare = tokens_col3(5.0);
        if let TokensView::Data(d) = &mut bare.tokens {
            d.by_worktree = WorktreeAxis::default();
        }
        assert!(worktree_axis_lines(&bare)
            .join(" ")
            .contains("no worktree facts in scope"));
    }

    /// The table renders the axis's OWN order (the feed establishes it — see
    /// `feeds::tokens::worktree_axis`; a renderer that re-sorted would hide a
    /// producer that had stopped), aligns on the widest name and the widest
    /// count, and spells an absent name `unknown` — never `main`, never
    /// blank (worktree provenance).
    #[test]
    fn the_axis_table_aligns_and_names_unknown_as_unknown() {
        let axis = WorktreeAxis {
            rows: vec![
                WorktreeSlice {
                    name: Some("swt-d".to_owned()),
                    tokens: 900_000,
                },
                WorktreeSlice {
                    name: Some("main".to_owned()),
                    tokens: 12_345,
                },
                WorktreeSlice {
                    name: None,
                    tokens: 1_500,
                },
            ],
            family_only: 0,
        };
        let rows = worktree_rows(&axis);
        assert_eq!(
            rows,
            vec![
                "swt-d    900,000 tok".to_owned(),
                "main      12,345 tok".to_owned(),
                "unknown    1,500 tok".to_owned(),
            ],
            "aligned, in the axis's order, `unknown` spelled out"
        );
        // The word is worktree provenance's, and the absent name never borrows the
        // main work tree's.
        assert!(rows.iter().any(|r| r.starts_with("unknown")));
        assert_eq!(
            rows.iter().filter(|r| r.starts_with("main")).count(),
            1,
            "only the row that IS main says main"
        );
    }

    /// [`WorktreeAxis::total`] is the sum it claims to be, and it saturates
    /// rather than wrapping on a corrupt projection's absurd counts.
    #[test]
    fn the_axis_total_is_its_rows_plus_the_family_remainder() {
        let axis = WorktreeAxis {
            rows: vec![
                WorktreeSlice {
                    name: Some("a".to_owned()),
                    tokens: 7,
                },
                WorktreeSlice {
                    name: None,
                    tokens: 3,
                },
            ],
            family_only: 5,
        };
        assert_eq!(axis.total(), 15);
        let absurd = WorktreeAxis {
            rows: vec![
                WorktreeSlice {
                    name: Some("a".to_owned()),
                    tokens: u64::MAX,
                },
                WorktreeSlice {
                    name: Some("b".to_owned()),
                    tokens: u64::MAX,
                },
            ],
            family_only: 1,
        };
        assert_eq!(absurd.total(), u64::MAX, "saturates, never wraps");
    }
}
