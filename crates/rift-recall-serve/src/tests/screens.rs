//! The source-level screens: structural claims about the production half of
//! this crate that a passing suite could not make on its own — one token
//! charge site, sealed constructors only, no inline four-state.
//!
//! **What is screened.** Every production file of the crate, read through
//! `include_str!` from one fixed list ([`PRODUCTION_SOURCES`]) rather than a
//! directory walk, so a new module cannot slip out of the screen by being
//! new: the list is held against the `mod` tree parsed out of `lib.rs` and
//! `verbs/mod.rs`, in both directions. A file's trailing `#[cfg(test)] mod`
//! is cut off first, so what is screened is what ships.
//!
//! **The one exemption.** The four-state screen exempts exactly one region:
//! what follows the definition of `envelope_view` in `envelope.rs`, the sole
//! reader of `ArtifactState`. The horizon is a file boundary plus a
//! column-zero anchor inside that one file, and the anchor's uniqueness is
//! asserted rather than assumed, as it always was.

/// Every production source of the crate, in one fixed order.
///
/// `include_str!` is resolved at compile time, so a file that stops existing
/// fails the build and a file that is not listed is not screened — which is
/// why [`the_screened_files_are_the_declared_modules`] holds this list against
/// the `mod` declarations, so an unlisted module is a red test rather than a
/// silent hole.
const PRODUCTION_SOURCES: &[(&str, &str)] = &[
    ("lib.rs", include_str!("../lib.rs")),
    ("basis.rs", include_str!("../basis.rs")),
    ("basis_store.rs", include_str!("../basis_store.rs")),
    ("envelope.rs", include_str!("../envelope.rs")),
    ("held_diff.rs", include_str!("../held_diff.rs")),
    ("pricing.rs", include_str!("../pricing.rs")),
    ("provenance.rs", include_str!("../provenance.rs")),
    ("verbs/mod.rs", include_str!("../verbs/mod.rs")),
    ("verbs/doc.rs", include_str!("../verbs/doc.rs")),
    ("verbs/exact.rs", include_str!("../verbs/exact.rs")),
    ("verbs/hunks.rs", include_str!("../verbs/hunks.rs")),
    ("verbs/pack.rs", include_str!("../verbs/pack.rs")),
    ("verbs/search.rs", include_str!("../verbs/search.rs")),
    ("verbs/state.rs", include_str!("../verbs/state.rs")),
    ("window.rs", include_str!("../window.rs")),
];

/// The file that holds the four-state view, and so the one file with an
/// exempt tail.
const VIEW_FILE: &str = "envelope.rs";

/// The joined, column-zero anchor a file's trailing test module starts with.
/// Assembled from parts so this file's own copy is not one.
fn test_module_anchor() -> String {
    format!("\n#[{}]\nmod ", "cfg(test)")
}

/// The production half of one source file: everything before its trailing
/// `#[cfg(test)] mod`, or the whole file when it has none.
///
/// A second marker is reported rather than picked: the files this crate
/// carries hold at most one, and a screen that chose one of two would be
/// exempting the region between them on nobody's say-so.
fn production_half(src: &str) -> Result<&str, String> {
    let anchor = test_module_anchor();
    match src.matches(&anchor).count() {
        0 => Ok(src),
        1 => src
            .split(&anchor)
            .next()
            .ok_or_else(|| "unreachable: split always yields a first part".to_string()),
        n => Err(format!(
            "{n} test-module markers in one file, so the production half cannot be cut"
        )),
    }
}

fn source(name: &str) -> &'static str {
    PRODUCTION_SOURCES
        .iter()
        .find(|(file, _)| *file == name)
        .map_or_else(
            || panic!("{name} is not a listed production source"),
            |(_, src)| *src,
        )
}

/// The whole production surface, every file's production half concatenated in
/// the fixed order, with a file-boundary line between them.
fn production_surface() -> String {
    let mut surface = String::new();
    for (file, src) in PRODUCTION_SOURCES {
        surface.push_str(&format!("\n// ==== {file}\n"));
        surface.push_str(production_half(src).unwrap());
    }
    surface
}

/// The `mod` names a file declares at column zero, test modules excluded.
fn declared_modules(src: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut lines = src.lines().peekable();
    while let Some(line) = lines.next() {
        if line == "#[cfg(test)]" {
            // The next line is the gated declaration; skip it.
            let _ = lines.next();
            continue;
        }
        // `pub`, `pub(crate)` and bare declarations all count.
        let bare = line.strip_prefix("pub").map_or(line, |rest| {
            rest.trim_start_matches(|c| c != ' ').trim_start()
        });
        if let Some(name) = bare
            .strip_prefix("mod ")
            .and_then(|rest| rest.strip_suffix(';'))
        {
            names.push(name.to_owned());
        }
    }
    names
}

