//! The declared baselines: what a native document read and a native listing
//! would have cost, and the ceilings each is cut at.

use rift_recall_policy::PolicyEngine;

use super::fixtures::*;
use crate::*;

/// The declared baseline is a declaration, and its two arms say so: a container
/// is priced at the bytes a native read would have shown, cut at the pinned
/// inline ceiling; a PDF is priced at nothing, because what a PDF costs is
/// pages times a vision rate no artifact in this tree pins.
#[test]
fn the_declared_baseline_prices_bytes_and_declares_nothing_for_a_pdf() {
    let small = b"Sheet1\nA,B\n1,2\n";
    assert_eq!(
        declared_document_baseline("csv", small),
        rift_tokens::count("Sheet1\nA,B\n1,2\n")
    );
    assert_eq!(
        declared_document_baseline(".DOCX", small),
        declared_document_baseline("docx", small),
        "the extension arrives from a path, in both spellings and either case"
    );

    // Cut at the ceiling, not at the file: a container ten times the inline
    // threshold is priced at the threshold.
    let long = vec![b'x'; NATIVE_INLINE_CHARS * 10];
    assert_eq!(
        declared_document_baseline("docx", &long),
        rift_tokens::count(&"x".repeat(NATIVE_INLINE_CHARS))
    );

    // A PDF declares nothing, however big it is.
    assert_eq!(declared_document_baseline("pdf", &long), 0);
    assert_eq!(declared_document_baseline(".pdf", small), 0);
}

/// ★ The character ceiling is not a token ceiling, and a dense document is
/// declared at the one the client would really have stopped at.
///
/// A character is worth up to about a token, so the pinned 30,000-character
/// window is worth over 30,000 tokens of CJK — more than the whole result the
/// client would have delivered. Declaring that figure would claim credit for
/// tokens no read could have spent, so both ceilings apply and the tighter one
/// wins. The probe is a real one: this body's own uncut count is asserted to be
/// past the ceiling, so the case cannot quietly stop being the case it tests.
#[test]
fn a_dense_document_is_declared_at_the_clients_token_ceiling_not_its_character_one() {
    // Distinct CJK ideographs, which is what makes them dense: a repeated one
    // would merge into a handful of tokens and prove nothing. Broken into
    // short lines, which is how a document's text arrives and what keeps the
    // count a second's work rather than a minute's.
    let dense: String = (0..NATIVE_INLINE_CHARS)
        .map(|n| {
            if n % 16 == 15 {
                '\n'
            } else {
                char::from_u32(0x4E00 + u32::try_from(n % 20_000).unwrap()).unwrap()
            }
        })
        .collect();
    let uncut = rift_tokens::count(&dense);
    assert!(
        uncut > NATIVE_READ_TOKEN_CEILING,
        "the probe has to be past the ceiling to test it: {uncut}"
    );
    assert_eq!(
        declared_document_baseline("docx", dense.as_bytes()),
        NATIVE_READ_TOKEN_CEILING,
        "a declared figure never exceeds the result the client would have cut"
    );
}

/// **The declared baseline prices the listing this verb replaced, and nothing
/// else.**
///
/// It stops at the native cap because that is where the native tool stops, and
/// a content search declares nothing at all — the one figure this plane will
/// not invent.
#[test]
fn the_declared_search_baseline_prices_a_listing_and_never_a_content_grep() {
    let glob = ListingShape::Glob;
    let paths: Vec<String> = (0..250).map(|i| format!("src/module_{i:03}.rs")).collect();
    let hundred = declared_search_baseline(false, glob, "claude-code", paths.get(..100).unwrap());
    assert_eq!(
        declared_search_baseline(false, glob, "claude-code", &paths),
        hundred,
        "past the native cap the listing the model would have seen stops growing"
    );
    assert!(
        declared_search_baseline(false, glob, "claude-code", paths.get(..10).unwrap()) < hundred
    );
    assert_eq!(declared_search_baseline(false, glob, "claude-code", &[]), 0);
    assert_eq!(
        declared_search_baseline(true, glob, "claude-code", &paths),
        0,
        "a content grep has no declared native cost in this tree"
    );
}

