//! The Full3d splash raster (ADR-0014): mesh → shaded samples → cells.
//!
//! The ground is **meshed from the deterministic crack field** (one quad — two
//! triangles — per grid cell, corner heights from the crack's sink depth) and
//! the wordmark is the **extruded** figlet mask; both are shaded through the
//! [`super::wordmark::shade`] luminance ramp. The scene compiles to a
//! **deterministic point set** — a pure function of `(t, seed, resolution,
//! size)`, no wall clock, no global RNG — which [`resolve`] maps to terminal
//! cells via **braille density**, degrading to **half-blocks** where the
//! capability lattice demands (no [`Capabilities::SUBCELL`], or the budget
//! ladder's [`Resolution::Half`]) and telling the caller to banner-fallback
//! where Unicode is absent entirely.
//!
//! This is the pinned engine's render-layer model (rasterize into a sub-cell
//! sample grid, resolve by density shader) implemented engine-free, so the
//! math, its determinism, and the skip parity are tested in every build; the
//! `xre-3d` feature's role is capability advertising ([`Capabilities::MESH_3D`]
//! / [`Capabilities::RAMP_SHADER`], see `render::caps::derive_caps`) plus the
//! full-fidelity truecolor present. Determinism note: only `+ - * / sqrt`
//! comparisons and clamps — no trig, no FMA (`mul_add`) — so keyframes are
//! bit-identical across platforms (the engine's own golden-frame rule).

use super::ground::CrackField;
use super::prng::Pcg32;
use super::wordmark::{figlet_lines, reveal, shade, RampMaterial, Rgb};
use crate::layout::Rect;
use crate::render::frame::{Color, Frame, Style};
use crate::render::Capabilities;

/// The Full3d sampling density — the budget ladder's first downgrade axis
/// (ADR-0014: "resolution downgrades first, then the tier").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Braille-density sampling: 2×4 samples per terminal cell.
    Full,
    /// Half-block-density sampling: 1×2 samples per cell (≈ 4× cheaper).
    Half,
}

impl Resolution {
    /// Samples per cell `(x, y)` at this resolution.
    #[must_use]
    pub const fn samples_per_cell(self) -> (u32, u32) {
        match self {
            Self::Full => (2, 4),
            Self::Half => (1, 2),
        }
    }
}

/// One shaded scene sample: a normalized position (plot convention — `x` 0
/// left → 1 right, `y` 0 bottom → 1 top) plus its luminance key and ramp
/// colour. The keyframe goldens pin these as quantized point sets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScenePoint {
    /// Horizontal position in `[0, 1]`.
    pub x: f32,
    /// Vertical position in `[0, 1]` (bottom-up).
    pub y: f32,
    /// The shading key in `[0, 1]`: drives dot density (dithered) and picks
    /// the cell's representative colour (the brightest sample wins).
    pub lum: f32,
    /// The ramp-shaded colour (quantized once, at shading time).
    pub rgb: (u8, u8, u8),
}

// --- the scene constants (normalized space; y up) ---------------------------

/// Ground mesh columns (scene detail is a scene property; the raster adapts).
const GROUND_W: u16 = 46;
/// Ground mesh rows.
const GROUND_H: u16 = 18;
/// Screen y of the ground's near edge (`v = 0`).
const GROUND_NEAR_Y: f32 = 0.05;
/// Screen-y span from near edge to far edge (`v = 1`).
const GROUND_SPAN_Y: f32 = 0.50;
/// Screen width of the ground's near edge.
const GROUND_NEAR_W: f32 = 0.96;
/// How much the far edge pinches toward the center (fake perspective).
const GROUND_PINCH: f32 = 0.34;
/// Screen-y drop of a fully sunken cell (the crack opening downward).
const SINK_DROP_Y: f32 = 0.05;
/// World-height exaggeration for slope lighting (grid units per unit sink).
const HEIGHT_EXAG: f32 = 2.5;
/// How strongly a sunken sample darkens (`1 - SINK_DARKEN * sink`): a fully
/// sunken sample falls below [`LUM_FLOOR`], so the settled crack reads as
/// true holes in the dot field, with dim seams along its thin arms.
const SINK_DARKEN: f32 = 0.92;

