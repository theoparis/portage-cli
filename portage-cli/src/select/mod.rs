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
//! - [`mirrors`] — `mirrorselect` workalike for managing GENTOO_MIRRORS.

mod binutils;
mod clang;
mod compiler;
mod env_d;
mod linker;
mod mirrors;
mod profile;
mod repos;

use anyhow::Result;
use camino::Utf8PathBuf;

use crate::cli::{Cli, Roots, SelectCommand};
use crate::style::{C_HOST, C_PREFIX};

/// Activate the newest binutils profile built into `roots`' merge root for
/// `target` (the `binutils-config` half of `crossdev --setup`'s toolchain
/// activation). Takes `Roots` directly, not `&Cli` — the `cross-<CTARGET>/*`
/// toolchain always lives in the plain outer EROOT (see `crossdev/mod.rs`'s
/// module doc), so callers must pass `Cli::base_roots`, never `Cli::roots`
/// (which would substitute in a `--cross`-active sysroot instead).
pub fn activate_binutils(roots: &Roots, target: &str) -> Result<bool> {
    binutils::activate_latest(roots, target)
}

/// Activate the newest gcc profile built into `roots`' merge root for
/// `target` (the `gcc-config` half). Run after [`activate_binutils`]. See its
/// doc comment for why this takes `Roots` rather than `&Cli`.
pub fn activate_compiler(roots: &Roots, target: &str) -> Result<bool> {
    compiler::activate_latest(roots, target)
}

/// The `SLOT` `gcc-config` currently has active for `target` in `roots`, or
/// `None` if no toolchain has been activated there yet.
pub fn current_compiler_slot(roots: &Roots, target: &str) -> Option<String> {
    compiler::current_slot(roots, target)
}

/// Dispatch `em select <module> <action>`.
pub async fn run(command: &SelectCommand, globals: &Cli) -> Result<()> {
    match command {
        SelectCommand::Profile { action } => profile::run(action, globals),
        SelectCommand::Repository { action } => repos::run(action, globals),
        SelectCommand::Compiler { action } => compiler::run(action, globals),
        SelectCommand::Binutils { action } => binutils::run(action, globals),
        SelectCommand::Linker { action } => linker::run(action, globals),
        SelectCommand::Clang { action } => clang::run(action, globals),
        SelectCommand::Mirrors { action } => mirrors::run(action, globals).await,
    }
}

/// The configuration root for `etc/portage` operations: `--config-root`
/// (cross sysroot / offset) when given, else `--prefix`/`--local` overlay, else `/`.
fn config_portage_dir(globals: &Cli) -> Utf8PathBuf {
    config_portage_dir_for(&globals.roots())
}

/// [`config_portage_dir`], but from an already-computed [`Roots`] rather than
/// `&Cli` — used by [`env_d`] so its crossdev-facing entry points
/// ([`activate_binutils`]/[`activate_compiler`]) can be handed
/// `Cli::base_roots` instead of the `--cross`-substituted `Cli::roots`.
pub(super) fn config_portage_dir_for(roots: &Roots) -> Utf8PathBuf {
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

/// Check if we're in a prefix/local context (--local or --prefix without --config-root).
pub fn is_prefix_context(globals: &Cli) -> bool {
    is_prefix_context_for(&globals.roots())
}

/// [`is_prefix_context`], but from an already-computed [`Roots`] — see
/// [`config_portage_dir_for`].
pub(super) fn is_prefix_context_for(roots: &Roots) -> bool {
    roots.config().is_none() && roots.config_overlay().is_some()
}

/// Format a source label for display in prefix context.
pub fn source_label(is_host: bool) -> String {
    if is_host {
        format!("{C_HOST} (host){C_HOST:#}")
    } else {
        format!("{C_PREFIX} (prefix){C_PREFIX:#}")
    }
}

/// Get CHOST from make.conf.
pub fn get_chost(globals: &Cli) -> Result<String, anyhow::Error> {
    let make_conf_path = config_portage_dir(globals).join("make.conf");
    let system_make_conf = Utf8PathBuf::from("/etc/make.conf");

    let paths_to_check = if system_make_conf.is_file() {
        vec![system_make_conf, make_conf_path]
    } else {
        vec![make_conf_path]
    };

    for path in paths_to_check {
        if let Ok(content) = std::fs::read_to_string(&path) {
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with("CHOST=") {
                    let mut chost = line.trim_start_matches("CHOST=").trim().to_string();
                    let needs_strip = (chost.starts_with('"') && chost.ends_with('"'))
                        || (chost.starts_with("'") && chost.ends_with("'"));
                    if needs_strip {
                        chost = chost[1..chost.len() - 1].to_string();
                    }
                    return Ok(chost);
                }
            }
        }
    }
    let arch = globals.arch.as_str();
    Ok(format!("{arch}-unknown-linux-gnu"))
}
