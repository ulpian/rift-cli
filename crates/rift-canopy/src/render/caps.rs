//! Terminal + build capability detection → [`Capabilities`].
//!
//! `detect` is the thin, env/probe-reading wrapper; the mapping logic lives in
//! the pure [`derive_caps`] — and, on Windows, the pure [`windows_facts`] — so
//! it is deterministic and unit-testable in both the `xre` and ASCII-only
//! builds (no test reads the real terminal). The compiled-in 3D / parallel
//! features are folded in via `cfg!`, so a build without them can never claim
//! those capabilities.
//!
//! **Why Windows has its own probe.** The engine's probe reads `COLORTERM`,
//! `TERM` and the locale variables, which Windows Terminal, conhost and VS
//! Code's terminal never set — so on Windows it reported 16 colours and ASCII,
//! and the dashboard always landed on the style-less floor. The console can
//! answer the real question directly: asked to enable virtual-terminal
//! processing, every Windows 10 (1703+) console that says yes renders 24-bit
//! colour and attributes. Which glyphs it can show is a separate fact, read off
//! the host's own markers ([`WindowsConsole`]).

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
    if cfg!(feature = "xre") && truecolor {
        c = c.with(Capabilities::RAMP_SHADER);
    }
    if cfg!(feature = "xre-3d") {
        c = c.with(Capabilities::MESH_3D);
    }
    if cfg!(feature = "parallel") {
        c = c.with(Capabilities::PARALLEL);
    }
    if legacy_win {
        c = c.with(Capabilities::LEGACY_WIN);
    }
    c
}

/// What the Windows console probe learned — plain facts, so the mapping onto
/// capabilities ([`windows_facts`]) is a pure function a test can drive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowsConsole {
    /// The console accepted `ENABLE_VIRTUAL_TERMINAL_PROCESSING` (or `TERM`
    /// names a VT-capable host of its own, the mintty case). This is the one
    /// fact that decides colour: a VT console renders 24-bit SGR.
    pub vt: bool,
    /// `WT_SESSION` is set — Windows Terminal, whose fonts carry the full glyph set.
    pub wt_session: bool,
    /// `TERM_PROGRAM=vscode` — VS Code's integrated terminal (xterm.js).
    pub vscode: bool,
    /// `TERM` is set — mintty / Git Bash / another host that speaks for itself.
    pub term: bool,
}

/// Map the Windows console facts onto [`derive_caps`]'s inputs:
/// `(truecolor, unicode_full, legacy_win)`.
///
/// Colour follows VT alone. Unicode additionally needs a host known to render
/// braille, box-drawing and the icon glyphs — Windows Terminal, VS Code, or a
/// host that sets `TERM` — because a bare conhost window with a raster or
/// Consolas font shows tofu for several of them, and a colour tier with ASCII
/// icons beats tofu. A console that refuses VT is the legacy floor: no colour,
/// no Unicode, and the splash never animates.
#[must_use]
pub const fn windows_facts(console: WindowsConsole) -> (bool, bool, bool) {
    let truecolor = console.vt;
    let unicode_full = console.vt && (console.wt_session || console.vscode || console.term);
    (truecolor, unicode_full, !console.vt)
}

/// Detect capabilities for the current process.
///
/// Under the `xre` feature this probes the terminal — the console itself on
/// Windows, `xre_term`'s environment probe elsewhere; otherwise it falls back
/// to a minimal env-only probe. Honors `NO_COLOR` / `RIFT_CANOPY_ASCII` as a
/// forced-ASCII signal in addition to the caller's `force_ascii`.
#[must_use]
pub fn detect(force_ascii: bool) -> Capabilities {
    let forced = force_ascii
        || std::env::var_os("NO_COLOR").is_some()
        || std::env::var_os("RIFT_CANOPY_ASCII").is_some();
    let (truecolor, unicode_full, legacy_win) = probe();
    let caps = derive_caps(truecolor, unicode_full, legacy_win, forced);
    #[cfg(feature = "xre")]
    if !forced && terminal_probe().color != xre_term::ColorDepth::Mono && !legacy_win {
        return caps.with(Capabilities::ANSI_COLOR);
    }
    caps
}

/// The Windows probe: the console's own answer, mapped by [`windows_facts`].
#[cfg(all(windows, feature = "xre"))]
fn probe() -> (bool, bool, bool) {
    windows_facts(probe_windows_console())
}

/// Read the four Windows console facts. The VT question is asked of the
/// console (which also switches VT processing on, the same call the engine's
/// guard makes); the host markers are plain environment reads.
#[cfg(all(windows, feature = "xre"))]
fn probe_windows_console() -> WindowsConsole {
    let set = |key: &str| std::env::var_os(key).is_some_and(|v| !v.is_empty());
    WindowsConsole {
        vt: crossterm::ansi_support::supports_ansi(),
        wt_session: set("WT_SESSION"),
        vscode: std::env::var("TERM_PROGRAM").is_ok_and(|v| v == "vscode"),
        term: set("TERM"),
    }
}

/// The engine's environment probe, which is right everywhere the environment
/// describes the terminal (every POSIX host).
#[cfg(all(not(windows), feature = "xre"))]
fn probe() -> (bool, bool, bool) {
    let probe = terminal_probe();
    (
        probe.color == xre_term::ColorDepth::TrueColor,
        probe.unicode == xre_term::UnicodeLevel::Full,
        false,
    )
}