/// Wordmark screen width (centered).
const WM_WIDTH: f32 = 0.62;
/// Wordmark row height on screen.
const WM_ROW_H: f32 = 0.062;
/// Wordmark base (bottom edge) before the rise.
const WM_BASE_Y: f32 = 0.16;
/// How far the wordmark rises out of the crack over the reveal.
const WM_RISE: f32 = 0.30;
/// Extrusion layers behind the front face.
const WM_EXTRUDE_LAYERS: u32 = 2;
/// Per-layer extrusion screen offset (x rightward, y downward).
const WM_EXTRUDE_DX: f32 = 0.007;
const WM_EXTRUDE_DY: f32 = 0.013;

/// Samples below this luminance are dropped at generation time (invisible at
/// every dither threshold; keeps the point set lean and the goldens readable).
const LUM_FLOOR: f32 = 0.045;

/// The ordered-dither thresholds for braille dots (4 rows × 2 columns, Bayer
/// order): a dot lights iff its sample's luminance exceeds its threshold, so
/// dot *density* encodes luminance — the density-resolve half of ADR-0014.
const DITHER_BRAILLE: [[f32; 2]; 4] = [
    [0.0625, 0.5625],
    [0.8125, 0.3125],
    [0.1875, 0.6875],
    [0.9375, 0.4375],
];

/// The ordered-dither thresholds for the half-block fallback (2 rows).
const DITHER_HALF: [f32; 2] = [0.18, 0.55];

/// The ground material: a dim void-violet ramp (the wordmark keeps the brand
/// ramp from [`RampMaterial::rift`]); always fully revealed.
fn ground_material() -> RampMaterial {
    RampMaterial {
        ramp: vec![
            (
                0.0,
                Rgb {
                    r: 0.05,
                    g: 0.05,
                    b: 0.13,
                },
            ),
            (
                0.55,
                Rgb {
                    r: 0.30,
                    g: 0.24,
                    b: 0.55,
                },
            ),
            (
                1.0,
                Rgb {
                    r: 0.55,
                    g: 0.48,
                    b: 0.86,
                },
            ),
        ],
        edge_boost: 0.16,
        reveal: 1.0,
        sweep_dir: (0.0, -1.0),
        dither: true,
    }
}

/// The wordmark side-face material: the brand ramp, dimmed (extrusion walls).
fn side_material(revealed: f32) -> RampMaterial {
    let mut m = RampMaterial::rift();
    m.edge_boost = 0.05;
    m.reveal = revealed;
    m
}

// --- small deterministic vector helpers -------------------------------------

/// Cross product.
fn cross(a: (f32, f32, f32), b: (f32, f32, f32)) -> (f32, f32, f32) {
    (
        a.1 * b.2 - a.2 * b.1,
        a.2 * b.0 - a.0 * b.2,
        a.0 * b.1 - a.1 * b.0,
    )
}

/// Normalize (degenerate input → the flat "up" normal, so shading stays total).
fn normalize(v: (f32, f32, f32)) -> (f32, f32, f32) {
    let len2 = v.0 * v.0 + v.1 * v.1 + v.2 * v.2;
    if len2 <= f32::EPSILON || !len2.is_finite() {
        return (0.0, 1.0, 0.0);
    }
    let inv = 1.0 / len2.sqrt();
    (v.0 * inv, v.1 * inv, v.2 * inv)
}

/// Rec. 709 luma of the shader's linear channels (the density key).
fn luma(r: f32, g: f32, b: f32) -> f32 {
    (0.2126 * r + 0.7152 * g + 0.0722 * b).clamp(0.0, 1.0)
}

