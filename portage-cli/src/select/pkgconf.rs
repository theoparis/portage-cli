//! `em select pkgconf` — picks the `pkg-config`/`pkgconf` backend and
//! creates the `<CTARGET>-pkg-config` wrapper.
//!
//! Real crossdev ships a generic `cross-pkg-config` script, symlinked per
//! target as `<CTARGET>-pkg-config`, that derives `PKG_CONFIG_SYSROOT_DIR`/
//! `PKG_CONFIG_LIBDIR` from `$ESYSROOT`/`$SYSROOT`/`$ROOT` at invocation
//! time. `em` never builds an equivalent, and `toolchain-funcs.eclass`'s
//! `tc-getPKG_CONFIG` searches `$PATH` for exactly that name — so any cross
//! package that reaches a real `pkg-config` call has nothing to find (see
//! `portage-repo/src/build/shell.rs`'s cross-toolchain-selection block,
//! which now correctly leaves `PKG_CONFIG` unset rather than pointing at a
//! wrapper that doesn't exist).
//!
//! No versioned-profile/env.d state here, unlike `compiler`/`binutils`: a
//! plain symlink into whichever real backend (`pkgconf`/`pkg-config`) is
//! chosen already carries all the state needed, and both backends read
//! `PKG_CONFIG_SYSROOT_DIR`/`PKG_CONFIG_LIBDIR` from the environment
//! directly — `em` already exports these generically from the sysroot's own
//! `make.conf` (`export_sourced_env`), so no derivation script is needed the
//! way real crossdev's wrapper provides for use outside an em-managed phase.

use anyhow::{Context, Result};
use camino::Utf8PathBuf;

use super::env_d;
use crate::cli::{Cli, PkgconfAction};
use portage_resolve::Roots;

/// The backends this module knows how to wrap, in preference order (used by
/// [`activate_pkgconf`]'s auto-pick and `list`'s ordering).
const BACKENDS: &[&str] = &["pkgconf", "pkg-config"];

/// Find `name` on the real `$PATH`, returning its absolute path if present.
fn find_on_path(name: &str) -> Option<Utf8PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let candidate = dir.join(name);
            candidate
                .is_file()
                .then(|| Utf8PathBuf::from_path_buf(candidate).ok())
                .flatten()
        })
    })
}

/// Path to the `<target>-pkg-config` wrapper, rooted like `select`'s other
/// activation entry points (`env_d::eprefix`).
fn wrapper_path(roots: &Roots, target: &str) -> Utf8PathBuf {
    env_d::eprefix(roots)
        .join("usr/bin")
        .join(format!("{target}-pkg-config"))
}

/// What the wrapper at `wrapper_path` currently resolves to, if anything —
/// the backend name (matched against [`BACKENDS`] by filename) plus the
/// resolved absolute path.
fn current_backend(roots: &Roots, target: &str) -> Option<(String, Utf8PathBuf)> {
    let link = wrapper_path(roots, target);
    let resolved = std::fs::canonicalize(&link).ok()?;
    let resolved = Utf8PathBuf::from_path_buf(resolved).ok()?;
    let name = resolved.file_name()?.to_string();
    Some((name, resolved))
}

/// Create/update the `<target>-pkg-config` wrapper if it doesn't already
/// exist, preferring `pkgconf` over `pkg-config` (matching modern Gentoo's
/// own default). Idempotent and non-destructive: a wrapper already present
/// — whether from an earlier auto-activation or a deliberate `em select
/// pkgconf set` — is left untouched, the same `FillGapsOnly`-style
/// deference this codebase uses elsewhere for one-time bootstrap state.
/// Returns `false` if no known backend is reachable at all.
pub fn activate_pkgconf(roots: &Roots, target: &str) -> Result<bool> {
    let link = wrapper_path(roots, target);
    if std::fs::symlink_metadata(&link).is_ok() {
        return Ok(true);
    }
    let Some(backend_path) = BACKENDS.iter().find_map(|b| find_on_path(b)) else {
        return Ok(false);
    };
    env_d::symlink_force(&backend_path, &link)?;
    Ok(true)
}

