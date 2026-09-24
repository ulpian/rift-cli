//! The `closed-rewrite-forms` leg: every tool-call rewrite is one of three
//! pinned forms.

use std::path::Path;

use super::text::{is_ident_byte, mask_code, skip_balanced, token_offsets};
use super::{line_of, rust_corpus, Leg};

/// The two amendments Rift may make to a tool call, and the only fields either
/// one names.
///
/// `tool_input` is the call as the agent wrote it, carried so the amendment can
/// be built from it; the other two are the amendments themselves. A third field
/// name here would be a third thing Rift can change about a call it did not
/// make, which is the change this leg exists to make impossible to land
/// quietly.
const REWRITE_FIELDS: [&str; 3] = ["tool_input", "file_path", "head_limit"];

/// The two additive forms, spelled as the enum variants that carry them.
const REWRITE_VARIANTS: [&str; 2] = ["DocumentSidecar", "GrepHeadLimit"];

/// Every form the response capability table may name: two additive field
/// amendments and one bounded replacement of a strict redundant shell read.
const RESPONSE_REWRITE_FORMS: [&str; 3] = [
    "DocumentSidecarPath",
    "GrepHeadLimit",
    "ShellReadReplacement",
];

/// **A tool-call rewrite is one of three pinned forms, never an open-ended edit.**
///
/// Two screens over the masked corpus, tests included — a test that builds a
/// third form is a third form that exists:
///
/// 1. every `Rewrite::<Variant> { … }` names a variant from
///    `REWRITE_VARIANTS` and fields from `REWRITE_FIELDS`;
/// 2. every `RewriteForm::<Variant>` names one of the three pinned response forms;
/// 3. the additive `Rewrite` enum's own declaration names its two variants and
///    no others, with those field names and no others — read separately,
///    because a declaration is not written in the shape a construction site.
///
/// # Errors
/// If the corpus cannot be read, or neither form is found anywhere (a screen
/// over a vocabulary that has vanished is a screen that guards nothing).
pub fn closed_rewrite_forms(root: &Path) -> Result<Leg, String> {
    screen_rewrites(&rust_corpus(root)?)
}

/// The screen over an explicit corpus — the seam a planted third form is
/// proven through.
fn screen_rewrites(corpus: &[(String, String)]) -> Result<Leg, String> {
    let mut v = Vec::new();
    let mut sites = 0_usize;
    for (rel, text) in corpus {
        let masked = mask_code(text);
        // The amendment type lives in `rift-agent`, so only a file inside that
        // crate or one that names it can build one. Without this, an unrelated
        // `Rewrite` in another crate's test support answers to the same two
        // words and the leg reports on a type it has no business judging.
        if !rel.starts_with("crates/rift-agent") && !masked.contains("rift_agent") {
            continue;
        }
        v.extend(declared_rewrite_forms(rel, &masked));
        let bytes = masked.as_bytes();
        for at in token_offsets(&masked, "Rewrite::") {
            let mut i = at + "Rewrite::".len();
            let start = i;
            while bytes.get(i).copied().is_some_and(is_ident_byte) {
                i += 1;
            }
            let Some(variant) = masked.get(start..i) else {
                continue;
            };
            sites += 1;
            let line = line_of(&masked, at);
            if !REWRITE_VARIANTS.contains(&variant) {
                v.push(format!(
                    "{rel}:{line}: `Rewrite::{variant}` is a third amendment — Rift changes a \
                     document read's path or adds a search's missing cap, and nothing else"
                ));
                continue;
            }
            while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
                i += 1;
            }
            if bytes.get(i) != Some(&b'{') {
                continue;
            }
            for field in named_fields(&masked, i) {
                if !REWRITE_FIELDS.contains(&field.as_str()) {
                    v.push(format!(
                        "{rel}:{line}: `Rewrite::{variant}` names `{field}` — an amendment \
                         touches the one field its form is named for"
                    ));
                }
            }
        }
        for at in token_offsets(&masked, "RewriteForm::") {
            let mut i = at + "RewriteForm::".len();
            let start = i;
            while bytes.get(i).copied().is_some_and(is_ident_byte) {
                i += 1;
            }
            let Some(form) = masked.get(start..i) else {
                continue;
            };
            sites += 1;
            if !RESPONSE_REWRITE_FORMS.contains(&form) {
                v.push(format!(
                    "{rel}:{}: `RewriteForm::{form}` is not a pinned rewrite form",
                    line_of(&masked, at)
                ));
            }
        }
    }
    if sites == 0 {
        return Err("gate-intercept: no amendment site found anywhere in the tree".to_owned());
    }
    Ok(Leg::new(
        "closed-rewrite-forms",
        format!("{sites} rewrite site(s), all of them one of the three pinned forms"),
        v,
    ))
}