/// Keep the host correction shared by capability detection and presentation.
#[cfg(feature = "xre")]
pub(super) fn terminal_probe() -> xre_term::Capabilities {
    let mut probe = xre_term::Capabilities::probe();
    probe.color = host_color_depth(std::env::var("TERM_PROGRAM").ok().as_deref(), probe.color);
    probe
}

#[cfg(feature = "xre")]
fn host_color_depth(host: Option<&str>, depth: xre_term::ColorDepth) -> xre_term::ColorDepth {
    // Apple Terminal interprets unsupported RGB parameters as independent SGR
    // codes, corrupting attributes and backgrounds. Ignore inherited truecolor.
    if host == Some("Apple_Terminal") && depth == xre_term::ColorDepth::TrueColor {
        xre_term::ColorDepth::Ansi256
    } else {
        depth
    }
}

/// Without the engine we can't probe unicode; assume the safe ASCII floor.
#[cfg(not(feature = "xre"))]
fn probe() -> (bool, bool, bool) {
    (
        std::env::var("COLORTERM").is_ok_and(|v| v.contains("truecolor") || v.contains("24bit")),
        false,
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "xre")]
    #[test]
    fn apple_terminal_caps_rgb_without_overriding_mono_or_other_hosts() {
        use xre_term::ColorDepth::{Ansi16, Ansi256, Mono, TrueColor};
        for depth in [Mono, Ansi16, Ansi256, TrueColor] {
            let expected = if depth == TrueColor { Ansi256 } else { depth };
            assert_eq!(host_color_depth(Some("Apple_Terminal"), depth), expected);
            assert_eq!(host_color_depth(Some("iTerm.app"), depth), depth);
            assert_eq!(host_color_depth(None, depth), depth);
        }
    }

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

    /// The two hosts a Windows developer actually runs the dashboard in land
    /// on the full engine tier — colour, Unicode glyphs, sub-cell plots — the
    /// moment the console accepts VT, without any of the POSIX environment
    /// variables the engine's own probe wants.
    #[test]
    fn windows_terminal_and_vscode_land_on_the_engine_tier() {
        let hosts = [
            WindowsConsole {
                vt: true,
                wt_session: true,
                ..WindowsConsole::default()
            },
            WindowsConsole {
                vt: true,
                vscode: true,
                ..WindowsConsole::default()
            },
            WindowsConsole {
                vt: true,
                term: true,
                ..WindowsConsole::default()
            },
        ];
        for host in hosts {
            let (truecolor, unicode, legacy) = windows_facts(host);
            let c = derive_caps(truecolor, unicode, legacy, false);
            assert!(c.contains(Capabilities::TRUECOLOR), "{host:?}");
            assert!(c.contains(Capabilities::UNICODE), "{host:?}");
            assert!(c.contains(Capabilities::SUBCELL), "{host:?}");
            assert!(!c.contains(Capabilities::LEGACY_WIN), "{host:?}");
        }
    }

    /// A bare conhost window that accepts VT gets colour but ASCII icons —
    /// its default fonts render tofu for several of the Unicode glyphs.
    #[test]
    fn bare_conhost_with_vt_gets_colour_but_ascii_glyphs() {
        let (truecolor, unicode, legacy) = windows_facts(WindowsConsole {
            vt: true,
            ..WindowsConsole::default()
        });
        let c = derive_caps(truecolor, unicode, legacy, false);
        assert!(c.contains(Capabilities::TRUECOLOR));
        assert!(!c.contains(Capabilities::UNICODE));
        assert!(!c.contains(Capabilities::SUBCELL));
        assert!(!c.contains(Capabilities::LEGACY_WIN));
    }

    /// A console that refuses VT processing is the legacy floor whatever the
    /// host markers say: no colour, no Unicode, and the ASCII backend.
    #[test]
    fn a_console_that_refuses_vt_is_the_legacy_floor() {
        let (truecolor, unicode, legacy) = windows_facts(WindowsConsole {
            vt: false,
            wt_session: true,
            vscode: true,
            term: true,
        });
        assert_eq!((truecolor, unicode, legacy), (false, false, true));
        let c = derive_caps(truecolor, unicode, legacy, false);
        assert!(c.contains(Capabilities::LEGACY_WIN));
        assert_eq!(
            super::super::select_backend(c, false).name(),
            "ascii-fallback",
            "no truecolor bit ⇒ the floor backend"
        );
    }

    #[cfg(not(feature = "xre-3d"))]
    #[test]
    fn no_3d_feature_means_no_mesh_capability() {
        let c = derive_caps(true, true, false, false);
        assert!(!c.contains(Capabilities::MESH_3D));
        assert_eq!(c.contains(Capabilities::RAMP_SHADER), cfg!(feature = "xre"));
    }

    #[cfg(feature = "xre-3d")]
    #[test]
    fn xre_3d_feature_advertises_the_raster_capability() {
        // The hi-res coloured plot path routes on MESH_3D, which the xre-3d build
        // sets so `XreBackend::plot` selects `xre_hires::draw`.
        let c = derive_caps(true, true, false, false);
        assert!(c.contains(Capabilities::MESH_3D));
        assert!(c.contains(Capabilities::RAMP_SHADER));
    }
}