/// ★ **Each shape is priced at its own client ceiling, and the two are
/// nothing like each other.**
///
/// A glob tool stops after a hundred files whatever else is true. A shell
/// listing prints every match and is cut only where the client stops reading
/// the result inline — so the same match set is a far larger listing when the
/// call that would have produced it was a shell command, and pricing both at
/// the file cap prices the second as though the first tool had run it.
///
/// The three alpha families price a listing alike: only Claude Code's page
/// states the character ceiling, and the other two take it by a deliberate
/// consistency choice rather than by an artifact of their own.
#[test]
fn each_listing_shape_is_priced_at_its_own_clients_ceiling() {
    let paths: Vec<String> = (0..4_000)
        .map(|i| format!("crates/a/src/m_{i:04}.rs"))
        .collect();
    let glob = declared_search_baseline(false, ListingShape::Glob, "claude-code", &paths);
    let listing = declared_search_baseline(false, ListingShape::Listing, "claude-code", &paths);
    assert_eq!(
        glob,
        declared_search_baseline(false, ListingShape::Glob, "claude-code", &paths[..100]),
        "a glob is its own tool's hundred files and no more"
    );
    assert!(
        listing > glob * 5,
        "a shell listing is bounded by characters, not by a hundred files: \
         {listing} vs {glob}"
    );
    // The character ceiling is a ceiling: the listing it cuts fits inside it,
    // and one more path would not have.
    let kept = priceable_listing(
        &PolicyEngine::new(&cfg()),
        ListingShape::Listing,
        "claude-code",
        &paths,
        |_| false,
    );
    let spelled: usize = kept.iter().map(|p| p.len() + 1).sum();
    assert!(spelled <= NATIVE_INLINE_CHARS, "{spelled}");
    assert!(
        spelled + kept[0].len() + 1 > NATIVE_INLINE_CHARS,
        "the budget was filled, not merely started: {spelled}"
    );
    assert!(kept.len() > NATIVE_LISTING_PATHS, "{}", kept.len());
    for family in ["claude-code", "codex", "antigravity"] {
        assert_eq!(
            SearchCeiling::of(family, ListingShape::Listing),
            SearchCeiling::InlineChars(NATIVE_INLINE_CHARS),
            "{family}: one figure across the alpha clients"
        );
        assert_eq!(
            SearchCeiling::of(family, ListingShape::Glob),
            SearchCeiling::Files(NATIVE_LISTING_PATHS),
            "{family}: a glob is bounded by its own tool"
        );
    }
    assert_eq!(
        SearchCeiling::of(NO_CLIENT_FAMILY, ListingShape::Glob),
        SearchCeiling::Files(NATIVE_LISTING_PATHS),
        "a call belonging to no client is priced at the bound that cannot be \
         too large"
    );
}

/// **A listing's price is its set, not the order the walk produced it in.**
///
/// A character budget cuts a set, so which paths survive the cut would depend
/// on a directory order nothing in this tree pins unless one order is chosen.
/// One is: the sorted one. Asserted rather than assumed, because if it stopped
/// holding, two runs over one repository would book two different figures.
#[test]
fn a_listing_is_cut_in_one_order_whatever_order_it_arrived_in() {
    let paths: Vec<String> = (0..4_000)
        .map(|i| format!("crates/a/src/m_{i:04}.rs"))
        .collect();
    let forward = declared_search_baseline(false, ListingShape::Listing, "codex", &paths);
    let mut backward = paths.clone();
    backward.reverse();
    assert_eq!(
        forward,
        declared_search_baseline(false, ListingShape::Listing, "codex", &backward)
    );
    // And the cut is idempotent: pricing an already-cut listing books the same
    // figure, which is what lets the hook cut and the worker price.
    let kept = priceable_listing(
        &PolicyEngine::new(&cfg()),
        ListingShape::Listing,
        "codex",
        &paths,
        |_| false,
    );
    assert_eq!(
        forward,
        declared_search_baseline(false, ListingShape::Listing, "codex", &kept)
    );
}

