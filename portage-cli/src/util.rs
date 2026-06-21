//! Small filesystem helpers shared across applets.

use anyhow::{Context, Result};
use camino::Utf8Path;

/// Write `contents` to `path` only if it does not already exist (idempotent
/// scaffolding for `em setup` / `em crossdev`, which must not clobber a file the
/// user or a previous run wrote).
pub(crate) fn write_if_absent(path: &Utf8Path, contents: &str) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    std::fs::write(path, contents).with_context(|| format!("writing {path}"))
}
