//! The **declared** native cost of what a verb stands in for: the read of a
//! document, or the listing a name search replaced.
//!
//! Declared, never measured — the read or the listing this prices never ran,
//! so there is no second output to count — and every figure here errs low, in
//! the one direction a number Rift claims credit for is allowed to err. The
//! ceilings are read off committed artifacts, not remembered; the one
//! assumption among them ([`SearchCeiling::of`]) is stated where it is made.

use rift_recall_policy::PolicyEngine;
use rift_recall_types::path_is_safe;

/// How many characters of a document a native read would have put in front of
/// the model before the client cut it off.
///
/// The character half of the client's own ceiling, pinned rather than rounded:
/// a terminal result is written to a file and replaced by a pointer past
/// ~30,000 characters (the recorded spike walked the boundary a thousand
/// characters at a time — 29,999 inline, 30,999 as a pointer). It is a
/// ceiling on characters and says nothing about what those characters cost,
/// which is why [`NATIVE_READ_TOKEN_CEILING`] is applied after it and not
/// instead of it.
///
/// **It bounds a shell listing too, and for the same reason.** The pinned page
/// states this for a *command's result*, not for a document — a valid result
/// arrives "Inline up to roughly 30,000 characters", and past that the model
/// is handed a file path and a short preview. So a `find` whose output would
/// overflow it is cut here, which is what makes a listing's ceiling a
/// character count where a glob's is a file count: see [`SearchCeiling`].
pub const NATIVE_INLINE_CHARS: usize = 30_000;

/// The token half of the same ceiling: a `Read` result stops here.
///
/// The two ceilings are not interchangeable, and the corpus is what settled
/// it. A character is worth between roughly a third of a token (lean XML) and
/// a whole one (a compressed container, or dense CJK), so
/// [`NATIVE_INLINE_CHARS`] characters are 5,700 tokens of one document and
/// over 30,000 of another. Whichever binds first is the one a native read
/// would really have paid, so a declared figure is cut by both — and a figure
/// Rift claims credit for has to err low.
pub const NATIVE_READ_TOKEN_CEILING: u32 = 25_000;

/// How many paths a native listing of this repository would have put in front
/// of the model before it truncated.
///
/// The pinned figure, not a round one, and the pin is a committed artifact
/// rather than a memory: `crates/rift-agent-claude-code/fixtures/claude-code-tool-field-surface.md`
/// records the `Glob` tool's own page verbatim, and the sentence this constant
/// is read off says "Results are sorted by modification time and capped at 100
/// files". A fixture whose number moved and a constant that did not would be
/// two claims about one tool, so the fixture is read back and compared with
/// this constant by `the_hundred_file_cap_is_the_tools_own_pinned_figure`.
///
/// It bounds what this verb ranks, and it is the ceiling
/// [`declared_search_baseline`] prices a **glob** at — one of the two, because
/// the other shape a name search comes in is not bounded by a file count at
/// all: see [`SearchCeiling`].
pub const NATIVE_LISTING_PATHS: usize = 100;

/// Which native call an answered name search stood in for.
///
/// The axis a declared listing baseline is priced on, because the two shapes
/// are cut off by two different things: a glob tool stops itself after a fixed
/// number of files, and a shell listing prints every match and is cut by how
/// much of a command's result the client reads back inline. Pricing both at
/// the file cap prices the second one as though the first tool had run it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListingShape {
    /// A glob tool call: the tool itself stops after
    /// [`NATIVE_LISTING_PATHS`] files.
    Glob,
    /// A shell or directory listing: the command prints every match, and the
    /// client stops reading its result at its own inline character ceiling.
    Listing,
}

/// The bound one [`ListingShape`] is cut off by on one client.
///
/// Two units, deliberately, because the two pins are in two units: a file
/// count and a character count. A ceiling is never inferred, only read off a
/// committed artifact — the same discipline that governs what may be written
/// on a channel — so a shape whose ceiling this tree does not pin takes the
/// one the owner ruled it takes, and says so where a reader can see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchCeiling {
    /// The glob tool's own documented file cap.
    Files(usize),
    /// The client's own documented inline character ceiling for a result.
    InlineChars(usize),
}

/// The family a name search that belongs to no client is priced under.
///
/// The `search` verb is an operator's own call and not any client's tool call,
/// so it has no family and no client result ceiling. It is priced as a
/// [`Glob`](ListingShape::Glob), whose ceiling does not depend on the family at
/// all — so this names the absence rather than borrowing some client's name for
/// a call that client never made.
pub const NO_CLIENT_FAMILY: &str = "";