/// Quantize a `[0, 1]` channel to a byte (round-half-up; deterministic).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn to_byte(ch: f32) -> u8 {
    (ch.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

// --- the ground mesh ---------------------------------------------------------

/// How many seeded crack fields form the fracture network. A single field can
/// die out within a few ticks on an unlucky seed (the frontier is
/// probabilistic); a small union of deterministically-derived fields keeps
/// the settled ground visibly fractured for every seed while each field stays
/// the same bounded, terminating crack model the 2D tier ships.
const FRACTURE_FIELDS: u32 = 4;

/// Per-field open-cell cap: without it a lucky field floods the grid and the
/// settled ground vanishes entirely; with it each fissure is a bounded patch
/// and the union stays a fracture network over a surviving surface.
const FIELD_OPEN_CAP: usize = 40;

/// The deterministic fracture network: `FRACTURE_FIELDS` crack fields whose
/// origins and PRNG streams derive from the one splash seed, each advanced
/// proportionally to `t` (the 2D tier's recipe), read as a union.
struct Fracture {
    fields: Vec<CrackField>,
}

impl Fracture {
    fn grown(seed: u64, t: f32) -> Self {
        let budget = u32::from(GROUND_W) + u32::from(GROUND_H);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let target = (t.clamp(0.0, 1.0) * budget as f32) as u32;
        let mut origin_rng = Pcg32::seed(seed, 0x9e37_79b9_7f4a_7c15);
        let mut fields = Vec::with_capacity(FRACTURE_FIELDS as usize);
        for k in 0..FRACTURE_FIELDS {
            // Field 0 cracks at the grid center (the wordmark rises from it);
            // the others at seed-derived origins in the middle band.
            let origin = if k == 0 {
                (GROUND_W / 2, GROUND_H / 2)
            } else {
                #[allow(clippy::cast_possible_truncation)]
                let ox =
                    (origin_rng.below(u32::from(GROUND_W / 2)) + u32::from(GROUND_W / 4)) as u16;
                #[allow(clippy::cast_possible_truncation)]
                let oy =
                    (origin_rng.below(u32::from(GROUND_H / 2)) + u32::from(GROUND_H / 4)) as u16;
                (ox, oy)
            };
            let mut field = CrackField::new(
                GROUND_W,
                GROUND_H,
                origin,
                seed.wrapping_add(u64::from(k).wrapping_mul(0xa076_1d64_78bd_642f)),
            );
            // Advance to the t-proportional tick target, additionally capped
            // by open count. Growth is a deterministic prefix sequence, so
            // the state at any t₁ < t₂ is a prefix of the t₂ state — the
            // monotonicity the skip path's identical final surface rides on.
            while !field.done() && field.ticks() < target && field.open_count() < FIELD_OPEN_CAP {
                field.step(0.18);
            }
            fields.push(field);
        }
        Self { fields }
    }

    /// The union sink depth of a cell at time `t` (0 closed everywhere, `t²`
    /// open in any field — the same monotone easing as the 2D tier, so the
    /// skip path lands on an identical final surface).
    fn sink(&self, x: u16, y: u16, t: f32) -> f32 {
        self.fields
            .iter()
            .map(|f| f.depth_at(x, y, t))
            .fold(0.0, f32::max)
    }
}

/// A grid-corner height: the mean sink of the (≤ 4) cells sharing the corner.
/// Meshing the crack field means adjacent cells share corners — a connected
/// surface, not per-cell tiles.
fn corner_sink(fracture: &Fracture, cx: u16, cy: u16, t: f32) -> f32 {
    let mut sum = 0.0;
    for dx in 0..2u16 {
        for dy in 0..2u16 {
            let (x, x_underflow) = cx.overflowing_sub(1 - dx);
            let (y, y_underflow) = cy.overflowing_sub(1 - dy);
            // Out-of-grid neighbors contribute 0 (a flat rim keeps the border
            // closed); underflow means "outside", and `depth_at` is itself
            // bounds-checked past the far edge.
            if !x_underflow && !y_underflow {
                sum += fracture.sink(x, y, t);
            }
        }
    }
    sum * 0.25
}

/// Project a ground point `(u, v, sink)` (`u` across, `v` 0 near → 1 far,
/// `sink ∈ [0, 1]`) to normalized screen space (fake-perspective pinch; a
/// sunken point drops on screen).
fn project_ground(u: f32, v: f32, sink: f32) -> (f32, f32) {
    let width = GROUND_NEAR_W - GROUND_PINCH * v;
    let x = 0.5 + (u - 0.5) * width;
    let y = GROUND_NEAR_Y + v * GROUND_SPAN_Y - sink * SINK_DROP_Y;
    (x, y)
}

/// Emit the ground into `out`: for every mesh cell, flat-shade its two
/// triangles from their world normals and splat samples on the raster
/// lattice covering the cell's screen quad.
#[allow(clippy::cast_precision_loss)] // grid indices ≤ 46 — exact in f32
fn emit_ground(t: f32, seed: u64, lattice: (u32, u32), out: &mut Vec<ScenePoint>) {
    let fracture = Fracture::grown(seed, t);
    let material = ground_material();
    let gw = f32::from(GROUND_W);
    let gh = f32::from(GROUND_H);

    for cy in 0..GROUND_H {
        for cx in 0..GROUND_W {
            // The quad's corner sinks (shared with neighbors — one connected
            // surface for the geometry and the slope shading) …
            let s00 = corner_sink(&fracture, cx, cy, t);
            let s10 = corner_sink(&fracture, cx + 1, cy, t);
            let s01 = corner_sink(&fracture, cx, cy + 1, t);
            let s11 = corner_sink(&fracture, cx + 1, cy + 1, t);
            // … and the cell's own fracture void: the darkening is per
            // fractured cell (a surface break, not a smooth dip), so the
            // settled crack reads as holes exactly one mesh cell wide while
            // the smoothed corners keep a lit rim around them.
            let void = fracture.sink(cx, cy, t);

            // World corners for the normals (y up = -sink, exaggerated).
            let corner_at = |dx: u16, dy: u16, sink: f32| {
                (f32::from(cx + dx), -sink * HEIGHT_EXAG, f32::from(cy + dy))
            };
            let (p00, p10, p01, p11) = (
                corner_at(0, 0, s00),
                corner_at(1, 0, s10),
                corner_at(0, 1, s01),
                corner_at(1, 1, s11),
            );
            let n_a = ground_normal(p00, p10, p01); // triangle u+v ≤ 1
            let n_b = ground_normal(p11, p01, p10); // triangle u+v > 1

            // Sample the quad on the raster lattice: enough samples per axis
            // that every lattice dot the cell's screen extent spans receives
            // one (an upper bound on the extent — no holes at any size).
            let (u0, v0) = (f32::from(cx) / gw, f32::from(cy) / gh);
            let (u1, v1) = (f32::from(cx + 1) / gw, f32::from(cy + 1) / gh);
            let max_sink = s00.max(s10).max(s01).max(s11);
            let span_x = (u1 - u0) * (GROUND_NEAR_W - GROUND_PINCH * v0);
            let span_y = (v1 - v0) * GROUND_SPAN_Y + max_sink * SINK_DROP_Y;
            let (nx, ny) = coverage_counts(span_x, span_y, lattice);
            for iy in 0..ny {
                for ix in 0..nx {
                    #[allow(clippy::cast_precision_loss)] // counts ≤ 64
                    let fu = (ix as f32 + 0.5) / nx as f32;
                    #[allow(clippy::cast_precision_loss)]
                    let fv = (iy as f32 + 0.5) / ny as f32;
                    let u = u0 + (u1 - u0) * fu;
                    let v = v0 + (v1 - v0) * fv;
                    // Bilinear sink at the sample.
                    let sink = (s00 * (1.0 - fu) + s10 * fu) * (1.0 - fv)
                        + (s01 * (1.0 - fu) + s11 * fu) * fv;
                    let normal = if fu + fv <= 1.0 { n_a } else { n_b };
                    // Far rows dim: the shade sweep runs bright-near → dark-far.
                    let shaded = shade(normal, 1.0 - v, &material);
                    let darken = 1.0 - SINK_DARKEN * void.max(sink * 0.5).clamp(0.0, 1.0);
                    let (red, green, blue) =
                        (shaded.r * darken, shaded.g * darken, shaded.b * darken);
                    let lum = luma(red, green, blue);
                    if lum <= LUM_FLOOR {
                        continue;
                    }
                    let (px, py) = project_ground(u, v, sink);
                    out.push(ScenePoint {
                        x: px,
                        y: py,
                        lum,
                        rgb: (to_byte(red), to_byte(green), to_byte(blue)),
                    });
                }
            }
        }
    }
}

/// A ground triangle's unit normal, oriented upward (`y ≥ 0`).
fn ground_normal(a: (f32, f32, f32), b: (f32, f32, f32), c: (f32, f32, f32)) -> (f32, f32, f32) {
    let e1 = (b.0 - a.0, b.1 - a.1, b.2 - a.2);
    let e2 = (c.0 - a.0, c.1 - a.1, c.2 - a.2);
    let n = normalize(cross(e1, e2));
    if n.1 < 0.0 {
        (-n.0, -n.1, -n.2)
    } else {
        n
    }
}

/// The per-axis sample counts for a screen span (normalized units) on the
/// raster lattice: at least one sample per lattice dot the span covers, so
/// coverage is hole-free at any terminal size; clamped (1..=64) so degenerate
/// and hostile sizes both stay cheap. Deterministic in its inputs.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn coverage_counts(span_x: f32, span_y: f32, (dots_x, dots_y): (u32, u32)) -> (u32, u32) {
    #[allow(clippy::cast_precision_loss)] // dot grids are ≤ ~10⁴ — exact in f32
    let nx = ((span_x.abs() * dots_x as f32).ceil() as u32).clamp(1, 64);
    #[allow(clippy::cast_precision_loss)]
    let ny = ((span_y.abs() * dots_y as f32).ceil() as u32).clamp(1, 64);
    (nx, ny)
}