pub fn run(action: &PkgconfAction, globals: &Cli) -> Result<()> {
    let roots = globals.roots();
    let target = match action {
        PkgconfAction::List { target, .. } | PkgconfAction::Show { target, .. } => target
            .clone()
            .unwrap_or_else(|| env_d::get_default_target(globals)),
        PkgconfAction::Set { target, .. } => target
            .clone()
            .unwrap_or_else(|| env_d::get_default_target(globals)),
    };

    match action {
        PkgconfAction::List { .. } => {
            let current = current_backend(&roots, &target);
            for (i, backend) in BACKENDS.iter().enumerate() {
                let reachable = find_on_path(backend);
                let is_current = current.as_ref().is_some_and(|(name, _)| name == backend);
                let mut line = format!("  [{}] {backend}", i + 1);
                if reachable.is_none() {
                    line.push_str(" (not found)");
                }
                if is_current {
                    line.push_str(" *");
                }
                println!("{line}");
            }
            Ok(())
        }
        PkgconfAction::Show { .. } => {
            match current_backend(&roots, &target) {
                Some((name, path)) => println!("{name} ({path})"),
                None => println!("(no pkg-config wrapper set for target '{target}')"),
            }
            Ok(())
        }
        PkgconfAction::Set { backend, .. } => {
            let resolved_name = if let Ok(n) = backend.parse::<usize>() {
                let idx = n.checked_sub(1).context("backend numbers start at 1")?;
                *BACKENDS.get(idx).with_context(|| {
                    format!("backend number {n} out of range (1..={})", BACKENDS.len())
                })?
            } else {
                BACKENDS
                    .iter()
                    .find(|b| **b == backend)
                    .copied()
                    .with_context(|| {
                        format!(
                            "unknown pkg-config backend '{backend}' (expected one of {BACKENDS:?})"
                        )
                    })?
            };
            let backend_path = find_on_path(resolved_name)
                .with_context(|| format!("'{resolved_name}' not found on $PATH"))?;
            let link = wrapper_path(&roots, &target);
            env_d::symlink_force(&backend_path, &link)?;
            println!(">>> {target}-pkg-config -> {backend_path}");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser;

    /// `PATH` is process-global and read by every test in this binary
    /// (unlike `HOME`, which `cli.rs`'s tests save/restore without a lock —
    /// `PATH` is used far more pervasively, so a plain save/restore raced
    /// against other `pkgconf` tests running concurrently in the same
    /// process). A process-wide mutex serializes the whole
    /// set-run-restore critical section instead.
    static PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct PathGuard {
        saved: Option<std::ffi::OsString>,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl PathGuard {
        fn set(dirs: &[&std::path::Path]) -> Self {
            let guard = PATH_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let saved = std::env::var_os("PATH");
            let joined = std::env::join_paths(dirs).unwrap();
            // SAFETY: serialized by `PATH_LOCK`, held until this guard drops.
            unsafe {
                std::env::set_var("PATH", joined);
            }
            Self {
                saved,
                _guard: guard,
            }
        }
    }

    impl Drop for PathGuard {
        fn drop(&mut self) {
            // SAFETY: see `set`.
            unsafe {
                match &self.saved {
                    Some(p) => std::env::set_var("PATH", p),
                    None => std::env::remove_var("PATH"),
                }
            }
        }
    }

    fn write_fake_backend(dir: &std::path::Path, name: &str) {
        std::fs::write(dir.join(name), "#!/bin/sh\n:\n").unwrap();
    }

    #[test]
    fn activate_pkgconf_creates_wrapper_preferring_pkgconf() {
        let dir = tempfile::tempdir().unwrap();
        let bindir = dir.path().join("bin");
        std::fs::create_dir_all(&bindir).unwrap();
        write_fake_backend(&bindir, "pkgconf");
        write_fake_backend(&bindir, "pkg-config");
        let _path = PathGuard::set(&[&bindir]);

        let root = dir.path().join("root");
        let cli = Cli::parse_from(["em", "--root", root.to_str().unwrap()]);
        let roots = cli.outer_roots().with_own_config_root_if_self_contained();

        let created = activate_pkgconf(&roots, "riscv64-unknown-linux-gnu").unwrap();
        assert!(created);

        let (name, _) = current_backend(&roots, "riscv64-unknown-linux-gnu").unwrap();
        assert_eq!(name, "pkgconf", "must prefer pkgconf over pkg-config");
    }

    #[test]
    fn activate_pkgconf_falls_back_to_pkg_config_when_pkgconf_missing() {
        let dir = tempfile::tempdir().unwrap();
        let bindir = dir.path().join("bin");
        std::fs::create_dir_all(&bindir).unwrap();
        write_fake_backend(&bindir, "pkg-config");
        let _path = PathGuard::set(&[&bindir]);

        let root = dir.path().join("root");
        let cli = Cli::parse_from(["em", "--root", root.to_str().unwrap()]);
        let roots = cli.outer_roots().with_own_config_root_if_self_contained();

        assert!(activate_pkgconf(&roots, "riscv64-unknown-linux-gnu").unwrap());
        let (name, _) = current_backend(&roots, "riscv64-unknown-linux-gnu").unwrap();
        assert_eq!(name, "pkg-config");
    }

    /// Regression test for the bug this module exists to fix: without any
    /// reachable backend, `activate_pkgconf` must leave no wrapper behind
    /// (not a dangling symlink to a name that doesn't exist).
    #[test]
    fn activate_pkgconf_returns_false_when_no_backend_reachable() {
        let dir = tempfile::tempdir().unwrap();
        let empty_bindir = dir.path().join("empty-bin");
        std::fs::create_dir_all(&empty_bindir).unwrap();
        let _path = PathGuard::set(&[&empty_bindir]);

        let root = dir.path().join("root");
        let cli = Cli::parse_from(["em", "--root", root.to_str().unwrap()]);
        let roots = cli.outer_roots().with_own_config_root_if_self_contained();

        assert!(!activate_pkgconf(&roots, "riscv64-unknown-linux-gnu").unwrap());
        assert!(current_backend(&roots, "riscv64-unknown-linux-gnu").is_none());
    }

    /// A deliberate `em select pkgconf set` choice (or an earlier
    /// auto-activation) must survive a later `activate_pkgconf` call rather
    /// than being silently re-picked.
    #[test]
    fn activate_pkgconf_does_not_clobber_an_existing_wrapper() {
        let dir = tempfile::tempdir().unwrap();
        let bindir = dir.path().join("bin");
        std::fs::create_dir_all(&bindir).unwrap();
        write_fake_backend(&bindir, "pkgconf");
        write_fake_backend(&bindir, "pkg-config");
        let _path = PathGuard::set(&[&bindir]);

        let root = dir.path().join("root");
        let cli = Cli::parse_from(["em", "--root", root.to_str().unwrap()]);
        let roots = cli.outer_roots().with_own_config_root_if_self_contained();

        // Deliberately pick pkg-config first.
        env_d::symlink_force(
            &Utf8PathBuf::from_path_buf(bindir.join("pkg-config")).unwrap(),
            &wrapper_path(&roots, "riscv64-unknown-linux-gnu"),
        )
        .unwrap();

        assert!(activate_pkgconf(&roots, "riscv64-unknown-linux-gnu").unwrap());
        let (name, _) = current_backend(&roots, "riscv64-unknown-linux-gnu").unwrap();
        assert_eq!(
            name, "pkg-config",
            "existing choice must not be overwritten"
        );
    }
}
