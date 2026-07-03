//! Terminal-escape sanitization for untrusted projection strings.
//!
//! Column 2 (and the column-1 list) render values that originate outside Rift —
//! model names, paths, agent identifiers captured from tool output. Emitting those
//! bytes verbatim would let a hostile string move the cursor, recolor the screen,
//! or spoof the UI via ANSI/OSC escape sequences (`05-security`: untrusted tool
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
}