// --- the extruded wordmark ---------------------------------------------------

/// Emit the wordmark: the figlet mask extruded `WM_EXTRUDE_LAYERS` deep
/// (side layers first, then the front face, so the brightest fragments win
/// the cell), shaded through the brand ramp, rising with `reveal(t)` and
/// reveal-gated bottom-up (the sweep out of the crack).
#[allow(clippy::cast_precision_loss)] // mask dimensions ≤ ~32 — exact in f32
fn emit_wordmark(t: f32, lattice: (u32, u32), out: &mut Vec<ScenePoint>) {
    let lines = figlet_lines();
    let rows = lines.len();
    if rows == 0 {
        return;
    }
    let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    if cols == 0 {
        return;
    }
    let rev = reveal(t);
    let px_w = WM_WIDTH / cols as f32;
    let base_y = WM_BASE_Y + WM_RISE * rev;
    let x0 = 0.5 - WM_WIDTH * 0.5;

    // Precompute per-layer normals: side walls tilt right/down; front faces
    // the light. Normalized with the same deterministic helper.
    let front_n = (0.0, 0.0, 1.0);
    let side_n = normalize((0.55, -0.35, 0.76));
    let front_m = {
        let mut m = RampMaterial::rift();
        m.reveal = rev;
        m
    };
    let side_m = side_material(rev);

    // Deepest layer first — the front face is emitted last and, being the
    // brightest, wins the per-cell colour (later wins ties as well).
    for layer in (0..=WM_EXTRUDE_LAYERS).rev() {
        let is_front = layer == 0;
        let lf = layer as f32;
        let (dx, dy) = (WM_EXTRUDE_DX * lf, -(WM_EXTRUDE_DY * lf));
        let (normal, material) = if is_front {
            (front_n, &front_m)
        } else {
            (side_n, &side_m)
        };
        for (row, line) in lines.iter().enumerate() {
            // The reveal sweep coordinate: bottom row low → top row high,
            // offset half a row so nothing is revealed at t = 0 (the 2D
            // tier's "nothing before the sweep starts" behavior).
            let sweep = ((rows - 1 - row) as f32 + 0.5) / rows as f32;
            let shaded = shade(normal, sweep, material);
            if shaded.a < 0.5 {
                continue; // not yet revealed — the row is still in the crack
            }
            let lum_scale = if is_front { 1.0 } else { 0.55 };
            let (red, green, blue) = (
                shaded.r * lum_scale,
                shaded.g * lum_scale,
                shaded.b * lum_scale,
            );
            let mut lum = luma(red, green, blue);
            if is_front {
                // The front face rides high on the density key so the glyph
                // shapes stay solid (legible) while the ramp still carries
                // the colour gradient; the extrusion walls stay mid-density,
                // which is the depth cue.
                lum = (0.62 + 0.38 * lum).clamp(0.0, 1.0);
            }
            if lum <= LUM_FLOOR {
                continue;
            }
            let rgb = (to_byte(red), to_byte(green), to_byte(blue));
            let py0 = base_y + (rows - 1 - row) as f32 * WM_ROW_H + dy;
            for (col, ch) in line.chars().enumerate() {
                if ch == ' ' {
                    continue;
                }
                let px0 = x0 + col as f32 * px_w + dx;
                splat_rect(px0, py0, px_w, WM_ROW_H, lum, rgb, lattice, out);
            }
        }
    }
}

