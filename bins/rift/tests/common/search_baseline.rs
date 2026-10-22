//! The pinned ceilings and the baseline formula every search figure in
//! `search_savings` is priced against: the tool-field artifact the caps are
//! read off, the ceiling each shape is cut at, and the pool and claim a
//! scripted search is held to.
//!
//! Kept apart from the script so the formula a reader checks against the
//! product's own function sits on one page, and the suite that runs the
//! session has room to grow.

use super::NameCase;

/// The committed artifact the hundred-file cap is read off.
pub(super) const TOOL_FIELD_PIN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../crates/rift-agent-claude-code/fixtures/claude-code-tool-field-surface.md"
);

/// What the client the call was made on would have delivered, before Rift
/// answered it.
///
/// Two shapes, two ceilings, both pinned in this repository's own committed
/// artifacts, and they are nothing like each other: the `Glob` tool stops
/// after a hundred files, and a shell command's result arrives inline up to
/// roughly thirty thousand characters — so a `find` over a large subtree puts
/// forty times a glob's worth of paths in front of the model. The product
/// prices each shape at its own ceiling, and this fixture holds it to that
/// figure search by search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Ceiling {
    /// The `Glob` tool's own documented file cap.
    Files(usize),
    /// A shell result's own documented inline character ceiling.
    InlineChars(usize),
}

impl Ceiling {
    /// The ceiling for one shape on one family.
    ///
    /// A glob is bounded by its own tool on every family. A listing is bounded
    /// by how much of a command's result the client reads back inline, and one
    /// client's page states that figure: Claude Code's. Codex and Antigravity
    /// state none anywhere in this tree and take the same figure by a
    /// deliberate consistency choice — one number across the alpha clients rather
    /// than three — which is an assumption about them and is written down as
    /// one both here and beside the constant the product reads.
    pub(super) fn of(_family: &str, lever: &str) -> Self {
        match lever {
            "listing answer" => Self::InlineChars(NATIVE_INLINE_CHARS),
            _ => Self::Files(rift_recall_serve::NATIVE_LISTING_PATHS),
        }
    }
}

/// How many characters of a command's result a client reads back inline.
///
/// Pinned, like the file cap beside it, and read back out of the same
/// committed artifact by [`the_shell_result_ceiling_is_the_clients_own_pinned_figure`](super::the_shell_result_ceiling_is_the_clients_own_pinned_figure).
pub(super) const NATIVE_INLINE_CHARS: usize = 30_000;

/// **The pool.** What the model would have received had Rift answered nothing:
/// the paths of the listing, one per line, cut at the ceiling the client this
/// call was made on documents.
///
/// Repository-relative, where the native call prints absolute paths, and
/// without the truncation notice the tool would have added — both of which
/// make this smaller than what really arrives.
pub(super) fn pool_of(case: &NameCase) -> u32 {
    match Ceiling::of(case.family, case.lever) {
        // Faithful where the fixture knows the order, conservative where it
        // cannot: `Glob` prints the hundred most recently modified files, and
        // a corpus committed in one go has nothing to tell them apart — so the
        // hundred priced here are the cheapest hundred, which is the least the
        // model could have received.
        Ceiling::Files(cap) => glob_baseline(&cheapest_head(&case.priced, cap)),
        // A shell listing has an order — the sorted one the product prints —
        // so this fills the client's character budget in that same order
        // rather than choosing one.
        Ceiling::InlineChars(cap) => {
            let mut sorted = case.priced.clone();
            sorted.sort();
            let mut listing = String::new();
            for path in sorted {
                if listing.len() + path.len() + 1 > cap {
                    break;
                }
                if !listing.is_empty() {
                    listing.push('\n');
                }
                listing.push_str(&path);
            }
            rift_tokens::count(&listing)
        }
    }
}

/// **The claim.** What the product will book for this search, and the most it
/// could book: the same paths, cut at the ceiling this call's own client
/// documents for this call's own shape — which is the same cut [`pool_of`]
/// makes, computed here from the corpus this fixture wrote rather than from
/// the decision the product left behind.
///
/// **A listing is one exact figure.** Its ceiling is a character budget over
/// the whole screened match set, so nothing about the ranking enters it: the
/// claim is the pool, and a product that priced it as a glob — or cut it in
/// some other order — books a different number and this fixture goes red.
///
/// **A glob is one figure wherever the hundred is knowable** — all of them
/// when fewer than a hundred matched, and any hundred of the equal-cost family
/// when more did. It differs only where the ranking chooses a hundred out of
/// thousands, and there the fixture states the cheapest and the dearest
/// hundred and requires the booked figure to lie between them.
pub(super) fn claim_of(case: &NameCase) -> (u32, u32) {
    let cap = rift_recall_serve::NATIVE_LISTING_PATHS;
    if let Ceiling::InlineChars(_) = Ceiling::of(case.family, case.lever) {
        let exact = pool_of(case);
        return (exact, exact);
    }
    if case.priced.len() <= cap || case.exact {
        let exact = glob_baseline(&cheapest_head(&case.priced, cap));
        return (exact, exact);
    }
    let mut by_cost = case.priced.clone();
    by_cost.sort_by_cached_key(|path| (rift_tokens::count(path), path.clone()));
    (
        glob_baseline(&by_cost[..cap]),
        glob_baseline(&by_cost[by_cost.len() - cap..]),
    )
}

/// The product's own price for a glob's listing, spelled once so the six
/// places that ask for it cannot drift apart.
pub(super) fn glob_baseline(paths: &[String]) -> u32 {
    rift_recall_serve::declared_search_baseline(
        false,
        rift_recall_serve::ListingShape::Glob,
        "claude-code",
        paths,
    )
}

/// The `n` cheapest paths of a listing, or all of it: the least the hundred a
/// ranking chose could have cost.
fn cheapest_head(paths: &[String], n: usize) -> Vec<String> {
    let mut by_cost = paths.to_vec();
    by_cost.sort_by_cached_key(|path| (rift_tokens::count(path), path.clone()));
    by_cost.truncate(n);
    by_cost
}

/// Whether the hundred a ranking would have chosen is a hundred this fixture
/// knows the price of: all of them, when fewer than a hundred matched, and
/// any hundred of a family whose names all cost the same when more did.
///
/// Derived rather than declared beside each line of the script, because it is
/// a property of the paths and a script line that claimed otherwise would
/// quietly turn an exact expectation into a range or a range into a lie.
pub(super) fn hundred_is_knowable(priced: &[String]) -> bool {
    if priced.len() <= rift_recall_serve::NATIVE_LISTING_PATHS {
        return true;
    }
    let first = rift_tokens::count(&priced[0]);
    priced.iter().all(|path| rift_tokens::count(path) == first)
}
