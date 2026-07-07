//! Shared golden-test facility: the **style-aware snapshot serializer** (M55-03).
//!
//! The M4/M5 goldens are glyph-only (`frame_text` drops style; the ASCII backend
//! never flushes colour), so the whole surface theme is invisible to snapshots.
//! [`style_snapshot`] joins them: it serializes a frame's glyphs **and** every
//! cell's resolved fg/bg/attrs, in a stable, diff-readable text form, cut per
//! capability [`Tier`] — so a colour regression fails a test instead of a
//! dogfood eyeball (ADR-0023).
//!
//! Format (three blocks): the glyph grid; a parallel **style-id grid** where a
//! cell references a legend entry (`.` = terminal default); and the legend
//! mapping each id to its resolved style. Rows are guarded with `|` so trailing
//! blanks stay visible, and blank trailing rows (all-default) are trimmed for a
//! compact golden. Everything is a pure function of `(frame, tier)`.

// Shared across integration-test crates via `mod common;` (private module), so
// its `pub` helpers are crate-local by construction, and not every golden file
// exercises every helper.
#![allow(dead_code, unreachable_pub)]

use rift_canopy::render::frame::{Color, Frame, Mods, Style};
use rift_canopy::theme::{resolve_style, Tier};

/// The id alphabet for distinct non-default styles (default → `.`).
const IDS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// Serialize `frame` with per-cell style, resolved through `tier`. Deterministic
/// and reviewable: the same frame always yields the same block, and any change
/// to a glyph *or* a resolved colour/attr changes the output.
#[must_use]
pub fn style_snapshot(frame: &Frame, tier: Tier) -> String {
    let w = frame.width();
    let h = frame.height();

    // Resolve every cell once; assign a stable id to each distinct non-default
    // style in row-major first-appearance order.
    let mut legend: Vec<Style> = Vec::new();
    let resolved = |x: u16, y: u16| -> (char, Style) {
        let c = frame.get(x, y).unwrap_or_default();
        (c.ch, resolve_style(c.style, tier))
    };
    let id_of = |style: Style, legend: &mut Vec<Style>| -> char {
        if style == Style::DEFAULT {
            return '.';
        }
        let idx = legend.iter().position(|s| *s == style).unwrap_or_else(|| {
            legend.push(style);
            legend.len() - 1
        });
        char::from(*IDS.get(idx).unwrap_or(&b'?'))
    };

    // Last non-blank row (any non-space glyph or non-default style).
    let mut last_row = 0i32 - 1;
    for y in 0..h {
        for x in 0..w {
            let (ch, st) = resolved(x, y);
            if ch != ' ' || st != Style::DEFAULT {
                last_row = i32::from(y);
                break;
            }
        }
    }
    if last_row < 0 {
        return format!("[{tier:?}] (blank frame)");
    }
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let rows = (last_row as u16) + 1;

    let mut glyphs = String::new();
    let mut ids = String::new();
    for y in 0..rows {
        glyphs.push('|');
        ids.push('|');
        for x in 0..w {
            let (ch, st) = resolved(x, y);
            glyphs.push(ch);
            ids.push(id_of(st, &mut legend));
        }
        glyphs.push_str("|\n");
        ids.push_str("|\n");
    }

    let mut legend_txt = String::new();
    for (i, style) in legend.iter().enumerate() {
        let id = char::from(*IDS.get(i).unwrap_or(&b'?'));
        legend_txt.push_str(&format!(
            "{id}  fg={}  bg={}  {}\n",
            fmt_color(style.fg),
            fmt_color(style.bg),
            fmt_mods(style.mods)
        ));
    }

    format!(
        "[{tier:?}]\n{glyphs}\n· style ids ·\n{ids}\n· legend ·\n{}",
        if legend_txt.is_empty() {
            "(no colour)\n".to_string()
        } else {
            legend_txt
        }
    )
}

/// The four capability tiers, truecolor → 256 → mono → ASCII.
pub const TIERS: [Tier; 4] = [Tier::Truecolor, Tier::Ansi256, Tier::Mono, Tier::Ascii];

/// Render a surface at all four tiers into one golden block — the load-bearing
/// form the gate's tier leg asserts. `draw` is called **once per tier** so glyph
/// substitution (icons drop to ASCII on the floor via `tier.unicode()`) is part
/// of the golden, not just the colour resolution.
#[must_use]
pub fn four_tier_golden(draw: impl Fn(Tier) -> Frame) -> String {
    TIERS
        .iter()
        .map(|t| style_snapshot(&draw(*t), *t))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Format a colour: `#rrggbb`, `@NNN` (256-indexed), or `.` (terminal default).
fn fmt_color(c: Color) -> String {
    match c {
        Color::Default => ".".to_string(),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(i) => format!("@{i}"),
    }
}

/// Format the attribute set: `+bold+underline…`, or `-` when none.
fn fmt_mods(m: Mods) -> String {
    let mut out = String::new();
    for (flag, name) in [
        (Mods::BOLD, "bold"),
        (Mods::DIM, "dim"),
        (Mods::REVERSE, "rev"),
        (Mods::UNDERLINE, "ul"),
    ] {
        if m.contains(flag) {
            out.push('+');
            out.push_str(name);
        }
    }
    if out.is_empty() {
        "-".to_string()
    } else {
        out
    }
}