/// Splat an axis-aligned screen rect onto the raster lattice: one sample per
/// covered lattice dot (hole-free at any size), each carrying the rect's
/// shading. Bounded sampling (≤ 64 per axis) keeps hostile sizes cheap.
fn splat_rect(
    x0: f32,
    y0: f32,
    w: f32,
    h: f32,
    lum: f32,
    rgb: (u8, u8, u8),
    lattice: (u32, u32),
    out: &mut Vec<ScenePoint>,
) {
    let (nx, ny) = coverage_counts(w, h, lattice);
    for iy in 0..ny {
        for ix in 0..nx {
            #[allow(clippy::cast_precision_loss)]
            let fx = (ix as f32 + 0.5) / nx as f32;
            #[allow(clippy::cast_precision_loss)]
            let fy = (iy as f32 + 0.5) / ny as f32;
            out.push(ScenePoint {
                x: x0 + w * fx,
                y: y0 + h * fy,
                lum,
                rgb,
            });
        }
    }
}

// --- the public surface --------------------------------------------------------

/// Compile the Full3d scene at timeline `t ∈ [0, 1]` into its deterministic
/// point set: a pure function of `(t, seed, res, size)` — the keyframe
/// contract (injected clock upstream; seeded PRNG only; no wall clock here).
/// Points outside `[0, 1]²` are possible while the wordmark rises past the
/// top of a tiny scene and are dropped by [`resolve`]'s contract.
#[must_use]
pub fn scene_points(t: f32, seed: u64, res: Resolution, size: (u16, u16)) -> Vec<ScenePoint> {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let (sx, sy) = res.samples_per_cell();
    let lattice = (u32::from(size.0.max(1)) * sx, u32::from(size.1.max(1)) * sy);
    let mut out = Vec::new();
    emit_ground(t, seed, lattice, &mut out);
    emit_wordmark(t, lattice, &mut out);
    out
}

