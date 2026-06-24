//! `em select compiler` — `gcc-config`/`eselect gcc` workalike.
//!
//! Manages compiler profile selection for gcc. Reads/writes env.d files and
//! creates symlinks similar to gcc-config. Supports grouping profiles by target
//! architecture and showing which is active per architecture.

use std::collections::BTreeMap;

use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};

use super::{Cli, env_d};
use crate::cli::CompilerAction;

/// GCC-specific profile type.
pub struct GccProfileType;

impl env_d::EnvDProfile for GccProfileType {
    fn module_name() -> &'static str {
        "compiler"
    }

    fn env_d_subdir() -> &'static str {
        "gcc"
    }

    fn global_env_file() -> &'static str {
        "04gcc-{target}"
    }

    fn global_env_uses_target() -> bool {
        true
    }

    fn target_var_name() -> &'static str {
        "CTARGET="
    }

    fn install_wrappers(
        globals: &Cli,
        target: &str,
        vars: &BTreeMap<String, String>,
    ) -> Result<()> {
        let Some(gcc_path) = vars.get("GCC_PATH").filter(|p| !p.is_empty()) else {
            return Ok(());
        };
        install_gcc_wrappers(&env_d::eprefix(globals), target, gcc_path)
    }
}

/// Replicate `gcc-config`'s `usr/bin/<T>-<tool>` → `<GCC_PATH>/<T>-<tool>` symlinks
/// (the gcc-bin binaries are already `<T>-`prefixed), plus the `<T>-cc` alias.
/// `gcc_path` is the env.d `GCC_PATH` (`/usr/<CBUILD>/<T>/gcc-bin/<ver>`); it is
/// always resolved under `eprefix` so a `--local`/`--prefix` install links its own
/// binaries, not a same-pathed host copy. No-op until the compiler is merged.
fn install_gcc_wrappers(eprefix: &Utf8Path, target: &str, gcc_path: &str) -> Result<()> {
    // GCC_PATH may or may not already carry the EPREFIX; strip it then re-root so
    // the symlink content stays inside the prefix either way.
    let rel = gcc_path.strip_prefix(eprefix.as_str()).unwrap_or(gcc_path);
    let bindir = eprefix.join(rel.trim_start_matches('/'));
    if !bindir.is_dir() {
        return Ok(());
    }
    let usr_bin = eprefix.join("usr/bin");
    let mut have_gcc = false;
    for entry in std::fs::read_dir(&bindir)? {
        let Ok(path) = Utf8PathBuf::from_path_buf(entry?.path()) else {
            continue;
        };
        let name = path.file_name().unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        env_d::symlink_force(&bindir.join(name), &usr_bin.join(name))?;
        have_gcc |= name == format!("{target}-gcc");
    }
    if have_gcc {
        env_d::symlink_force(
            Utf8Path::new(&format!("{target}-gcc")),
            &usr_bin.join(format!("{target}-cc")),
        )?;
    }
    Ok(())
}

/// Activate the newest gcc profile built into this root for `target`
/// (`crossdev --setup`). EPREFIX-aware; no-op until a gcc step merges. Run after
/// [`super::binutils::activate_latest`] so the gcc wrappers can reach cross as/ld.
pub fn activate_latest(globals: &Cli, target: &str) -> Result<bool> {
    env_d::activate_latest::<GccProfileType>(globals, target)
}

pub fn run(action: &CompilerAction, globals: &Cli) -> Result<()> {
    let target = match action {
        CompilerAction::List { target, .. } | CompilerAction::Show { target, .. } => target
            .clone()
            .unwrap_or_else(|| env_d::get_default_target(globals)),
        CompilerAction::Set { target, .. } => target
            .clone()
            .unwrap_or_else(|| env_d::get_default_target(globals)),
    };

    let base_dir = env_d::env_d_dir::<GccProfileType>(globals);

    match action {
        CompilerAction::List { .. } => env_d::run_list::<GccProfileType>(globals),
        CompilerAction::Show { .. } => env_d::run_show::<GccProfileType>(globals, &target),
        CompilerAction::Set { profile, .. } => {
            env_d::run_set::<GccProfileType>(globals, &target, profile, &base_dir)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_wrappers_link_gcc_bin_directly() {
        let td = tempfile::TempDir::new().unwrap();
        let eprefix = Utf8Path::from_path(td.path()).unwrap().to_path_buf();
        let target = "riscv64-unknown-linux-gnu";
        let gcc_path = format!("/usr/aarch64-unknown-linux-gnu/{target}/gcc-bin/15");

        let bindir = eprefix.join(gcc_path.trim_start_matches('/'));
        std::fs::create_dir_all(&bindir).unwrap();
        for tool in ["gcc", "g++", "cpp"] {
            std::fs::write(bindir.join(format!("{target}-{tool}")), b"#!/bin/true\n").unwrap();
        }

        install_gcc_wrappers(&eprefix, target, &gcc_path).unwrap();

        let bin_gcc = eprefix.join("usr/bin").join(format!("{target}-gcc"));
        assert_eq!(
            std::fs::read_link(&bin_gcc).unwrap(),
            bindir.join(format!("{target}-gcc")).as_std_path()
        );
        // <T>-cc aliases <T>-gcc (relative content).
        let bin_cc = eprefix.join("usr/bin").join(format!("{target}-cc"));
        assert_eq!(
            std::fs::read_link(&bin_cc).unwrap(),
            std::path::Path::new(&format!("{target}-gcc"))
        );
    }
}
