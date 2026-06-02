//! Locate the `entire` binary.
//!
//! Precedence (first match wins):
//! 1. an explicit override (the `--entire-bin` flag / `RIFT_ENTIRE_BIN`),
//! 2. the `entire.bin` key in Rift config,
//! 3. `entire` discovered on `PATH`.

use std::path::PathBuf;

use crate::error::EntireError;

/// Resolve the path to the `entire` binary from the given overrides.
///
/// `explicit` is the highest-priority source (CLI flag or env), `config_bin`
/// is the value loaded from `rift.toml`, and the final fallback is `PATH`.
///
/// # Errors
/// Returns [`EntireError::NotFound`] if no candidate resolves.
pub fn locate(
    explicit: Option<PathBuf>,
    config_bin: Option<PathBuf>,
) -> Result<PathBuf, EntireError> {
    if let Some(path) = explicit.or(config_bin) {
        return Ok(path);
    }
    which::which("entire").map_err(|_| EntireError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_wins_over_config() {
        let got = locate(
            Some(PathBuf::from("/a/entire")),
            Some(PathBuf::from("/b/entire")),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/a/entire"));
    }

    #[test]
    fn falls_back_to_config() {
        let got = locate(None, Some(PathBuf::from("/b/entire"))).unwrap();
        assert_eq!(got, PathBuf::from("/b/entire"));
    }
}
