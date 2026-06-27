use std::path::PathBuf;

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced while building or reading a binary package.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// An external tool (`tar`/`zstd`) exited non-zero.
    #[error("{tool} failed with exit code {code}")]
    Tool { tool: &'static str, code: i32 },

    /// A path lacked an expected component (parent / file name).
    #[error("invalid path: {0}")]
    BadPath(PathBuf),
}
