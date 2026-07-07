//! The dashboard's graph palette — a small, coherent set of truecolor values
//! plus the gamma-aware interpolation the [`crate::plot::Paint`] gradients use.
//!
//! Colour is applied only on the truecolor (xre) backend; the ASCII floor
//! ignores style, so every colour here is purely additive and never changes a
//! graph's *shape*. Values are chosen to read on dark terminals and to keep the
//! four token classes distinguishable (blue / green / violet / amber — a
//! deuteranopia-safe quadruple). Gradients interpolate in linearized light
//! (`GAMMA`) so a green→red ramp has no muddy mid-band.

use crate::render::frame::Color;

/// The Rift accent green (mirrors `view::ACCENT`: title, selection, savings).
pub const ACCENT: Color = Color::Rgb(0x6c, 0xe0, 0x9a);

/// Token-usage bars: a vertical gradient — deep teal at the baseline brightening
/// to `(lo, hi)` green as a bar rises.
pub const TOKEN_LO: Color = Color::Rgb(0x1f, 0x6f, 0x7a);
/// The bright top of the token-bar gradient.
pub const TOKEN_HI: Color = Color::Rgb(0x8c, 0xf0, 0xb4);

/// The four token classes, in `[input, read, output, cache_write]` order — the
/// [`crate::plot::Paint::ByCategory`] palette for the input|read|output section.
pub const BREAKDOWN: [Color; 4] = [
    Color::Rgb(0x5c, 0xa8, 0xff), // input  — uncached (blue)
    Color::Rgb(0x6c, 0xe0, 0x9a), // read   — cache read (green)
    Color::Rgb(0xb4, 0x8c, 0xff), // output — (violet)
    Color::Rgb(0xf0, 0xb4, 0x5c), // cache-write — secondary (amber)
];

/// The nic-gauge thermometer ramp: green (empty) → amber → red (full). Evaluated
/// by the cell's horizontal fraction, so a fuller meter extends into the red.
pub const GAUGE_RAMP: [(f32, Color); 3] = [
    (0.0, Color::Rgb(0x6c, 0xe0, 0x9a)),
    (0.6, Color::Rgb(0xf0, 0xc4, 0x5c)),
    (1.0, Color::Rgb(0xf0, 0x5c, 0x5c)),
];

/// Recall MCP-calls bars — a cool cyan, distinct from the green savings line.
pub const RECALL_CALLS: Color = Color::Rgb(0x5c, 0xc4, 0xe0);

/// The muted foreground the suggest panel's text fades in FROM (ADR-0022) and
/// its descriptions rest at.
pub const MUTED: Color = Color::Rgb(0x5a, 0x64, 0x5a);

/// The settled body-foreground the suggest rows fade TO.
pub const BODY_FG: Color = Color::Rgb(0xd8, 0xdd, 0xd8);

/// The gamma used to linearize sRGB channels before interpolation.
const GAMMA: f32 = 2.2;

/// Linearize one 0–255 sRGB channel to ~linear light.
fn to_linear(c: u8) -> f32 {
    (f32::from(c) / 255.0).powf(GAMMA)
}

/// Convert a linear-light value back to a 0–255 sRGB channel.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn to_srgb(l: f32) -> u8 {
    (l.clamp(0.0, 1.0).powf(1.0 / GAMMA) * 255.0).round() as u8
}

/// Interpolate one channel in linear light (`t` clamped to `[0, 1]`).
fn lerp_channel(a: u8, b: u8, t: f32) -> u8 {
    let la = to_linear(a);
    let lb = to_linear(b);
    to_srgb(la + (lb - la) * t)
}

/// The RGB triple of a colour, or `None` for `Default`/`Indexed` (which carry no
/// numeric channels to blend).
fn rgb_of(c: Color) -> Option<(u8, u8, u8)> {
    match c {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Default | Color::Indexed(_) => None,
    }
}

