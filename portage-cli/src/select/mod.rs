//! `em select` — native workalikes of `eselect` modules.
//!
//! Currently implemented:
//! - [`profile`] — a cross-aware `eselect profile` (can set a foreign-arch
//!   profile, which `eselect profile` refuses).
//! - [`repos`] — `eselect repository` limited to **local** repositories
//!   (creating/adding/removing overlays on disk; remote syncing is a TODO).

mod profile;
mod repos;

use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};

use crate::cli::{Cli, SelectCommand};

/// Dispatch `em select <module> <action>`.
pub fn run(command: &SelectCommand, globals: &Cli) -> Result<()> {
    match command {
        SelectCommand::Profile { action } => profile::run(action, globals),
        SelectCommand::Repository { action } => repos::run(action, globals),
    }
}

/// The configuration root for `etc/portage` operations: `--config-root`
/// (cross sysroot / offset) when given, else `/`.
fn config_portage_dir(globals: &Cli) -> Utf8PathBuf {
    globals
        .roots()
        .config()
        .unwrap_or_else(|| Utf8Path::new("/"))
        .join("etc/portage")
}
