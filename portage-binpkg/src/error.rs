//! Errors for GPKG read/write operations.

use std::path::PathBuf;

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced while building or reading a binary package.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// I/O error while reading or writing GPKG files.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// An external tool (`tar`/`zstd`) exited non-zero.
    #[error("{tool} failed with exit code {code}")]
    Tool {
        /// Name of the external tool.
        tool: &'static str,
        /// Non-zero exit code returned by the tool.
        code: i32,
    },

    /// A path lacked an expected component (parent / file name).
    #[error("invalid path: {0}")]
    BadPath(PathBuf),

    /// The container is missing a required member or is otherwise malformed.
    #[error("corrupt or incomplete GPKG: {0}")]
    Corrupt(String),
}
