//! The Full3d frame-budget benchmark (ADR-0014 / M5-15).
//!
//! Documents the **≤ 8 ms/frame** threshold pinned as
//! [`rift_canopy::splash::director::FRAME_BUDGET`]: one full mesh-raster frame
//! — scene compile (crack-field ground mesh + extruded wordmark, luminance
//! shading) plus the density resolve into a 120×40 frame — must fit the
//! budget on a healthy machine. The live director probes exactly this draw
//! through its injected clock and, when a machine misses the budget, the
//! `BudgetLadder` downgrades **resolution first** (the `full` → `half` pair
//! below is that rung's cost ratio), **then drops the tier to Reduced2d**
//! (the floor benched last). Run with `cargo bench -p rift-canopy`.

use std::hint::black_box;
use std::time::Duration;

use criterion::{criterion_main, Criterion};
use rift_canopy::render::frame::{Frame, Size};
use rift_canopy::render::Capabilities;
use rift_canopy::splash::director::{Tier, FRAME_BUDGET};
use rift_canopy::splash::mesh3d::Resolution;
use rift_canopy::splash::render2d::draw_tier;
use rift_canopy::AsciiBackend;

/// A typical live terminal size for the budget measurement.
const BENCH_SIZE: Size = Size::new(120, 40);

/// The director's default splash seed (the goldens pin the same one).
const SPLASH_SEED: u64 = 0x5249_4654_5249_4654;

/// The forced Full3d capability set (benches never probe a real terminal).
fn full3d_caps() -> Capabilities {
    Capabilities::TRUECOLOR
        | Capabilities::UNICODE
        | Capabilities::SUBCELL
        | Capabilities::MESH_3D
        | Capabilities::RAMP_SHADER
}

/// Draw one splash frame exactly the way the director's probe measures it.
fn one_frame(tier: Tier, t: f32, res: Resolution) -> Frame {
    let mut backend = AsciiBackend;
    let mut frame = Frame::new(BENCH_SIZE);
    draw_tier(
        tier,
        black_box(t),
        SPLASH_SEED,
        &mut backend,
        &mut frame,
        full3d_caps(),
        res,
    );
    frame
}

/// The budget group: Full3d at both raster resolutions and the Reduced2d
/// floor the ladder drops to. Criterion's mean for `full3d_full_120x40` is
/// the number to hold against [`FRAME_BUDGET`].
fn splash_frame_budget(c: &mut Criterion) {
    // Surface the documented threshold in the bench output.
    assert_eq!(FRAME_BUDGET, Duration::from_millis(8));

    let mut group = c.benchmark_group("splash_frame");
    group.bench_function("full3d_full_120x40", |b| {
        b.iter(|| one_frame(Tier::Full3d, 0.5, Resolution::Full));
    });
    group.bench_function("full3d_half_120x40", |b| {
        b.iter(|| one_frame(Tier::Full3d, 0.5, Resolution::Half));
    });
    group.bench_function("reduced2d_120x40", |b| {
        b.iter(|| one_frame(Tier::Reduced2d, 0.5, Resolution::Full));
    });
    group.finish();
}

// The lint allowance covers the macro-generated harness items only.
#[allow(missing_docs)]
mod harness {
    use super::splash_frame_budget;
    use criterion::criterion_group;

    criterion_group!(benches, splash_frame_budget);
}

criterion_main!(harness::benches);
