//! The onboarding mark: a tiled R with independently phased, gentle brightness.

use crate::layout::Rect;
use crate::render::frame::{Color, Frame, Style};

const TILES: [&str; 7] = [
    "111110", "110011", "110011", "111110", "111000", "110110", "110011",
];

/// Render the mark using elapsed time injected by the event loop. A fixed zero
/// time gives a static, readable mark for reduced motion and headless output.
pub(crate) fn draw(frame: &mut Frame, area: Rect, seconds: f32, unicode: bool) {
    for (y, line) in TILES.iter().enumerate() {
        for (x, tile) in line.bytes().enumerate() {
            if tile != b'1' {
                continue;
            }
            let level = brightness(x, y, seconds);
            let style = Style::fg(Color::Rgb(level, level, level));
            let dx = u16::try_from(x * 2).unwrap_or(0);
            let dy = u16::try_from(y).unwrap_or(0);
            let glyph = if unicode { '█' } else { '#' };
            if dx + 1 < area.w && dy < area.h {
                frame.set(area.x + dx, area.y + dy, glyph, style);
                frame.set(area.x + dx + 1, area.y + dy, glyph, style);
            }
        }
    }
}

fn brightness(x: usize, y: usize, seconds: f32) -> u8 {
    let phase = ((x * 19 + y * 31) % 47) as f32 * 0.41;
    let speed = 0.7 + ((x * 7 + y * 11) % 9) as f32 * 0.08;
    let t = if seconds.is_finite() { seconds } else { 0.0 };
    (192.0 + 63.0 * (t * speed + phase).sin()) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staggered_tiles_remain_readable_and_change_at_different_rates() {
        assert_ne!(brightness(0, 0, 1.0), brightness(1, 0, 1.0));
        assert_ne!(brightness(0, 0, 1.0), brightness(0, 0, 2.0));
        for time in [0.0, 1.0, 10.0, f32::NAN] {
            for x in 0..6 {
                for y in 0..7 {
                    assert!(brightness(x, y, time) >= 128);
                }
            }
        }
    }
}
