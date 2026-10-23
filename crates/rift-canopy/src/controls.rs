//! Shared selection marks for onboarding and project settings.

use crate::render::{Color, Style};

pub(crate) fn toggle(enabled: bool, unicode: bool) -> (char, Style) {
    let glyph = match (enabled, unicode) {
        (true, true) => '●',
        (false, true) => '○',
        (true, false) => '*',
        (false, false) => 'o',
    };
    let color = if enabled {
        crate::theme::HEALTH_OK
    } else {
        Color::Rgb(255, 255, 255)
    };
    (glyph, Style::fg(color))
}
