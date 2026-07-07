//! Drawing the splash fidelity tiers into Canopy's [`Frame`].
//!
//! The Reduced2d tier draws the [`ground`] crack as Unicode glyphs and the
//! [`wordmark`] as a rising, ramp-colored "RIFT" banner; the StaticAscii tier
//! draws the figlet banner; Full3d compiles the [`mesh3d`] scene (the ground
//! meshed from the crack field, the extruded wordmark) to its deterministic
//! point set and density-resolves it — braille, half-blocks where the caps or
//! the budget ladder's [`Resolution`] demand, and the backend's `render_scene`
//! banner where Unicode is absent (never an error). Every tier is a
//! deterministic function of `(t, seed[, resolution])`, so the keyframe
//! goldens and the skip → final-frame parity hold.

use super::director::Tier;
use super::ground::CrackField;
use super::mesh3d::{self, Resolution};
use super::wordmark::{figlet_lines, reveal};
use crate::render::frame::{Color, Frame, Style};
use crate::render::{draw_centered, RenderBackend, SceneRef};

/// The crack glyph ramp by sink depth (shallow → deep).
const CRACK_RAMP: [char; 4] = ['░', '▒', '▓', '█'];

/// Draw the splash at timeline `t ∈ [0, 1]` for `tier`, seeded by `seed`.
/// `res` is the Full3d raster density (the budget ladder's first downgrade
/// axis); the other tiers ignore it.
pub fn draw_tier(
    tier: Tier,
    t: f32,
    seed: u64,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    caps: crate::render::Capabilities,
    res: Resolution,
) {
    let area = frame.rect();
    match tier {
        Tier::Instant => {}
        Tier::StaticAscii => {
            let lines = figlet_lines();
            draw_centered(frame, area, &lines);
        }
        Tier::Reduced2d => {
            draw_crack(frame, t, seed);
            draw_wordmark_2d(frame, t);
        }
        Tier::Full3d => {
            // The mesh raster (ADR-0014): a deterministic point set resolved
            // by density. Where even Unicode is absent, the render seam's
            // banner fallback keeps the entrypoint total.
            let points = mesh3d::scene_points(t, seed, res, (area.w, area.h));
            if !mesh3d::resolve(frame, area, &points, caps, res) {
                let lines = figlet_lines();
                let scene = SceneRef {
                    fallback: lines.as_slice(),
                };
                backend.render_scene(&scene, area, frame);
            }
        }
    }
}

/// Draw the ground crack: a fresh seeded field advanced to `t`, so the output is
/// a pure function of `(t, seed)` (deterministic keyframes + skip parity).
fn draw_crack(frame: &mut Frame, t: f32, seed: u64) {
    let area = frame.rect();
    if area.w == 0 || area.h == 0 {
        return;
    }
    let t = t.clamp(0.0, 1.0);
    let mut crack = CrackField::new(area.w, area.h, (area.w / 2, area.h / 2), seed);
    // Advance proportionally to t (budget = perimeter-ish; bounded, terminating).
    let budget = u32::from(area.w) + u32::from(area.h);
    let target = (t * budget as f32) as u32;
    crack.run_to_completion(0.18, target);

    let depth = (t * t).clamp(0.0, 1.0); // ease-in sink for opened cells at time t
    let idx = ((depth * (CRACK_RAMP.len() as f32 - 1.0)) as usize).min(CRACK_RAMP.len() - 1);
    #[allow(clippy::indexing_slicing)] // idx clamped to CRACK_RAMP.len() - 1 above
    let glyph = CRACK_RAMP[idx];
    let style = Style::fg(Color::Rgb(0x35, 0x2b, 0x5e)); // void violet, dim
    for y in 0..area.h {
        for x in 0..area.w {
            if crack.is_open(x, y) {
                frame.set(x, y, glyph, style);
            }
        }
    }
}