/// The families whose own committed artifact states an inline result ceiling,
/// and the figure it states.
///
/// One entry today. A family that later pins its own joins this table and
/// nothing else changes; [`SearchCeiling::of`] is the only reader.
const PINNED_INLINE_CHARS: [(&str, usize); 1] = [("claude-code", NATIVE_INLINE_CHARS)];

impl SearchCeiling {
    /// The ceiling one shape is bounded by on one family.
    ///
    /// **A glob** is bounded by its own tool, whatever the client's result
    /// ceiling is, so every family gets [`NATIVE_LISTING_PATHS`].
    ///
    /// **A listing** is bounded by how much of a command's result the client
    /// reads back inline. Claude Code's page states that figure and is the one
    /// entry in this module's private per-family pin table. Codex and
    /// Antigravity state none
    /// anywhere in this tree, and by the owner's consistency ruling they are
    /// priced at Claude Code's — the aim being one figure across the alpha
    /// clients rather than three. **That is an assumption, and it is stated
    /// here as one:** it says a shell run by those clients is cut off at
    /// roughly the same size, which nothing in this repository proves. It is
    /// the direction consistency points; if either client publishes its own
    /// ceiling, that number becomes its pin and this assumption stops
    /// applying to it.
    #[must_use]
    pub fn of(family: &str, shape: ListingShape) -> Self {
        match shape {
            ListingShape::Glob => Self::Files(NATIVE_LISTING_PATHS),
            ListingShape::Listing => Self::InlineChars(
                PINNED_INLINE_CHARS
                    .iter()
                    .find(|(pinned, _)| *pinned == family)
                    .map_or(NATIVE_INLINE_CHARS, |(_, chars)| *chars),
            ),
        }
    }
}

/// The **declared** native cost of reading one document, in tokens.
///
/// Declared, not measured, and the distinction is the whole point: the read
/// this figure prices never happened, so there is no second reading to count.
/// What is counted is the bytes as a native read would have presented them —
/// lossy UTF-8, because that is what a client hands a model when it opens a
/// container it cannot parse — cut at [`NATIVE_INLINE_CHARS`] characters and
/// then at [`NATIVE_READ_TOKEN_CEILING`] tokens, because a client that stops
/// at either stops at whichever it reaches first. Every row that carries the
/// result also carries [`TAG_DECLARED_BASELINE`](crate::TAG_DECLARED_BASELINE), so a reader can never
/// mistake it for a measurement.
///
/// **PDF declares nothing.** A PDF is not presented as text at all: the client
/// renders its pages and the model sees images, so the honest figure is pages
/// times a vision-token rate, and no artifact in this tree pins that rate. A
/// declared zero is the conservative answer — the verb still serves the
/// sidecar, and claims nothing for having done so — and it is the one the
/// document fixture replaces when the rate is pinned.
#[must_use]
pub fn declared_document_baseline(ext: &str, source: &[u8]) -> u32 {
    if ext.trim_start_matches('.').eq_ignore_ascii_case("pdf") {
        return 0;
    }
    // At most four bytes a character, so this is every byte that can be part
    // of the first `NATIVE_INLINE_CHARS` characters and no more: a 16 MiB
    // container is never decoded whole to price a window this small.
    let head = source
        .get(..NATIVE_INLINE_CHARS.saturating_mul(4))
        .unwrap_or(source);
    let text: String = String::from_utf8_lossy(head)
        .chars()
        .take(NATIVE_INLINE_CHARS)
        .collect();
    rift_tokens::count(&text).min(NATIVE_READ_TOKEN_CEILING)
}

/// The listing a native call would have printed that Rift is willing to stand
/// in for: what the client would have shown for this shape, minus every path
/// this repository refuses to serve.
///
/// **Why the refused paths leave the pool rather than stay in it.** A native
/// listing really would have printed them, so counting them would be defensible
/// arithmetic — and it would still be Rift claiming credit for tokens it did
/// not remove from anybody's context, since a path it will not name is a path
/// the model has to go and find another way. The conservative direction is the
/// only one a figure Rift publishes may err in, so the screen is answered
/// *before* the price and never after it. Two of the screens applied here are
/// the same two this crate's own search screen applies to what it renders —
/// the repository's own `denied_paths`, and the structural safety of the name
/// itself — and `refused` is the caller's own third: the repository's
/// sensitive list belongs to the policy engine's private side, and the one
/// caller that must not apply it (the `search` verb, which deliberately names
/// sensitive paths to the operator who asked) says so by passing a screen that
/// refuses nothing.
///
/// Where the shape's [`SearchCeiling`] is a character budget the screen runs
/// inside the fill, so a refused path neither spends the budget nor stops the
/// listing at it; where it is a file cap the head is taken first and then
/// screened, so a refused path still spends one of the cap's slots — the
/// conservative side, since it can only make the priced listing shorter.
#[must_use]
pub fn priceable_listing(
    policy: &PolicyEngine<'_>,
    shape: ListingShape,
    family: &str,
    paths: &[String],
    refused: impl Fn(&str) -> bool,
) -> Vec<String> {
    listing_within(SearchCeiling::of(family, shape), paths, |path| {
        policy.is_denied_path(path) || !path_is_safe(path) || refused(path)
    })
}

