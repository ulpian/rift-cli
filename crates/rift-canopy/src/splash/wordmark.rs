//! The "Rift" wordmark: ramp-shaded fade-in driver state + the static banner.
//!
//! The Full3d tier (future, behind `xre-3d`) extrudes "Rift" as a mesh and shades
//! it with [`shade`]; the Reduced2d tier reuses the same [`reveal`] sweep over a
//! 2D block banner; the minimal tier shows [`FIGLET_RIFT`] unanimated. Everything
//! here is pure and **total** — [`shade`] sanitizes its inputs so it can never
//! emit NaN/inf, and [`reveal`] is monotone non-decreasing so the fade never
//! flickers and the final frame is deterministic (skip parity).

/// A non-premultiplied linear RGB color, channels in `[0, 1]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb {
    /// Red.
    pub r: f32,
    /// Green.
    pub g: f32,
    /// Blue.
    pub b: f32,
}

/// An RGBA color, channels in `[0, 1]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    /// Red.
    pub r: f32,
    /// Green.
    pub g: f32,
    /// Blue.
    pub b: f32,
    /// Alpha.
    pub a: f32,
}

/// The ramp (shape) shader material: a gradient plus reveal/rim parameters.
#[derive(Clone, Debug)]
pub struct RampMaterial {
    /// Sorted gradient stops `(key, color)` with `key ∈ [0, 1]`.
    pub ramp: Vec<(f32, Rgb)>,
    /// Rim highlight strength on bevels (reads the 3D form).
    pub edge_boost: f32,
    /// `0..1` sweep: how much of the wordmark has ramped in.
    pub reveal: f32,
    /// Direction the reveal travels (upward, out of the crack).
    pub sweep_dir: (f32, f32),
    /// Braille/half-block dithering for sub-cell smoothness when available.
    pub dither: bool,
}

impl RampMaterial {
    /// Rift's brand ramp: void-blue → rift-violet → bright edge.
    #[must_use]
    pub fn rift() -> Self {
        Self {
            ramp: vec![
                (
                    0.0,
                    Rgb {
                        r: 0.05,
                        g: 0.07,
                        b: 0.18,
                    },
                ),
                (
                    0.55,
                    Rgb {
                        r: 0.42,
                        g: 0.27,
                        b: 0.86,
                    },
                ),
                (
                    1.0,
                    Rgb {
                        r: 0.78,
                        g: 0.90,
                        b: 1.0,
                    },
                ),
            ],
            edge_boost: 0.35,
            reveal: 0.0,
            sweep_dir: (0.0, -1.0),
            dither: false,
        }
    }
}

impl Default for RampMaterial {
    fn default() -> Self {
        Self::rift()
    }
}

/// The wordmark animation driver state at a point in time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wordmark {
    /// Timeline position `[0, 1]`.
    pub t: f32,
    /// How much of the wordmark has ramped in `[0, 1]`.
    pub reveal: f32,
}

impl Wordmark {
    /// The wordmark driver at timeline `t`.
    #[must_use]
    pub fn at(t: f32) -> Self {
        Self {
            t: t.clamp(0.0, 1.0),
            reveal: reveal(t),
        }
    }

    /// The deterministic final pose (`t = 1.0`), used by the skip path.
    #[must_use]
    pub fn final_pose() -> Self {
        Self::at(1.0)
    }
}

/// The reveal sweep at timeline `t`: smoothstep over `[0.15, 0.60]` (the spec's
/// "glyphs rise" window), clamped and **monotone non-decreasing** in `t`.
#[must_use]
pub fn reveal(t: f32) -> f32 {
    let t = san(t).clamp(0.0, 1.0);
    let u = ((t - 0.15) / (0.60 - 0.15)).clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u) // smoothstep
}

/// A fixed light direction (already normalized) the shader keys off.
const LIGHT: (f32, f32, f32) = (0.0, 0.55, 0.835);