/// Per-cell accumulation for the density resolve.
struct CellAcc {
    mask: u32,
    lum: f32,
    rgb: (u8, u8, u8),
}

/// Resolve a scene point set into `area` of `frame` by density:
///
/// - braille dots (2×4 per cell) when `caps` has [`Capabilities::SUBCELL`]
///   **and** the resolution is [`Resolution::Full`];
/// - half-blocks (`▀▄█`, 1×2 per cell) when `caps` still has
///   [`Capabilities::UNICODE`] — the "where cell caps demand" fallback, also
///   taken when the budget ladder has halved the resolution;
/// - `false` (nothing drawn) when even Unicode is absent — the caller draws
///   the static-banner fallback through the render seam.
///
/// A dot lights iff its sample's luminance exceeds that dot's ordered-dither
/// threshold, so density encodes the luminance ramp; the cell's colour is its
/// brightest lit sample (later samples win ties — the front face wins).
/// Out-of-range or non-finite points are ignored; writes never leave `area`.
pub fn resolve(
    frame: &mut Frame,
    area: Rect,
    points: &[ScenePoint],
    caps: Capabilities,
    res: Resolution,
) -> bool {
    if !caps.contains(Capabilities::UNICODE) {
        return false;
    }
    if area.w == 0 || area.h == 0 {
        return true; // resolvable, just nowhere to draw
    }
    let braille = caps.contains(Capabilities::SUBCELL) && res == Resolution::Full;
    let (sub_x, sub_y) = if braille { (2u32, 4u32) } else { (1, 2) };
    let dots_x = u32::from(area.w) * sub_x;
    let dots_y = u32::from(area.h) * sub_y;

    let mut cells: std::collections::BTreeMap<(u16, u16), CellAcc> =
        std::collections::BTreeMap::new();
    for p in points {
        if !p.x.is_finite() || !p.y.is_finite() {
            continue;
        }
        if !(0.0..=1.0).contains(&p.x) || !(0.0..=1.0).contains(&p.y) {
            continue;
        }
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        #[allow(clippy::cast_sign_loss)]
        let dx = ((p.x * dots_x as f32) as u32).min(dots_x - 1);
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        #[allow(clippy::cast_sign_loss)]
        let dy_up = ((p.y * dots_y as f32) as u32).min(dots_y - 1);
        let dy = dots_y - 1 - dy_up; // dot rows count down from the top
        let threshold = dither_threshold(braille, dx % sub_x, dy % sub_y);
        if p.lum <= threshold {
            continue;
        }
        let Ok(cell_x) = u16::try_from(dx / sub_x) else {
            continue;
        };
        let Ok(cell_y) = u16::try_from(dy / sub_y) else {
            continue;
        };
        let bit = if braille {
            braille_bit(dx % sub_x, dy % sub_y)
        } else if dy.is_multiple_of(sub_y) {
            0b01 // top half
        } else {
            0b10 // bottom half
        };
        let acc = cells.entry((cell_x, cell_y)).or_insert(CellAcc {
            mask: 0,
            lum: -1.0,
            rgb: p.rgb,
        });
        acc.mask |= bit;
        if p.lum >= acc.lum {
            acc.lum = p.lum;
            acc.rgb = p.rgb;
        }
    }

    for ((cx, cy), acc) in cells {
        let ch = if braille {
            char::from_u32(0x2800 + acc.mask).unwrap_or('⠿')
        } else {
            match acc.mask {
                0b01 => '▀',
                0b10 => '▄',
                _ => '█',
            }
        };
        let style = Style::fg(Color::Rgb(acc.rgb.0, acc.rgb.1, acc.rgb.2));
        frame.set(area.x + cx, area.y + cy, ch, style);
    }
    true
}

