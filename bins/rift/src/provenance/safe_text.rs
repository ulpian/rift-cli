//! Turning untrusted strings into something that can reach a terminal.
//!
//! Its own module because the guarantee is a property of these types and
//! functions alone: [`Label`] has one constructor and a private field, and
//! every other provenance module reaches a terminal through [`safe_path`] or
//! [`safe_excerpt`]. A second sanitizer anywhere else would be a second place
//! for the rule to be almost right.

/// Longest agent/model label rendered. Long enough for real model ids, short
/// enough that a hostile one cannot reflow a terminal.
const MAX_LABEL: usize = 48;

/// Longest *rendered* path in the human header, in characters after escaping.
///
/// Comfortably above the paths this workspace actually holds — the deepest
/// tracked one is **100 characters**
/// (`crates/rift-recall-eval/src/snapshots/rift_recall_eval__record__tests__seeded_regression_report.snap`,
/// measured, not estimated; git's own ceiling is 4096 bytes) — so the bound only
/// ever bites a pathological name. It is a scrollback bound, **not** the forgery
/// defence: escaping is (see [`safe_path`]), and what a bound cannot defend
/// against is quantified in the module doc.
pub(super) const MAX_PATH: usize = 160;

/// A string that has been through the label filter — the only way to have one.
///
/// Agent and model strings come from adapters and from a shard's metadata; they
/// are data, not code, and this verb is one of the few places they are printed
/// beside untrusted git output. Sanitizing them *at the call site* worked but
/// bound nothing: a second construction site would have bypassed the filter in
/// silence and no test would have failed. The guarantee lives in the type
/// instead — [`Label::of`] is the sole constructor and the field is private, so
/// a `SessionRef` carrying an unfiltered agent is unspellable.
#[derive(Clone, Debug, PartialEq, Eq, ::serde::Serialize)]
#[serde(transparent)]
pub(crate) struct Label(String);

impl Label {
    /// Keep only characters that can safely reach a terminal, bounded in
    /// length: `[A-Za-z0-9._/+-]` survives, anything else becomes `.`, and a
    /// label longer than [`MAX_LABEL`] is cut with a `…` so it cannot reflow a
    /// terminal.
    pub(crate) fn of(s: &str) -> Self {
        let mut out: String = s
            .chars()
            .take(MAX_LABEL)
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '+') {
                    c
                } else {
                    '.'
                }
            })
            .collect();
        if s.chars().count() > MAX_LABEL {
            out.push('…');
        }
        Self(out)
    }
}

impl std::fmt::Display for Label {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A caller-supplied path as it can safely be shown: escaped, quoted, bounded.
///
/// **The escaping is the load-bearing half.** A filename is attacker-controlled
/// in any repository you clone, so a path that is a newline followed by a
/// plausible blame row would otherwise print a row byte-indistinguishable from
/// a real one — asserting `ai` authorship on `file-touched`, the one basis this
/// module reserves for recorded evidence. `char::escape_debug` is the same
/// rendering `{:?}` gives, which is what the not-blameable arm already used, so
/// the two now agree by construction rather than by coincidence; it escapes
/// every `Cc`/`Cf` code point, so `\n`, `\r`, `\t`, `ESC` and `U+202E` all come
/// out as text. Interior quotes are escaped too, so the closing quote is
/// unambiguous.
///
/// The length bound is the weaker half and is not claimed as a forgery defence:
/// a bound that would defeat *wrapping*-based forgery would have to be smaller
/// than a terminal's width, which this cannot know. It exists so a pathological
/// filename cannot fill a scrollback, and it counts **rendered** characters,
/// since one hostile code point can escape to eight. The module doc carries the
/// same disclaimer with the arithmetic (68 characters on line 1 and 80 on line 2
/// at 80 columns) — deliberately, because a blanket claim up there and an
/// admission down here would be a disagreement about the same fact.
pub(crate) fn safe_path(p: &str) -> String {
    let mut body = String::new();
    let mut width = 0usize;
    let mut cut = false;
    for c in p.chars() {
        let piece: String = c.escape_debug().collect();
        let w = piece.chars().count();
        if width + w > MAX_PATH {
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

/// A redacted excerpt as it can safely reach a terminal: every char through
/// `escape_debug` (C0/C1, ESC, RTL overrides all become text), bounded to
/// `max` rendered characters with a `…` when cut. Newlines stay literal so the
/// caller can indent each line — they are the one control char a prompt block
/// wants, and the caller prints them only via its own `  | ` prefix.
pub(crate) fn safe_excerpt(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut width = 0usize;
    for c in s.chars() {
        if width >= max {
            out.push('…');
            break;
        }
        if c == '\n' {
            out.push('\n');
            width += 1;
            continue;
        }
        let piece: String = c.escape_debug().collect();
        width += piece.chars().count();
        out.push_str(&piece);
    }
    out
}

/// `n unit` or `n units`.
pub(super) fn plural(n: usize, unit: &str) -> String {
    if n == 1 {
        format!("{n} {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

/// The first 12 characters of an id — enough to recognise, short enough to scan.
pub(super) fn short(id: &str) -> String {
    id.chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The prompt block's neutralizer: controls become text, the bound holds,
    /// and the newline survives (the caller indents each line itself).
    #[test]
    fn the_prompt_excerpt_escapes_controls_and_keeps_newlines() {
        let s = "fix\u{1b}[31m the build\nplease \u{202e}now";
        let out = safe_excerpt(s, 600);
        assert!(!out.chars().any(|c| c.is_control() && c != '\n'), "{out:?}");
        assert!(out.contains('\n'));
        assert!(out.contains("\\u{1b}"), "{out:?}");
        let long = safe_excerpt(&"x".repeat(700), 600);
        assert!(long.chars().count() <= 601 && long.ends_with('…'));
    }

    #[test]
    fn a_hostile_label_cannot_reach_the_terminal_intact() {
        let evil = "claude\u{1b}[2Jcode\nrm -rf /";
        let safe = Label::of(evil).to_string();
        assert!(!safe.contains('\u{1b}'));
        assert!(!safe.contains('\n'));
        assert!(!safe.contains(' '));
        assert_eq!(Label::of("claude-code").to_string(), "claude-code");
        // Bounded: a very long model id cannot reflow a terminal.
        let long = Label::of(&"m".repeat(400)).to_string();
        assert_eq!(long.chars().count(), MAX_LABEL + 1);
    }
}
