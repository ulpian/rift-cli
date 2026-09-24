//! The `one-reason-renderer` leg: one module writes the sentences a model
//! reads.

use std::path::Path;

use crate::ci_meta::production_half;

use super::text::mask_comments;
use super::{is_test_surface, line_of, rust_corpus, Leg};

/// Where every sentence a model reads from Rift is written.
const RENDERER: &str = "crates/rift-recall-validity/src/trust/frames/mod.rs";

/// **One module writes the sentences a model reads.**
///
/// Every frame Rift pushes into a model's context is refused if it can be
/// forged, counted honestly, and held to its own bytes by goldens — all of that
/// inside one module. A second place that opens a line with one of the same
/// phrases inherits none of it, and it is indistinguishable to a reader (and to
/// a model) from the real thing.
///
/// So: no production code outside the renderer may spell one of its openers.
/// Comments are masked and string literals are not, because a phrase written
/// into an output IS a string literal, while a comment quoting one is how this
/// codebase explains itself.
///
/// # Errors
/// If the corpus or the renderer cannot be read, or the openers stop parsing.
pub fn one_reason_renderer(root: &Path) -> Result<Leg, String> {
    Ok(screen_renderer(&rust_corpus(root)?, &frame_openers(root)?))
}

/// The screen over an explicit corpus and opener list — the seam a planted
/// sentence is proven through.
fn screen_renderer(corpus: &[(String, String)], openers: &[String]) -> Leg {
    let mut v = Vec::new();
    let mut screened = 0_usize;
    for (rel, text) in corpus {
        if rel == RENDERER || is_test_surface(rel) {
            continue;
        }
        screened += 1;
        let production = mask_comments(&production_half(text));
        for opener in openers {
            for at in production.match_indices(opener.as_str()).map(|(at, _)| at) {
                v.push(format!(
                    "{rel}:{}: writes `{opener}`, a line only {RENDERER} may open — a frame \
                     written anywhere else carries none of the refusals that make it safe",
                    line_of(&production, at).saturating_sub(1)
                ));
            }
        }
    }
    Leg::new(
        "one-reason-renderer",
        format!(
            "{} opening phrase(s) declared in {RENDERER}; {screened} other file(s) write none",
            openers.len()
        ),
        v,
    )
}

/// The phrases the renderer opens its lines with, read out of the renderer
/// itself rather than retyped here.
///
/// Retyping them would make this gate a second copy to keep in step, and a
/// stale second copy guards the wording it remembers instead of the wording
/// that ships. The declarations are `const FRAME_<something>_OPENER: &str =
/// "…";` and the parse is deliberately dumb.
fn frame_openers(root: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(root.join(RENDERER))
        .map_err(|e| format!("read {RENDERER}: {e}"))?;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("const FRAME_") else {
            continue;
        };
        let Some((name, value)) = rest.split_once(": &str = ") else {
            continue;
        };
        if !name.ends_with("_OPENER") {
            continue;
        }
        let phrase = value.trim().trim_end_matches(';').trim_matches('"');
        if !phrase.is_empty() {
            out.push(phrase.to_owned());
        }
    }
    if out.len() < 5 {
        return Err(format!(
            "gate-intercept: only {} frame opener(s) parsed out of {RENDERER} — the declarations \
             changed shape and this leg would guard almost nothing",
            out.len()
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The openers come out of the renderer itself, so this gate cannot guard
    /// wording the renderer no longer has.
    #[test]
    fn the_frame_openers_are_read_out_of_the_renderer() {
        let openers = frame_openers(&crate::workspace_root()).expect("the openers parse");
        assert!(openers.len() >= 9, "{openers:?}");
        assert!(
            openers.iter().any(|o| o.contains("Read skipped:")),
            "{openers:?}"
        );
    }

    /// ★ A second place that opens a line with one of Rift's own frame phrases
    /// is caught; the same phrase in a comment is not.
    #[test]
    fn a_frame_phrase_written_outside_the_renderer_is_caught() {
        let openers = vec!["[rift] Read skipped:".to_owned()];
        let planted = vec![(
            "bins/rift/src/elsewhere.rs".to_owned(),
            "fn say() -> &'static str {\n    \"[rift] Read skipped: trust me\"\n}\n".to_owned(),
        )];
        let leg = screen_renderer(&planted, &openers);
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("elsewhere.rs:2"),
            "{:#?}",
            leg.violations
        );

        let explained = vec![(
            "bins/rift/src/elsewhere.rs".to_owned(),
            "/// Never writes `[rift] Read skipped:` itself.\nfn say() {}\n".to_owned(),
        )];
        assert!(
            screen_renderer(&explained, &openers).violations.is_empty(),
            "a comment explaining the guard is not a breach of it"
        );
    }
}