/// The ordered-dither threshold for a sub-cell dot position.
fn dither_threshold(braille: bool, dot_x: u32, dot_y: u32) -> f32 {
    if braille {
        let row = DITHER_BRAILLE.get(dot_y as usize).unwrap_or(&[0.5, 0.5]);
        *row.get(dot_x as usize).unwrap_or(&0.5)
    } else {
        *DITHER_HALF.get(dot_y as usize).unwrap_or(&0.5)
    }
}

/// The braille dot bit for a sub-cell coordinate (Unicode braille numbering —
/// the same mapping as the plot tier's, restated here because the splash
/// resolve carries per-sample luminance the cosmetic-only `Paint` contract
/// deliberately cannot express).
fn braille_bit(dot_x: u32, dot_y: u32) -> u32 {
    const LEFT: [u32; 4] = [0x01, 0x02, 0x04, 0x40];
    const RIGHT: [u32; 4] = [0x08, 0x10, 0x20, 0x80];
    let col = if dot_x == 0 { LEFT } else { RIGHT };
    *col.get(dot_y as usize).unwrap_or(&0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::frame::Size;

    fn full_caps() -> Capabilities {
        Capabilities::TRUECOLOR
            | Capabilities::UNICODE
            | Capabilities::SUBCELL
            | Capabilities::MESH_3D
            | Capabilities::RAMP_SHADER
    }

    fn lit(f: &Frame) -> Vec<(u16, u16, char)> {
        let mut out = Vec::new();
        for y in 0..f.height() {
            for x in 0..f.width() {
                if let Some(c) = f.get(x, y) {
                    if c.ch != ' ' {
                        out.push((x, y, c.ch));
                    }
                }
            }
        }
        out
    }

    #[test]
    fn point_set_is_deterministic_for_clock_and_seed() {
        let a = scene_points(0.5, 42, Resolution::Full, (60, 20));
        let b = scene_points(0.5, 42, Resolution::Full, (60, 20));
        assert_eq!(a, b, "same (t, seed, res, size) → identical point set");
        let c = scene_points(0.5, 43, Resolution::Full, (60, 20));
        assert_ne!(a, c, "a different seed cracks differently");
    }

    #[test]
    fn final_pose_contains_ground_and_wordmark() {
        let pts = scene_points(1.0, 7, Resolution::Full, (60, 20));
        assert!(!pts.is_empty());
        // The wordmark's front face is bright; the ground is dim — both bands
        // must be present in the final pose.
        assert!(pts.iter().any(|p| p.lum > 0.6), "bright wordmark fragments");
        assert!(
            pts.iter().any(|p| p.lum < 0.5 && p.y < 0.6),
            "dim ground fragments"
        );
    }

    #[test]
    fn wordmark_rises_with_t() {
        let low = scene_points(0.2, 7, Resolution::Full, (60, 20));
        let high = scene_points(1.0, 7, Resolution::Full, (60, 20));
        let top = |pts: &[ScenePoint]| {
            pts.iter()
                .filter(|p| p.lum > 0.6)
                .map(|p| p.y)
                .fold(0.0f32, f32::max)
        };
        assert!(
            top(&high) > top(&low),
            "the revealed wordmark climbs as t advances"
        );
    }

    #[test]
    fn braille_resolve_uses_braille_glyphs() {
        let pts = scene_points(1.0, 7, Resolution::Full, (48, 16));
        let mut f = Frame::new(Size::new(48, 16));
        let area = f.rect();
        assert!(resolve(&mut f, area, &pts, full_caps(), Resolution::Full));
        let cells = lit(&f);
        assert!(!cells.is_empty());
        assert!(
            cells
                .iter()
                .any(|&(_, _, ch)| (0x2800..=0x28FF).contains(&(ch as u32))),
            "braille glyphs present"
        );
    }

    #[test]
    fn resolve_degrades_to_half_blocks_without_subcell() {
        // Forced caps: unicode but no sub-cell fidelity → half-blocks only.
        let caps = Capabilities::TRUECOLOR
            | Capabilities::UNICODE
            | Capabilities::MESH_3D
            | Capabilities::RAMP_SHADER;
        let pts = scene_points(1.0, 7, Resolution::Full, (48, 16));
        let mut f = Frame::new(Size::new(48, 16));
        let area = f.rect();
        assert!(resolve(&mut f, area, &pts, caps, Resolution::Full));
        let cells = lit(&f);
        assert!(!cells.is_empty());
        assert!(
            cells
                .iter()
                .all(|&(_, _, ch)| matches!(ch, '▀' | '▄' | '█')),
            "half-block glyphs only where caps lack SUBCELL"
        );
    }

    #[test]
    fn half_resolution_resolves_as_half_blocks() {
        // The budget ladder's first rung coarsens the raster even where the
        // terminal could do braille.
        let pts = scene_points(1.0, 7, Resolution::Half, (48, 16));
        let mut f = Frame::new(Size::new(48, 16));
        let area = f.rect();
        assert!(resolve(&mut f, area, &pts, full_caps(), Resolution::Half));
        assert!(lit(&f)
            .iter()
            .all(|&(_, _, ch)| matches!(ch, '▀' | '▄' | '█')));
    }

    #[test]
    fn no_unicode_means_unresolvable() {
        let pts = scene_points(1.0, 7, Resolution::Full, (40, 12));
        let mut f = Frame::new(Size::new(40, 12));
        let area = f.rect();
        assert!(!resolve(
            &mut f,
            area,
            &pts,
            Capabilities::TRUECOLOR,
            Resolution::Full
        ));
        assert!(lit(&f).is_empty(), "nothing drawn — caller banners");
    }

    #[test]
    fn resolved_cells_are_truecolor() {
        let pts = scene_points(1.0, 7, Resolution::Full, (48, 16));
        let mut f = Frame::new(Size::new(48, 16));
        let area = f.rect();
        resolve(&mut f, area, &pts, full_caps(), Resolution::Full);
        for y in 0..f.height() {
            for x in 0..f.width() {
                if let Some(c) = f.get(x, y) {
                    if c.ch != ' ' {
                        assert!(
                            matches!(c.style.fg, Color::Rgb(_, _, _)),
                            "every lit splash cell carries a ramp colour"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn degenerate_sizes_never_panic() {
        for size in [(0u16, 0u16), (1, 1), (2, 1)] {
            let pts = scene_points(0.5, 3, Resolution::Half, size);
            let mut f = Frame::new(Size::new(size.0, size.1));
            let area = f.rect();
            let _ = resolve(&mut f, area, &pts, full_caps(), Resolution::Half);
        }
    }

    proptest::proptest! {
        /// Generation is total and resolve never writes outside its area, for
        /// arbitrary timelines/seeds — the scope-containment invariant.
        #[test]
        fn resolve_stays_inside_area(
            t in -0.5f32..1.5,
            seed in 0u64..64,
            ax in 0u16..6, ay in 0u16..6, w in 0u16..40, h in 0u16..16,
        ) {
            let pts = scene_points(t, seed, Resolution::Full, (w.max(1), h.max(1)));
            for p in &pts {
                proptest::prop_assert!(p.x.is_finite() && p.y.is_finite());
                proptest::prop_assert!((0.0..=1.0).contains(&p.lum));
            }
            let mut f = Frame::new(Size::new(48, 24));
            let area = Rect { x: ax, y: ay, w, h };
            resolve(&mut f, area, &pts, full_caps(), Resolution::Full);
            for y in 0..f.height() {
                for x in 0..f.width() {
                    if f.get(x, y).is_some_and(|c| c.ch != ' ') {
                        proptest::prop_assert!(x >= area.x && x < area.x + area.w);
                        proptest::prop_assert!(y >= area.y && y < area.y + area.h);
                    }
                }
            }
        }
    }
}
