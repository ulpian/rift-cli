//! Global, per-user splash preferences (`~/.config/rift/canopy.toml`).
//!
//! `seen_splash` is a *global* "show the open once per developer" fact — not a
//! per-repo, version-controlled value — so it lives beside the project registry
//! (same config-dir resolver as [`rift_registry`]), never in `.rift/config.toml`.
//! Reads are **typed and default-safe**: a missing, unreadable, or garbage file
//! yields [`CanopyPrefs::default`] (which plays the splash once), never a panic.
//! Writes are whole-file "latest-wins" — the superseding-write model for a
//! single-value preference. The path is env-injectable (`RIFT_CONFIG_DIR`), so
//! tests are hermetic.
//!
//! **Deliberate stay-behind (ADR-0009).** This is the one persistence path that
//! lives in `rift-canopy` rather than the `rift-canopy-data` plane: moving it
//! would need the forbidden reverse edge (`rift-canopy` → `rift-canopy-data`),
//! and the splash needs its prefs before any data plane exists. The crate's
//! small `toml`/`serde` dependency is accepted for exactly this module.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The persisted splash preferences.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CanopyPrefs {
    /// Whether the first-run splash has already been shown.
    pub seen_splash: bool,
    /// `"first-run"` (default) | `"always"` | `"never"`.
    pub splash_mode: String,
    /// Honor OS/user reduced-motion → the static tier, no animation.
    pub reduced_motion: bool,
    /// Enable mouse capture in the live dashboard (click/scroll). On by default;
    /// the `RIFT_CANOPY_NO_MOUSE` env kill-switch overrides it at launch. Capturing
    /// the mouse suppresses the terminal's native drag-to-select — most terminals
    /// still copy on Shift/Option-drag.
    pub mouse: bool,
}

impl Default for CanopyPrefs {
    fn default() -> Self {
        Self {
            seen_splash: false,
            splash_mode: "first-run".to_string(),
            reduced_motion: false,
            mouse: true,
        }
    }
}

/// The canopy-prefs path under the resolved config root (`…/rift/canopy.toml`).
#[must_use]
pub fn path_from(env: impl Fn(&str) -> Option<String>) -> PathBuf {
    // Reuse the registry's root resolution, then swap the filename.
    rift_registry::resolve_from(env).with_file_name("canopy.toml")
}

/// The default prefs path for this machine (real process environment).
#[must_use]
pub fn default_path() -> PathBuf {
    path_from(|k| std::env::var(k).ok())
}

/// Load prefs from the path resolved via `env`. Default-safe: any read/parse
/// failure (missing file, bad TOML, unknown keys) returns [`CanopyPrefs::default`].
#[must_use]
pub fn load_from(env: impl Fn(&str) -> Option<String>) -> CanopyPrefs {
    std::fs::read_to_string(path_from(env))
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist `prefs` to the path resolved via `env` (creating the dir). A
/// superseding whole-file write.
///
/// # Errors
/// [`std::io::Error`] if the directory cannot be created or the file written.
pub fn save_from(env: impl Fn(&str) -> Option<String>, prefs: &CanopyPrefs) -> std::io::Result<()> {
    let path = path_from(env);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = toml::to_string(prefs).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(path, body)
}

/// Load prefs from the real process environment.
#[must_use]
pub fn load() -> CanopyPrefs {
    load_from(|k| std::env::var(k).ok())
}

/// Save prefs to the real process environment path (best-effort at the call site).
///
/// # Errors
/// [`std::io::Error`] on a write failure.
pub fn save(prefs: &CanopyPrefs) -> std::io::Result<()> {
    save_from(|k| std::env::var(k).ok(), prefs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_for(dir: &std::path::Path) -> impl Fn(&str) -> Option<String> + '_ {
        move |k: &str| {
            if k == "RIFT_CONFIG_DIR" {
                Some(dir.to_string_lossy().into_owned())
            } else {
                None
            }
        }
    }

    #[test]
    fn missing_file_defaults_to_first_run() {
        let tmp = tempfile::tempdir().unwrap();
        let p = load_from(env_for(tmp.path()));
        assert_eq!(p, CanopyPrefs::default());
        assert!(!p.seen_splash);
        assert_eq!(p.splash_mode, "first-run");
        assert!(p.mouse, "mouse capture defaults on");
    }

    #[test]
    fn mouse_flag_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let env = env_for(tmp.path());
        let p = CanopyPrefs {
            mouse: false,
            ..CanopyPrefs::default()
        };
        save_from(&env, &p).unwrap();
        assert_eq!(load_from(&env), p);
        // A file written before the field existed still loads (serde default).
        let path = path_from(&env);
        std::fs::write(&path, "seen_splash = true\n").unwrap();
        assert!(load_from(&env).mouse, "absent mouse key defaults on");
    }

    #[test]
    fn round_trips_through_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let env = env_for(tmp.path());
        let p = CanopyPrefs {
            seen_splash: true,
            splash_mode: "always".into(),
            ..CanopyPrefs::default()
        };
        save_from(&env, &p).unwrap();
        assert_eq!(load_from(&env), p);
    }

    #[test]
    fn garbage_file_defaults_safely() {
        let tmp = tempfile::tempdir().unwrap();
        let env = env_for(tmp.path());
        let path = path_from(&env);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is not = valid toml ][").unwrap();
        // Must not panic, must default.
        assert_eq!(load_from(&env), CanopyPrefs::default());
    }

    #[test]
    fn unknown_keys_default_safely() {
        let tmp = tempfile::tempdir().unwrap();
        let env = env_for(tmp.path());
        let path = path_from(&env);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "seen_splash = true\nbogus_key = 1\n").unwrap();
        // deny_unknown_fields → parse error → default (not the partial value).
        assert_eq!(load_from(&env), CanopyPrefs::default());
    }

    #[test]
    fn path_is_canopy_toml_under_rift() {
        let p = path_from(|k| (k == "RIFT_CONFIG_DIR").then(|| "/cfg".to_string()));
        assert!(p.ends_with("rift/canopy.toml"));
    }
}
