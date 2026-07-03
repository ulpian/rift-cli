//! Terminal + build capability detection → [`Capabilities`].
//!
//! `detect` is the thin, env/probe-reading wrapper; the mapping logic lives in
//! the pure [`derive_caps`] so it is deterministic and unit-testable in both the
//! `xre` and ASCII-only builds (no test reads the real terminal). The compiled-in
//! 3D / parallel features are folded in via `cfg!`, so a build without them can
//! never claim those capabilities.

use super::Capabilities;

/// Build the capability set from already-probed terminal facts.
///
/// `force_ascii` (`--ascii` / `NO_COLOR` / `RIFT_CANOPY_ASCII`) yields the empty
/// set so [`super::select_backend`] picks the ASCII floor.
#[must_use]
pub fn derive_caps(
    truecolor: bool,
    unicode_full: bool,
    legacy_win: bool,
    force_ascii: bool,
) -> Capabilities {
    if force_ascii {
        return Capabilities::empty();
    }
    let mut c = Capabilities::empty();
    if truecolor {
        c = c.with(Capabilities::TRUECOLOR);
    }
    if unicode_full {
        c = c.with(Capabilities::UNICODE).with(Capabilities::SUBCELL);
    }
    if cfg!(feature = "xre-3d") {
        c = c
            .with(Capabilities::MESH_3D)
            .with(Capabilities::RAMP_SHADER);
    }
    if cfg!(feature = "parallel") {
        c = c.with(Capabilities::PARALLEL);
    }
    if legacy_win {
        c = c.with(Capabilities::LEGACY_WIN);
    }
    c
}

/// Detect capabilities for the current process.
///
/// Under the `xre` feature this probes the terminal via `xre_term`; otherwise it
/// falls back to a minimal env-only probe. Honors `NO_COLOR` / `RIFT_CANOPY_ASCII`
/// as a forced-ASCII signal in addition to the caller's `force_ascii`.
#[must_use]
pub fn detect(force_ascii: bool) -> Capabilities {
    let forced = force_ascii
        || std::env::var_os("NO_COLOR").is_some()
        || std::env::var_os("RIFT_CANOPY_ASCII").is_some();

    #[cfg(feature = "xre")]
    let (truecolor, unicode_full) = {
        let probe = xre_term::Capabilities::probe();
        (
            probe.color == xre_term::ColorDepth::TrueColor,
            probe.unicode == xre_term::UnicodeLevel::Full,
        )
    };

    // Without the engine we can't probe unicode; assume the safe ASCII floor.
    #[cfg(not(feature = "xre"))]
    let (truecolor, unicode_full) = (
        std::env::var("COLORTERM").is_ok_and(|v| v.contains("truecolor") || v.contains("24bit")),
        false,
    );

    derive_caps(truecolor, unicode_full, is_legacy_windows(), forced)
}

/// Whether we are on a legacy Windows console (no VT processing). Always `false`
/// off Windows; a conservative probe on Windows.
#[must_use]
fn is_legacy_windows() -> bool {
    cfg!(windows) && std::env::var_os("WT_SESSION").is_none() && std::env::var_os("TERM").is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn force_ascii_yields_empty() {
        assert!(derive_caps(true, true, false, true).is_empty());
    }

    #[test]
    fn truecolor_and_unicode_map_through() {
        let c = derive_caps(true, true, false, false);
        assert!(c.contains(Capabilities::TRUECOLOR));
        assert!(c.contains(Capabilities::UNICODE));
        assert!(c.contains(Capabilities::SUBCELL));
        assert!(!c.contains(Capabilities::LEGACY_WIN));
    }

    #[test]
    fn no_truecolor_means_no_truecolor_bit() {
        let c = derive_caps(false, true, false, false);
        assert!(!c.contains(Capabilities::TRUECOLOR));
        assert!(c.contains(Capabilities::UNICODE));
    }

    #[test]
    fn legacy_windows_flag_sets_bit() {
        let c = derive_caps(true, false, true, false);
        assert!(c.contains(Capabilities::LEGACY_WIN));
    }

    #[cfg(not(feature = "xre-3d"))]
    #[test]
    fn no_3d_feature_means_no_mesh_capability() {
        let c = derive_caps(true, true, false, false);
        assert!(!c.contains(Capabilities::MESH_3D));
        assert!(!c.contains(Capabilities::RAMP_SHADER));
    }
}
