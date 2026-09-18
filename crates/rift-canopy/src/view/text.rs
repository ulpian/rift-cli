//! Text clipping and eliding: every surface writes a row through one of
//! these, so a panel never bleeds into its neighbour and an over-long label
//! is cut with its ellipsis at the end (or, for a path, at the start).

use crate::layout::Rect;
use crate::render::frame::{Frame, Style};

/// Print `text` on row `dy` of `area`, truncated to the area's width and dropped
/// when it falls below the area — so a panel never bleeds into its neighbor.
pub(super) fn clip_line(frame: &mut Frame, area: Rect, dy: u16, text: &str, style: Style) {
    if dy >= area.h {
        return;
    }
    let max = usize::from(area.w);
    let clipped: String = text.chars().take(max).collect();
    frame.put_str(area.x, area.y + dy, &clipped, style);
}

/// Print `text` at `(x, y)`, clipped to `width` cells.
pub(super) fn clip_at(frame: &mut Frame, x: u16, y: u16, width: u16, text: &str, style: Style) {
    let clipped: String = text.chars().take(usize::from(width)).collect();
    frame.put_str(x, y, &clipped, style);
}

/// Truncate `s` to at most `w` display cells, appending an ellipsis when it is cut.
pub(super) fn elide(s: &str, w: usize) -> String {
    let count = s.chars().count();
    if count <= w {
        return s.to_string();
    }
    match w {
        0 => String::new(),
        1 => "…".to_string(),
        _ => {
            let mut out: String = s.chars().take(w - 1).collect();
            out.push('…');
            out
        }
    }
}

/// Truncate `s` to at most `w` cells from the **left**, prefixing an ellipsis
/// when it is cut — the mirror of [`elide`], for paths, where the tail (the
/// basename) is the part that identifies what you are looking at.
pub(super) fn elide_left(s: &str, w: usize) -> String {
    let count = s.chars().count();
    if count <= w {
        return s.to_string();
    }
    match w {
        0 => String::new(),
        1 => "…".to_string(),
        _ => {
            let mut out = String::from("…");
            out.extend(s.chars().skip(count - (w - 1)));
            out
        }
    }
}
