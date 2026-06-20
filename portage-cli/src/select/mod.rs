//! `em select` — native workalikes of `eselect` modules.
//!
//! Currently implemented:
//! - [`profile`] — a cross-aware `eselect profile` (can set a foreign-arch
//!   profile, which `eselect profile` refuses).
//! - [`repos`] — `eselect repository` limited to **local** repositories
//!   (creating/adding/removing overlays on disk; remote syncing is a TODO).

mod profile;
mod repos;

use anyhow::{Result, bail};
use camino::{Utf8Path, Utf8PathBuf};

use crate::cli::Cli;

/// Dispatch `em select <module> [args...]`.
pub fn run(module: &str, args: &[String], globals: &Cli) -> Result<()> {
    match module {
        "profile" => profile::run(args, globals),
        "repos" | "repository" => repos::run(args, globals),
        other => bail!(
            "em select: module '{other}' is not implemented (have: profile, repos). \
             Use the system `eselect {other}` for now."
        ),
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
