//! `em select` — native workalikes of `eselect` modules.
//!
//! Currently implemented:
//! - [`profile`] — a cross-aware `eselect profile` (can set a foreign-arch
//!   profile, which `eselect profile` refuses).
//! - [`repos`] — `eselect repository` limited to **local** repositories
//!   (creating/adding/removing overlays on disk; remote syncing is a TODO).
//! - [`compiler`] — `gcc-config`/`eselect gcc` workalike for compiler profile selection.
//! - [`binutils`] — `binutils-config`/`eselect binutils` workalike for binutils profile selection.
//! - [`linker`] — linker profile selection for ld, lld, mold, etc.
//! - [`clang`] — LLVM/clang slot selection.

mod binutils;
mod clang;
mod compiler;
mod linker;
mod profile;
mod repos;

use anyhow::Result;
use camino::Utf8PathBuf;

use crate::cli::{Cli, SelectCommand};

/// Dispatch `em select <module> <action>`.
pub fn run(command: &SelectCommand, globals: &Cli) -> Result<()> {
    match command {
        SelectCommand::Profile { action } => profile::run(action, globals),
        SelectCommand::Repository { action } => repos::run(action, globals),
        SelectCommand::Compiler { action } => compiler::run(action, globals),
        SelectCommand::Binutils { action } => binutils::run(action, globals),
        SelectCommand::Linker { action } => linker::run(action, globals),
        SelectCommand::Clang { action } => clang::run(action, globals),
    }
}

/// The configuration root for `etc/portage` operations: `--config-root`
/// (cross sysroot / offset) when given, else `--prefix`/`--local` overlay, else `/`.
fn config_portage_dir(globals: &Cli) -> Utf8PathBuf {
    let roots = globals.roots();
    // If config root is explicitly set (--config-root), use it
    if let Some(config) = roots.config() {
        return config.join("etc/portage");
    }
    // If using --local or --prefix, use the overlay directory (already points to etc/portage)
    if let Some(overlay) = roots.config_overlay() {
        return overlay.to_path_buf();
    }
    // Fall back to system root
    Utf8PathBuf::from("/etc/portage")
}