/// The files the `mod` tree reaches from `lib.rs`, as this crate lays them
/// out: a module is `name.rs`, or `name/mod.rs` plus the files it declares.
fn files_reached_by_the_mod_tree() -> Vec<String> {
    let mut reached = vec!["lib.rs".to_owned()];
    for name in declared_modules(source("lib.rs")) {
        let nested = format!("{name}/mod.rs");
        if let Some((_, src)) = PRODUCTION_SOURCES.iter().find(|(f, _)| *f == nested) {
            reached.push(nested.clone());
            reached.extend(
                declared_modules(src)
                    .into_iter()
                    .map(|child| format!("{name}/{child}.rs")),
            );
        } else {
            reached.push(format!("{name}.rs"));
        }
    }
    reached.sort();
    reached
}

/// The horizon the sealed-constructor screen cuts at: the **definition** of
/// `envelope_view`, spelled as a column-zero item.
///
/// Assembled from parts so this file's own copy of the needle is not one — the
/// screen reads the view file, but a horizon that matches its own test file is
/// a trap the next reader inherits.
fn view_anchor() -> String {
    format!("\npub(crate) {} envelope_view(", "fn")
}

/// The part of the view file the screen applies to: everything before
/// `envelope_view`'s definition.
///
/// The anchor is **joined and column-zero**, and its uniqueness is **asserted**
/// rather than assumed — the two halves of the same fix (`c7ed08e`, the same
/// class in `xtask/src/ci_meta.rs`). A bare `"fn envelope_view"` needle cuts at
/// the *first* occurrence anywhere, so a doc comment that merely QUOTES the
/// signature — `/// see `fn envelope_view` for the projection` — silently moves
/// the horizon up and shrinks the screened region to nothing. It fails open, and
/// it needs no adversary: writing that doc comment is an ordinary thing to do.
///
/// With this anchor, anything the screen cannot recognise is REPORTED rather
/// than exempted:
/// * zero matches — the definition was renamed, moved or made non-column-zero,
///   so the screen's premise is gone and it must not silently pass;
/// * two or more — ambiguous (a nested module defining its own), so the screen
///   must not silently pick one.
///
/// RESIDUAL, stated rather than papered over: a string literal or block comment
/// holding the exact sequence `\npub(crate) fn envelope_view(` at column zero still moves
/// the horizon. The view file holds none today, and
/// `the_declared_residual_is_exactly_as_wide_as_documented` pins that. This is
/// narrower than what it replaces, not absent.
fn screened_prefix(src: &str) -> Result<&str, String> {
    let anchor = view_anchor();
    match src.matches(&anchor).count() {
        1 => src
            .split(&anchor)
            .next()
            .ok_or_else(|| "unreachable: split always yields a first part".to_string()),
        0 => Err(format!(
            "the sealed-constructor screen has no horizon: no column-zero `{}` in the source it \
             reads. If the projection was renamed, rename the anchor with it.",
            anchor.trim_start()
        )),
        n => Err(format!(
            "the sealed-constructor screen has {n} candidate horizons, so it cannot say which \
             region is exempt. Disambiguate before trusting it."
        )),
    }
}

/// Everything the four-state screen reads: every production half whole, and
/// the view file up to its horizon.
fn four_state_screened() -> String {
    let mut screened = String::new();
    for (file, src) in PRODUCTION_SOURCES {
        let half = production_half(src).unwrap();
        let kept = if *file == VIEW_FILE {
            screened_prefix(half).unwrap()
        } else {
            half
        };
        screened.push_str(&format!("\n// ==== {file}\n"));
        screened.push_str(kept);
    }
    screened
}

#[test]
fn the_screened_files_are_the_declared_modules() {
    // The list is held against the `mod` tree in both directions: a module
    // declared but not listed would be unscreened, and a file listed but not
    // declared would be dead text lending the screen a false width.
    let mut listed: Vec<String> = PRODUCTION_SOURCES
        .iter()
        .map(|(file, _)| (*file).to_owned())
        .collect();
    listed.sort();
    assert_eq!(listed, files_reached_by_the_mod_tree());
}

#[test]
fn no_serve_path_builds_four_state_inline() {
    // Structural, not a convention: only the sealed constructors in
    // rift-recall-validity (`manifest_envelope`/`envelope_for`) produce a
    // four-state `ArtifactState`. No handler in this crate may name an
    // `ArtifactState` variant — the read-only `envelope_view` projection is the
    // sole exception. A new inline builder anywhere in the production surface
    // re-introduces the token outside the exempt tail and trips this test.
    let screened = four_state_screened();
    assert!(
        !screened.contains("ArtifactState::"),
        "a serve path constructs an ArtifactState inline (a sealed-constructor bypass)"
    );
}

#[test]
fn the_screen_still_covers_the_serving_half_of_the_file() {
    // The anti-shrink tripwire, independent of how the horizon is found: the
    // exempt region is one function's tail in one file, so the screened text
    // is very nearly the whole production surface. If it ever stops being
    // most of it, the screen has been moved rather than the code.
    let screened = four_state_screened();
    let whole = production_surface();
    assert!(
        screened.len() * 2 > whole.len(),
        "the sealed-constructor screen now covers only {}/{} bytes of the crate",
        screened.len(),
        whole.len()
    );
}