/// **The caller's own screen refuses before the price, and refusing costs the
/// listing nothing.**
///
/// The screen this crate cannot apply — the repository's sensitive list, which
/// lives on the policy engine's private side — is the hook's to pass in, and a
/// path it refuses must neither be counted nor consume the ceiling that stops
/// the count. Both, because a refused path that ate the budget would end the
/// listing early and quietly under-state it, which is a different bug from
/// over-stating it and just as much a wrong figure.
#[test]
fn the_callers_own_screen_refuses_before_the_ceiling_is_spent() {
    let cfg = cfg();
    let policy = PolicyEngine::new(&cfg);
    let mut paths: Vec<String> = (0..40).map(|i| format!("src/m_{i:02}.rs")).collect();
    paths.push("a-first-alphabetically/secret.pem".to_owned());
    let open = priceable_listing(
        &policy,
        ListingShape::Listing,
        "claude-code",
        &paths,
        |_| false,
    );
    let screened = priceable_listing(&policy, ListingShape::Listing, "claude-code", &paths, |p| {
        p.contains("secret")
    });
    assert!(open.contains(&"a-first-alphabetically/secret.pem".to_owned()));
    assert!(!screened.contains(&"a-first-alphabetically/secret.pem".to_owned()));
    assert_eq!(screened.len(), 40, "and every other path is still there");
    assert!(
        declared_search_baseline(false, ListingShape::Listing, "claude-code", &screened)
            < declared_search_baseline(false, ListingShape::Listing, "claude-code", &open),
        "dropping it makes the claim smaller, never larger"
    );
}

/// **A path the screen refuses is not priced into what the answer claims.**
///
/// The screen and the price are one decision taken in one order: the listing a
/// native call would have printed, cut where the tool cuts, minus the paths
/// this repository will not name. Pricing them and then refusing to show them
/// would be Rift billing for tokens it removed from nobody's context — the
/// arithmetic is defensible and the claim is not, so the conservative
/// direction wins and the refused path leaves the pool with the answer.
#[test]
fn a_path_the_screen_refuses_is_never_priced_into_the_declared_listing() {
    let cfg = cfg();
    let policy = PolicyEngine::new(&cfg).with_denied_paths(vec!["internal/**".to_owned()]);
    let mut paths: Vec<String> = (0..8).map(|i| format!("src/module_{i:02}.rs")).collect();
    paths.push("internal/roadmap.md".to_owned());
    // A name that could forge a line is not a name at all, here or in a frame.
    paths.push("src/broken\nname.rs".to_owned());

    let glob = ListingShape::Glob;
    let priced = priceable_listing(&policy, glob, "claude-code", &paths, |_| false);
    assert_eq!(
        priced,
        paths.get(..8).unwrap_or_default(),
        "the deny list and the structural screen both act before the price"
    );
    assert!(
        declared_search_baseline(false, glob, "claude-code", &priced)
            < declared_search_baseline(false, glob, "claude-code", &paths),
        "and dropping them makes the claim smaller, never larger"
    );

    // Past the native cap the screen has nothing left to work on: the tool
    // stopped there whatever this repository thinks of what it found.
    let many: Vec<String> = (0..250).map(|i| format!("src/module_{i:03}.rs")).collect();
    assert_eq!(
        priceable_listing(&policy, glob, "claude-code", &many, |_| false).len(),
        NATIVE_LISTING_PATHS
    );
    assert_eq!(
        declared_search_baseline(
            false,
            glob,
            "claude-code",
            &priceable_listing(&policy, glob, "claude-code", &many, |_| false)
        ),
        declared_search_baseline(false, glob, "claude-code", &many),
    );
}