/// Interpolate `a`→`b` at `t∈[0,1]` in linear light. Non-RGB endpoints don't
/// blend: the nearer endpoint is returned (a total, panic-free fallback).
#[must_use]
pub fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    match (rgb_of(a), rgb_of(b)) {
        (Some((ar, ag, ab)), Some((br, bg, bb))) => Color::Rgb(
            lerp_channel(ar, br, t),
            lerp_channel(ag, bg, t),
            lerp_channel(ab, bb, t),
        ),
        _ if t < 0.5 => a,
        _ => b,
    }
}

/// Sample a `(position, colour)` ramp at `x∈[0,1]`. Stops must be sorted by
/// position; `x` before the first / after the last clamps to that stop. Empty
/// ramp ⇒ the terminal default.
#[must_use]
pub fn ramp_color(stops: &[(f32, Color)], x: f32) -> Color {
    let x = if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let Some((first_pos, first_col)) = stops.first().copied() else {
        return Color::Default;
    };
    if x <= first_pos {
        return first_col;
    }
    for pair in stops.windows(2) {
        let [(lo_pos, lo_col), (hi_pos, hi_col)] = pair else {
            continue;
        };
        if x <= *hi_pos {
            let span = hi_pos - lo_pos;
            let t = if span > f32::EPSILON {
                (x - lo_pos) / span
            } else {
                0.0
            };
            return lerp_color(*lo_col, *hi_col, t);
        }
    }
    stops.last().map_or(Color::Default, |&(_, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(c: Color) -> (u8, u8, u8) {
        match c {
            Color::Rgb(r, g, b) => (r, g, b),
            other => panic!("expected rgb, got {other:?}"),
        }
    }

    #[test]
    fn lerp_endpoints_are_exact() {
        let a = Color::Rgb(0, 0, 0);
        let b = Color::Rgb(255, 255, 255);
        assert_eq!(rgb(lerp_color(a, b, 0.0)), (0, 0, 0));
        assert_eq!(rgb(lerp_color(a, b, 1.0)), (255, 255, 255));
    }

    #[test]
    fn lerp_midpoint_is_between() {
        let a = Color::Rgb(0, 0, 0);
        let b = Color::Rgb(200, 0, 0);
        let (r, _, _) = rgb(lerp_color(a, b, 0.5));
        assert!(r > 0 && r < 200, "midpoint channel {r} strictly between");
    }

    #[test]
    fn lerp_clamps_and_survives_non_finite() {
        let a = Color::Rgb(10, 10, 10);
        let b = Color::Rgb(20, 20, 20);
        assert_eq!(rgb(lerp_color(a, b, -1.0)), (10, 10, 10));
        assert_eq!(rgb(lerp_color(a, b, 2.0)), (20, 20, 20));
        assert_eq!(rgb(lerp_color(a, b, f32::NAN)), (10, 10, 10));
    }

    #[test]
    fn non_rgb_endpoint_falls_back_to_nearer() {
        assert_eq!(
            lerp_color(Color::Default, Color::Rgb(1, 2, 3), 0.2),
            Color::Default
        );
        assert_eq!(
            lerp_color(Color::Default, Color::Rgb(1, 2, 3), 0.9),
            Color::Rgb(1, 2, 3)
        );
    }

    #[test]
    fn ramp_endpoints_and_interior() {
        assert_eq!(ramp_color(&GAUGE_RAMP, 0.0), GAUGE_RAMP[0].1);
        assert_eq!(ramp_color(&GAUGE_RAMP, 1.0), GAUGE_RAMP[2].1);
        // Below the first / above the last clamp.
        assert_eq!(ramp_color(&GAUGE_RAMP, -5.0), GAUGE_RAMP[0].1);
        assert_eq!(ramp_color(&GAUGE_RAMP, 5.0), GAUGE_RAMP[2].1);
        // A mid value sits between the neighbouring stops.
        let mid = rgb(ramp_color(&GAUGE_RAMP, 0.3));
        assert!(mid.0 > 0x6c, "ramp reddens past the green stop: {mid:?}");
    }

    #[test]
    fn empty_ramp_is_default() {
        assert_eq!(ramp_color(&[], 0.5), Color::Default);
    }
}