/// Shade one fragment: normal-shading blended with a positional sweep, sampled
/// through the ramp, with a rim boost and reveal-gated alpha. **Total**: every
/// input is sanitized and every output channel is clamped to `[0, 1]`, so the
/// result is always finite (no NaN/inf reaches a cell).
#[must_use]
pub fn shade(frag_normal: (f32, f32, f32), sweep_coord: f32, m: &RampMaterial) -> Rgba {
    let n = (san(frag_normal.0), san(frag_normal.1), san(frag_normal.2));
    let sweep = san(sweep_coord).clamp(0.0, 1.0);
    let lit = (n.0 * LIGHT.0 + n.1 * LIGHT.1 + n.2 * LIGHT.2).clamp(0.0, 1.0);
    let key = mix(lit, sweep, 0.5).clamp(0.0, 1.0);
    let revealed = if san(m.reveal) >= sweep { 1.0 } else { 0.0 };
    let c = sample_ramp(&m.ramp, key);
    let boost = (san(m.edge_boost) * (1.0 - lit)).clamp(0.0, 1.0);
    Rgba {
        r: (c.r + boost).clamp(0.0, 1.0),
        g: (c.g + boost).clamp(0.0, 1.0),
        b: (c.b + boost).clamp(0.0, 1.0),
        a: revealed,
    }
}

/// Linear interpolation.
fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Map a non-finite value to `0.0`, so the shader is total over all inputs.
fn san(x: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        0.0
    }
}

/// Sample `ramp` (sorted by key) at `key ∈ [0, 1]` with linear interpolation.
fn sample_ramp(ramp: &[(f32, Rgb)], key: f32) -> Rgb {
    let key = key.clamp(0.0, 1.0);
    let Some(&(_, first)) = ramp.first() else {
        return Rgb {
            r: 0.0,
            g: 0.0,
            b: 0.0,
        };
    };
    if key <= ramp.first().map_or(f32::MAX, |&(k, _)| k) {
        return first;
    }
    for pair in ramp.windows(2) {
        let (Some(&(k0, c0)), Some(&(k1, c1))) = (pair.first(), pair.get(1)) else {
            continue; // windows(2) always yields pairs; keep panic-free anyway
        };
        if key <= k1 {
            let span = (k1 - k0).max(f32::EPSILON);
            let f = ((key - k0) / span).clamp(0.0, 1.0);
            return Rgb {
                r: mix(c0.r, c1.r, f),
                g: mix(c0.g, c1.g, f),
                b: mix(c0.b, c1.b, f),
            };
        }
    }
    ramp.last().map_or(first, |&(_, c)| c)
}

/// The minimal-tier static "RIFT" banner (embedded, monochrome).
pub const FIGLET_RIFT: &str = r" ____  ___ _____ _____
|  _ \|_ _|  ___|_   _|
| |_) || || |_    | |
|  _ < | ||  _|   | |
|_| \_\___|_|     |_|";

/// The static banner as lines (for centered drawing).
#[must_use]
pub fn figlet_lines() -> Vec<&'static str> {
    FIGLET_RIFT.lines().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_is_clamped_and_endpoints() {
        assert_eq!(reveal(-1.0), 0.0);
        assert_eq!(reveal(0.0), 0.0);
        assert!((reveal(1.0) - 1.0).abs() < 1e-6);
        assert_eq!(reveal(2.0), reveal(1.0));
    }

    #[test]
    fn final_pose_is_deterministic() {
        let a = Wordmark::final_pose();
        let b = Wordmark::final_pose();
        assert_eq!(a, b);
        assert!((a.reveal - 1.0).abs() < 1e-6);
    }

    #[test]
    fn figlet_banner_is_rift() {
        let lines = figlet_lines();
        assert_eq!(lines.len(), 5);
        assert!(FIGLET_RIFT.contains('_'));
    }

    #[test]
    fn shade_is_finite_for_nan_inputs() {
        let m = RampMaterial::rift();
        let c = shade((f32::NAN, f32::INFINITY, -f32::INFINITY), f32::NAN, &m);
        for ch in [c.r, c.g, c.b, c.a] {
            assert!(ch.is_finite());
            assert!((0.0..=1.0).contains(&ch));
        }
    }

    proptest::proptest! {
        #[test]
        fn reveal_is_monotonic(t0 in -0.5f32..1.5, d in 0.0f32..2.0) {
            let t1 = t0 + d;
            proptest::prop_assert!(reveal(t1) >= reveal(t0) - 1e-6);
        }

        #[test]
        fn shade_channels_in_unit_range(
            nx in -10.0f32..10.0, ny in -10.0f32..10.0, nz in -10.0f32..10.0,
            sweep in -1.0f32..2.0, reveal_v in 0.0f32..1.0,
        ) {
            let mut m = RampMaterial::rift();
            m.reveal = reveal_v;
            let c = shade((nx, ny, nz), sweep, &m);
            for ch in [c.r, c.g, c.b, c.a] {
                proptest::prop_assert!(ch.is_finite());
                proptest::prop_assert!((0.0..=1.0).contains(&ch));
            }
        }
    }
}
