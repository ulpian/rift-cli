//! Terminal-escape sanitization for untrusted projection strings.
//!
//! Column 2 (and the column-1 list) render values that originate outside Rift —
//! model names, paths, agent identifiers captured from tool output. Emitting those
//! bytes verbatim would let a hostile string move the cursor, recolor the screen,
//! or spoof the UI via ANSI/OSC escape sequences (the security model: untrusted tool
//! output is never served raw). [`sanitize_for_terminal`] is the one-way gate every
//! such string passes through before any glyph reaches a [`crate::Frame`] cell: it
//! drops all control characters (C0, `DEL`, and the C1 range — the `Cc` category),
//! which also neutralizes escape sequences because their `ESC`/CSI introducers are
//! themselves controls. What remains is inert printable text.

/// Return `s` with every control character removed, so no escape/CSI/OSC sequence
/// survives to reach the terminal. Idempotent and allocation-light (only when a
/// control char is present the string is rebuilt).
#[must_use]
pub fn sanitize_for_terminal(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// The **rendered-character budget** [`escape_for_terminal`] counts to, and the
/// same number `bins/rift` bounds a report path to.
///
/// It is deliberately larger than any panel is wide — a [`crate::Frame`] cell
/// grid clips by construction, so this bound is not about fitting. It is here
/// so the two surfaces produce byte-identical text for the same value, which is
/// what makes the cross-crate equality pin an equality.
pub const MAX_ESCAPED: usize = 160;

/// An untrusted value as a **card** can safely show it: escaped, quoted,
/// bounded — byte-identical to what the CLI's report renderers print for the
/// same string.
///
/// # Why this is not [`sanitize_for_terminal`]
///
/// Both neutralize; they differ in what the reader is left holding. Dropping a
/// control character is **lossy**, and for a *file list* lossy is a forgery
/// primitive: `src/\na.rs` and `src/a.rs` are two different repository paths
/// that the dropping filter renders identically, so a hostile filename can
/// present itself as an innocent one that really exists. Escaping cannot do
/// that — `char::escape_debug` renders every `Cc`/`Cf` code point as text, so
/// `\n`, `\r`, `\t`, `ESC` and `U+202E` come out visible and no two distinct
/// strings collide.
///
/// # Why it is byte-identical to the CLI's, on purpose
///
/// `rift dispatch` and `rift recap` print the same paths, agent tokens and
/// model names through `bins/rift`'s `provenance::safe_path`, and the whole
/// point of a card built on the verbs' own window facts is that the dashboard
/// and the document cannot disagree about a value. Two neutralizers would let
/// them disagree about the *spelling* even while agreeing about the fact.
/// `bins/rift`'s `the_card_escapes_exactly_as_the_report_does` asserts the
/// equality over a hostile corpus — the binary can see both functions; this
/// crate can only see one, which is why the pin lives there.
///
/// The length bound counts **rendered** characters (one hostile code point can
/// escape to eight) and is not claimed as a forgery defence — the same
/// disclaimer `bins/rift` carries, kept here so the two do not disagree about
/// their own strength either.
#[must_use]
pub fn escape_for_terminal(s: &str) -> String {
    let mut body = String::new();
    let mut width = 0usize;
    let mut cut = false;
    for c in s.chars() {
        let piece: String = c.escape_debug().collect();
        let w = piece.chars().count();
        if width + w > MAX_ESCAPED {
            cut = true;
            break;
        }
        body.push_str(&piece);
        width += w;
    }
    if cut {
        format!("\"{body}\"…")
    } else {
        format!("\"{body}\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_color_escape() {
        // A classic SGR color sequence: the ESC is dropped, leaving inert text.
        let dirty = "model\x1b[31mRED\x1b[0m";
        let clean = sanitize_for_terminal(dirty);
        assert!(!clean.contains('\x1b'), "no ESC survives");
        assert!(!clean.chars().any(char::is_control));
        assert_eq!(clean, "model[31mRED[0m");
    }

    #[test]
    fn strips_c0_c1_and_del() {
        let dirty = "a\u{0}b\u{7}c\u{7f}d\u{9b}e"; // NUL, BEL, DEL, C1 CSI
        let clean = sanitize_for_terminal(dirty);
        assert_eq!(clean, "abcde");
    }

    #[test]
    fn strips_newlines_and_tabs_that_would_break_a_cell() {
        assert_eq!(sanitize_for_terminal("x\n\r\ty"), "xy");
    }

    #[test]
    fn leaves_plain_text_and_unicode_untouched() {
        assert_eq!(sanitize_for_terminal("claude-opus-4"), "claude-opus-4");
        assert_eq!(sanitize_for_terminal("café ✓"), "café ✓");
    }

    #[test]
    fn is_idempotent() {
        let once = sanitize_for_terminal("a\x1b[1mb");
        assert_eq!(sanitize_for_terminal(&once), once);
    }

    #[test]
    fn escaping_renders_a_control_character_as_text_rather_than_dropping_it() {
        assert_eq!(escape_for_terminal("src/a.rs"), "\"src/a.rs\"");
        assert_eq!(escape_for_terminal("a\nb"), "\"a\\nb\"");
        assert_eq!(escape_for_terminal("x\u{1b}[31m"), "\"x\\u{1b}[31m\"");
        assert!(!escape_for_terminal("a\u{202e}b").contains('\u{202e}'));
        // The closing quote is unambiguous: an interior quote is escaped.
        assert_eq!(escape_for_terminal("a\"b"), "\"a\\\"b\"");
    }

    /// The lossy filter collides two different paths; the escape does not.
    /// That difference is the whole reason a second neutralizer exists.
    #[test]
    fn two_different_paths_never_render_the_same_escaped() {
        let hostile = "src/\na.rs";
        let innocent = "src/a.rs";
        assert_eq!(
            sanitize_for_terminal(hostile),
            sanitize_for_terminal(innocent),
            "the dropping filter really does collide them — this is the hazard"
        );
        assert_ne!(escape_for_terminal(hostile), escape_for_terminal(innocent));
    }

    #[test]
    fn the_bound_counts_rendered_characters_and_marks_the_cut() {
        let long = "a".repeat(MAX_ESCAPED + 10);
        let out = escape_for_terminal(&long);
        assert!(out.ends_with('…'), "a truncated value says so: {out}");
        assert_eq!(out.chars().count(), MAX_ESCAPED + 3, "quote+body+quote+cut");
        // One hostile code point escapes to several rendered characters, so the
        // budget is spent in rendered width, not in input length.
        let escapes = "\u{1b}".repeat(MAX_ESCAPED);
        let out = escape_for_terminal(&escapes);
        assert!(out.chars().count() <= MAX_ESCAPED + 3, "{out}");
    }
}
