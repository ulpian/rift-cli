//! Errors produced by the `entire` wrapper.

use std::path::PathBuf;

/// Something went wrong locating or running the `entire` binary.
#[derive(Debug, thiserror::Error)]
pub enum EntireError {
    #[error(
        "could not find the `entire` binary. Install it from \
         https://github.com/entireio/cli, set RIFT_ENTIRE_BIN, or pass --entire-bin <path>."
    )]
    NotFound,

    #[error("failed to spawn `entire` at {path}: {source}")]
    Spawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("`entire {args}` exited with status {code}")]
    NonZeroExit {
        args: String,
        code: i32,
        stderr: String,
    },

    #[error("failed to parse JSON from `entire {args}`: {source}")]
    JsonParse {
        args: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("unsupported `entire` version: {found} (Rift expects {expected})")]
    UnsupportedVersion { found: String, expected: String },
}