/// The variants and payload fields the amendment enum DECLARES, if `masked`
/// declares it.
///
/// A construction site is written `Rewrite::DocumentSidecar { … }` and a
/// declaration is not, so the two need separate readings. Without this one, an
/// unreachable third variant could sit in the enum indefinitely, waiting for a
/// caller — declared capability is capability.
fn declared_rewrite_forms(rel: &str, masked: &str) -> Vec<String> {
    let bytes = masked.as_bytes();
    let mut v = Vec::new();
    for at in token_offsets(masked, "enum Rewrite") {
        let Some(open) = masked
            .get(at..)
            .and_then(|rest| rest.find('{'))
            .map(|i| at + i)
        else {
            continue;
        };
        let end = skip_balanced(bytes, open, b'{', b'}');
        let mut depth = 0_usize;
        let mut i = open;
        while i < end {
            let Some(&b) = bytes.get(i) else { break };
            match b {
                b'{' | b'(' | b'[' => depth += 1,
                b'}' | b')' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
            if depth != 1
                || !is_ident_byte(b)
                || bytes
                    .get(i.wrapping_sub(1))
                    .is_some_and(|p| is_ident_byte(*p))
            {
                i += 1;
                continue;
            }
            let start = i;
            while bytes.get(i).copied().is_some_and(is_ident_byte) {
                i += 1;
            }
            let Some(variant) = masked.get(start..i) else {
                continue;
            };
            let line = line_of(masked, start);
            if !REWRITE_VARIANTS.contains(&variant) {
                v.push(format!(
                    "{rel}:{line}: the amendment enum declares `{variant}` — a declared third \
                     form is a third form, caller or no caller"
                ));
                continue;
            }
            let mut payload = i;
            while bytes.get(payload).is_some_and(u8::is_ascii_whitespace) {
                payload += 1;
            }
            if bytes.get(payload) != Some(&b'{') {
                continue;
            }
            for field in named_fields(masked, payload) {
                if !REWRITE_FIELDS.contains(&field.as_str()) {
                    v.push(format!(
                        "{rel}:{line}: `{variant}` declares `{field}` — an amendment carries the \
                         call and the one field its form is named for"
                    ));
                }
            }
        }
    }
    v
}

/// The field names a struct-literal or pattern names between `{` and `}`: an
/// identifier followed by `:` (but not `::`) at the top level of the braces.
fn named_fields(masked: &str, open_brace: usize) -> Vec<String> {
    let bytes = masked.as_bytes();
    let end = skip_balanced(bytes, open_brace, b'{', b'}');
    let mut out = Vec::new();
    let mut depth = 0_usize;
    let mut i = open_brace;
    while i < end {
        let Some(&b) = bytes.get(i) else { break };
        match b {
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth != 1
            || !is_ident_byte(b)
            || bytes
                .get(i.wrapping_sub(1))
                .is_some_and(|p| is_ident_byte(*p))
        {
            i += 1;
            continue;
        }
        let start = i;
        while bytes.get(i).copied().is_some_and(is_ident_byte) {
            i += 1;
        }
        let mut at = i;
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        if bytes.get(at) == Some(&b':') && bytes.get(at + 1) != Some(&b':') {
            if let Some(word) = masked.get(start..i) {
                out.push(word.to_owned());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★ A third amendment, or a third field on one of the two, is caught.
    #[test]
    fn a_third_way_to_amend_a_tool_call_is_caught() {
        let third_form = vec![(
            "crates/rift-agent/src/respond/mod.rs".to_owned(),
            "fn f() { let _ = Rewrite::ShellCommand { command: \"x\" }; }\n".to_owned(),
        )];
        let leg = screen_rewrites(&third_form).expect("the screen runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("third amendment"),
            "{:#?}",
            leg.violations
        );

        let third_field = vec![(
            "crates/rift-agent/src/respond/mod.rs".to_owned(),
            "fn f() { let _ = Rewrite::GrepHeadLimit { tool_input: t, head_limit: 4, path: p }; }\n"
                .to_owned(),
        )];
        let leg = screen_rewrites(&third_field).expect("the screen runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("names `path`"),
            "{:#?}",
            leg.violations
        );
    }

    /// ★ A fourth response rewrite form named on the capability table is
    /// caught: three forms are pinned and a fourth is a widened channel.
    #[test]
    fn a_fourth_response_rewrite_form_is_caught() {
        let fourth_form = vec![(
            "crates/rift-agent/src/respond/mod.rs".to_owned(),
            "fn f() { let _ = [RewriteForm::GrepHeadLimit, RewriteForm::ShellPrefix]; }\n"
                .to_owned(),
        )];
        let leg = screen_rewrites(&fourth_form).expect("the screen runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("`RewriteForm::ShellPrefix` is not a pinned rewrite form"),
            "{:#?}",
            leg.violations
        );
    }

    /// ★ A third variant DECLARED on the amendment enum is caught even with no
    /// caller anywhere.
    #[test]
    fn a_third_amendment_form_declared_but_never_built_is_caught() {
        let declared = vec![(
            "crates/rift-agent/src/respond/mod.rs".to_owned(),
            "pub enum Rewrite<'a> {\n    DocumentSidecar { tool_input: &'a Value, file_path: &'a str },\n             \x20   ShellPrefix { tool_input: &'a Value, prefix: &'a str },\n}\n             fn f() { let _ = Rewrite::DocumentSidecar { tool_input: t, file_path: p }; }\n"
                .to_owned(),
        )];
        let leg = screen_rewrites(&declared).expect("the screen runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("declares `ShellPrefix`"),
            "{:#?}",
            leg.violations
        );
    }

    /// An amendment type belonging to some other crate is not this leg's
    /// business, and a tree with no amendment at all is a screen guarding
    /// nothing.
    #[test]
    fn an_unrelated_type_of_the_same_name_is_left_alone() {
        let unrelated = vec![(
            "crates/rift-shadow-attribution/tests/support/mod.rs".to_owned(),
            "fn f() -> Rewrite { Rewrite::from(1) }\n".to_owned(),
        )];
        let err = screen_rewrites(&unrelated).expect_err("nothing amendable was found");
        assert!(err.contains("no amendment site"), "{err}");
    }
}