/// Draw the "RIFT" wordmark rising from the crack: the figlet banner revealed
/// bottom-up by `reveal(t)`, tinted with the brand violet.
fn draw_wordmark_2d(frame: &mut Frame, t: f32) {
    let area = frame.rect();
    let lines = figlet_lines();
    let rows = lines.len();
    if rows == 0 || area.h == 0 {
        return;
    }
    let rev = reveal(t);
    let visible = (rev * rows as f32).ceil() as usize;
    if visible == 0 {
        return;
    }
    let banner_h = u16::try_from(rows).unwrap_or(u16::MAX);
    let top = area.y + area.h.saturating_sub(banner_h) / 2;
    let style = Style::fg(Color::Rgb(0x9b, 0x8c, 0xff)); // rift violet
                                                         // Reveal the lowest `visible` rows (a rise out of the chasm).
    for (i, line) in lines.iter().enumerate() {
        if i + visible < rows {
            continue; // not yet revealed
        }
        let Ok(dy) = u16::try_from(i) else { break };
        let y = top + dy;
        let lw = u16::try_from(line.chars().count()).unwrap_or(0);
        let x = area.x + area.w.saturating_sub(lw) / 2;
        frame.put_str(x, y, line, style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::frame::Size;
    use crate::render::Capabilities;

    struct NullBackend;
    impl RenderBackend for NullBackend {
        fn name(&self) -> &'static str {
            "null"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::empty()
        }
        fn present(&mut self, _f: &Frame) -> Result<(), crate::render::RenderError> {
            Ok(())
        }
    }

    fn text_of(f: &Frame) -> String {
        let mut s = String::new();
        for y in 0..f.height() {
            for x in 0..f.width() {
                s.push(f.get(x, y).map_or(' ', |c| c.ch));
            }
            s.push('\n');
        }
        s
    }

    fn full3d_caps() -> Capabilities {
        Capabilities::TRUECOLOR
            | Capabilities::UNICODE
            | Capabilities::SUBCELL
            | Capabilities::MESH_3D
            | Capabilities::RAMP_SHADER
    }

    #[test]
    fn static_tier_draws_banner() {
        let mut f = Frame::new(Size::new(40, 10));
        let mut b = NullBackend;
        draw_tier(
            Tier::StaticAscii,
            0.0,
            1,
            &mut b,
            &mut f,
            Capabilities::empty(),
            Resolution::Full,
        );
        assert!(text_of(&f).contains('_'));
    }

    #[test]
    fn reduced_tier_is_deterministic_for_seed_and_t() {
        let mut b = NullBackend;
        let mut f1 = Frame::new(Size::new(60, 20));
        let mut f2 = Frame::new(Size::new(60, 20));
        draw_tier(
            Tier::Reduced2d,
            0.5,
            99,
            &mut b,
            &mut f1,
            Capabilities::empty(),
            Resolution::Full,
        );
        draw_tier(
            Tier::Reduced2d,
            0.5,
            99,
            &mut b,
            &mut f2,
            Capabilities::empty(),
            Resolution::Full,
        );
        assert_eq!(text_of(&f1), text_of(&f2));
    }

    #[test]
    fn reduced_tier_final_frame_shows_full_wordmark() {
        let mut b = NullBackend;
        let mut f = Frame::new(Size::new(60, 20));
        draw_tier(
            Tier::Reduced2d,
            1.0,
            7,
            &mut b,
            &mut f,
            Capabilities::empty(),
            Resolution::Full,
        );
        // At t=1 reveal is full, so every figlet row is present.
        let text = text_of(&f);
        for line in figlet_lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                assert!(text.contains(trimmed.split_whitespace().next().unwrap()));
            }
        }
    }

    #[test]
    fn instant_tier_draws_nothing() {
        let mut b = NullBackend;
        let mut f = Frame::new(Size::new(20, 5));
        draw_tier(
            Tier::Instant,
            0.5,
            1,
            &mut b,
            &mut f,
            Capabilities::empty(),
            Resolution::Full,
        );
        assert!(text_of(&f).trim().is_empty());
    }

    #[test]
    fn full3d_without_unicode_draws_banner_fallback() {
        // Even with the 3D capability bits, no Unicode → the render seam's
        // static banner (never an error, never a blank).
        let mut b = NullBackend;
        let mut f = Frame::new(Size::new(40, 10));
        draw_tier(
            Tier::Full3d,
            0.5,
            1,
            &mut b,
            &mut f,
            Capabilities::empty(),
            Resolution::Full,
        );
        assert!(text_of(&f).contains('_'));
    }

    #[test]
    fn full3d_is_deterministic_and_draws_the_mesh_raster() {
        let mut b = NullBackend;
        let mut f1 = Frame::new(Size::new(60, 20));
        let mut f2 = Frame::new(Size::new(60, 20));
        for f in [&mut f1, &mut f2] {
            draw_tier(
                Tier::Full3d,
                0.5,
                99,
                &mut b,
                f,
                full3d_caps(),
                Resolution::Full,
            );
        }
        assert_eq!(f1, f2, "same (t, seed, res) → identical cells and colours");
        // The raster resolves as braille density, not the banner.
        let text = text_of(&f1);
        assert!(
            text.chars()
                .any(|c| (0x2800..=0x28FF).contains(&(c as u32))),
            "braille glyphs present: {text}"
        );
    }

    #[test]
    fn full3d_half_resolution_coarsens_to_half_blocks() {
        // The budget ladder's first rung: same tier, half-block density.
        let mut b = NullBackend;
        let mut f = Frame::new(Size::new(60, 20));
        draw_tier(
            Tier::Full3d,
            1.0,
            7,
            &mut b,
            &mut f,
            full3d_caps(),
            Resolution::Half,
        );
        let text = text_of(&f);
        assert!(text.chars().any(|c| matches!(c, '▀' | '▄' | '█')));
        assert!(
            !text
                .chars()
                .any(|c| (0x2800..=0x28FF).contains(&(c as u32))),
            "no braille at half resolution"
        );
    }

    #[test]
    fn tiny_area_does_not_panic() {
        let mut b = NullBackend;
        for (w, h) in [(0, 0), (1, 1), (3, 2)] {
            let mut f = Frame::new(Size::new(w, h));
            draw_tier(
                Tier::Reduced2d,
                0.5,
                1,
                &mut b,
                &mut f,
                Capabilities::empty(),
                Resolution::Full,
            );
            let mut f3 = Frame::new(Size::new(w, h));
            draw_tier(
                Tier::Full3d,
                0.5,
                1,
                &mut b,
                &mut f3,
                full3d_caps(),
                Resolution::Half,
            );
        }
    }
}
