//! Rift's own configuration (distinct from `entire`'s settings).
//!
//! Layered via figment: built-in defaults < `rift.toml` in the repo <
//! `RIFT_`-prefixed environment variables. Today this only carries the override
//! for locating the `entire` binary, but the shape is ready to grow.

use std::path::{Path, PathBuf};

use figment::providers::{Env, Format, Serialized, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};

/// Configuration for the `entire` wrapper.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EntireConfig {
    /// Explicit path to the `entire` binary. `None` falls back to `PATH`.
    #[serde(default)]
    pub bin: Option<PathBuf>,
}

/// Top-level Rift configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub entire: EntireConfig,
}

impl Config {
    /// Load configuration, layering `rift.toml` (if present in `repo`) and the
    /// `RIFT_` environment over the defaults.
    ///
    /// # Errors
    /// Returns an error if a present `rift.toml` is malformed or env values
    /// fail to deserialize. The error is boxed because `figment::Error` is large.
    pub fn load(repo: impl AsRef<Path>) -> Result<Self, Box<figment::Error>> {
        Figment::from(Serialized::defaults(Config::default()))
            .merge(Toml::file(repo.as_ref().join("rift.toml")))
            .merge(Env::prefixed("RIFT_").split("__"))
            .extract()
            .map_err(Box::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_defaults_without_a_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load(dir.path()).unwrap();
        assert!(cfg.entire.bin.is_none());
    }

    #[test]
    fn reads_entire_bin_from_rift_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("rift.toml"),
            "[entire]\nbin = \"/opt/entire\"\n",
        )
        .unwrap();
        let cfg = Config::load(dir.path()).unwrap();
        assert_eq!(cfg.entire.bin, Some(PathBuf::from("/opt/entire")));
    }
}