#[test]
fn the_exempt_tail_holds_nothing_but_the_view() {
    // The horizon is a file boundary plus one anchor: past the anchor there is
    // the view's own body and nothing else, so nothing can hide behind it.
    let half = production_half(source(VIEW_FILE)).unwrap();
    let prefix = screened_prefix(half).unwrap();
    let tail = &half[prefix.len() + view_anchor().len()..];
    for opener in [
        "\nfn ", "\npub", "\nimpl", "\nstruct", "\nenum", "\nconst", "\nmod ",
    ] {
        assert!(
            !tail.contains(opener),
            "another item sits behind the horizon: {opener:?} in {tail:?}"
        );
    }
}

#[test]
fn a_quoted_signature_reports_instead_of_shrinking_the_screen() {
    // The defect this guards against, exactly: a doc comment that only MENTIONS the
    // projection. Under the old bare needle this cut the screened region down to
    // the file header and the test kept passing; now it is either invisible to
    // the anchor (a quote is not column-zero) or reported as ambiguous.
    let src = source(VIEW_FILE);
    let quoted = format!("/// see `fn envelope_view` for the projection\n{src}");
    let screened = screened_prefix(&quoted).unwrap();
    assert!(
        screened.len() * 2 > quoted.len(),
        "a quoted signature moved the horizon: {}/{} bytes",
        screened.len(),
        quoted.len()
    );
}

#[test]
fn a_second_definition_is_reported_not_silently_picked() {
    let src = source(VIEW_FILE);
    let planted = format!(
        "{src}\nmod inner {{\npub(crate) {} envelope_view() {{}}\n}}\n",
        "fn"
    );
    let err = screened_prefix(&planted).unwrap_err();
    assert!(err.contains("2 candidate horizons"), "{err}");
}

#[test]
fn a_missing_definition_is_reported_not_a_silent_pass() {
    let err = screened_prefix("fn other() {}\n").unwrap_err();
    assert!(err.contains("no horizon"), "{err}");
    // And the screen fails LOUDLY rather than exempting the whole file: a
    // renamed projection must not turn the guard off.
    let renamed =
        source(VIEW_FILE).replace("\npub(crate) fn envelope_view(", "\npub(crate) fn view_of(");
    assert!(screened_prefix(&renamed).is_err());
}

#[test]
fn the_declared_residual_is_exactly_as_wide_as_documented() {
    // `screened_prefix`'s docstring declares one residual: the joined sequence
    // inside a string literal or block comment. This pins that the view file
    // holds exactly one occurrence — the definition — and that no other
    // production file holds the anchor at all, so the residual is a statement
    // about shape and not a live hole. If this ever fails, the anchor got
    // STRONGER than documented — delete this test and say so.
    let anchor = view_anchor();
    for (file, src) in PRODUCTION_SOURCES {
        let occurrences: Vec<usize> = src.match_indices(&anchor).map(|(i, _)| i).collect();
        if *file != VIEW_FILE {
            assert!(occurrences.is_empty(), "{file}: {occurrences:?}");
            continue;
        }
        assert_eq!(occurrences.len(), 1, "{file}: {occurrences:?}");
        // The one occurrence is the definition: what follows it is the
        // signature's remainder, not comment or string text.
        let after = occurrences
            .first()
            .and_then(|i| src.get(i + anchor.len()..))
            .unwrap_or_default();
        assert!(
            after.starts_with("e: &Envelope) -> Value {"),
            "the single anchor is not the definition: {:?}",
            after.get(..40)
        );
    }
}

/// Structural: exactly one `TurnBudget` token-charge site across the whole
/// production surface — `try_tokens` appears only inside `charge`, and
/// `charge` is called only at the respond seam. The call ceiling (`try_call`)
/// lives only in the dispatch middleware. A second charge site (a handler
/// mutating the budget, in any module) trips this.
#[test]
fn turn_budget_has_exactly_one_token_charge_site() {
    let src = production_surface();
    assert_eq!(
        src.matches(".try_tokens(").count(),
        1,
        "try_tokens must appear only inside charge()"
    );
    assert_eq!(
        src.matches("charge(budget,").count(),
        1,
        "charge must be called only at the respond seam"
    );
    assert_eq!(
        src.matches(".try_call(").count(),
        1,
        "the call ceiling is drawn only in dispatch middleware"
    );
}

/// No serve path constructs a four-state envelope or a pack
/// section outside the sealed constructors. Structural, source-level: the
/// forgeable forms are struct-literal `Envelope {`/`Section {` construction
/// and field-init `state: ArtifactState::…` — matching on `&e.state` (the
/// read side) uses neither. `SectionEntry`/`ValidSummary` need no scan: their
/// fields are private, so a bypass there is already a compile error.
#[test]
fn no_bypass_serve_paths_use_sealed_constructors_only() {
    let src = production_surface();
    for forged in [
        "Envelope {",
        "state: ArtifactState::",
        "Section {",
        "PackItem",
    ] {
        assert!(
            !src.contains(forged),
            "serve path contains a sealed-constructor bypass: `{forged}` \
             (build envelopes via envelope_for/manifest_envelope and pack \
             sections via SectionEntry/PackRequest instead)"
        );
    }
}