/// The **declared** native cost of the search this verb stands in for, in
/// tokens.
///
/// Declared, not measured, and the same distinction as
/// [`declared_document_baseline`]: the listing this prices never ran, so there
/// is no second output to count.
///
/// **The formula, in one sentence, so a reader never has to infer it from the
/// code:** the paths of [`priceable_listing`] — the matches Rift found, minus
/// the ones this repository refuses to name, cut at the ceiling this call's
/// own client documents for this call's own shape — joined one per line as a
/// native listing prints them, counted with [`rift_tokens`], the tokenizer of
/// record. Nothing else enters it: no header, no per-path decoration, no
/// estimate.
///
/// **The ceiling, and where each half of it comes from.** A
/// [`Glob`](ListingShape::Glob) stops at [`NATIVE_LISTING_PATHS`] files, which
/// the tool's own page states. A [`Listing`](ListingShape::Listing) is not
/// bounded by a file count at all: the command prints every match and the
/// client stops reading its result at [`NATIVE_INLINE_CHARS`] characters,
/// which the same page states — so the paths are laid out one per line and cut
/// where that budget runs out. [`SearchCeiling::of`] holds the per-family
/// table and the one assumption in it.
///
/// Callers hand it the screened listing, which is what makes the first
/// sentence true; the two products that price a name search — the hook's
/// worker and the `search` verb — both take it from [`priceable_listing`], and
/// the search fixture holds the two halves against each other on a generated
/// repository.
///
/// It errs low in four ways at once, which is the direction a figure Rift will
/// claim credit for has to err in: Rift's index skips what git ignores and the
/// native listing does not, so the match set priced here is a subset; the paths
/// are repository-relative where the tool prints absolute ones; the tool's own
/// truncation notice is not counted at all; and a path the screen refused is
/// removed from the pool rather than claimed.
///
/// **A content search declares nothing.** Its native counterpart is a content
/// grep, whose output is every matching line of every matching file with no cap
/// at all unless the caller set one — there is no ceiling in this tree to price
/// it against, and a number invented for it would be the one figure in this
/// plane nobody could check. Zero is the honest answer, and the row that
/// carries it says so with [`TAG_BASELINE_NOT_DECLARED`](crate::TAG_BASELINE_NOT_DECLARED).
#[must_use]
pub fn declared_search_baseline(
    content: bool,
    shape: ListingShape,
    family: &str,
    paths: &[String],
) -> u32 {
    if content {
        return 0;
    }
    let listing = listing_within(SearchCeiling::of(family, shape), paths, |_| false).join("\n");
    rift_tokens::count(&listing)
}

/// The paths of one native listing, in the order the ceiling cuts them, with
/// everything `refused` says may not be counted left out — inside the fill for
/// a character budget, after the head is taken for a file cap — before the cut
/// rather than after it.
///
/// One function so the screen and the price can never be taken over two
/// different sets: [`priceable_listing`] calls it with a screen, and
/// [`declared_search_baseline`] calls it with none over an already-screened
/// list, which makes the second cut idempotent.
///
/// **`Files`** takes the head as it was given, because a glob tool's own
/// ranking decides which files it prints and Rift's caller hands them in the
/// order it would claim them.
///
/// **`InlineChars`** sorts first. A character budget cuts a *set*, and which
/// paths survive the cut must not depend on a directory walk order nothing in
/// this tree pins — one order, taken lexicographically, is the one a caller
/// and a fixture can both reproduce. Each path is charged its own length plus
/// the newline that separates it, including the first, which errs the count
/// low by exactly one character.
fn listing_within(
    ceiling: SearchCeiling,
    paths: &[String],
    refused: impl Fn(&str) -> bool,
) -> Vec<String> {
    match ceiling {
        SearchCeiling::Files(cap) => paths
            .iter()
            .take(cap)
            .filter(|path| !refused(path))
            .cloned()
            .collect(),
        SearchCeiling::InlineChars(cap) => {
            let mut sorted: Vec<&String> = paths.iter().collect();
            sorted.sort_unstable();
            let mut chars = 0usize;
            let mut kept = Vec::new();
            for path in sorted {
                if refused(path) {
                    continue;
                }
                if chars + path.len() + 1 > cap {
                    break;
                }
                chars += path.len() + 1;
                kept.push(path.clone());
            }
            kept
        }
    }
}
