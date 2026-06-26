//! Run an unprivileged build+merge under a fake root so `chown`/setuid succeed
//! and ownership is recorded, instead of swallowing the EPERM and losing it.
//!
//! v1 wraps the *whole* em run in one fake-root session (the umbrella model in
//! `todo/fakeroot-privilege-backends.md`): when an unprivileged invocation will
//! run build phases, [`maybe_supervise`] re-execs em once under the supervisor
//! and the caller exits with its status. The merge then stays in one process, so
//! the existing in-process merge gate still serialises qmerge. Per-package
//! `__worker` sessions and the sudo/fakeroot/hakoniwa backends slot in behind
//! [`Backend`] later.

use crate::cli::{Applet, Cli};

/// Marker set on the supervised re-exec so the faked child does not re-supervise.
const ACTIVE_ENV: &str = "EM_PRIVILEGE_ACTIVE";

/// The fake/real-root mechanism backing an unprivileged build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Already root, or already inside a faked session: real chowns, no wrapping.
    RealRoot,
    /// Pure-Rust ptrace+seccomp fake root (`fakeroost`) — the default unprivileged.
    Fakeroost,
}

impl Backend {
    /// Pick the backend for this process: [`RealRoot`](Self::RealRoot) when
    /// euid==0 or already inside a faked session, otherwise the best available
    /// faker.
    pub fn detect() -> Self {
        if rustix::process::geteuid().is_root() || already_active() {
            Backend::RealRoot
        } else {
            Backend::Fakeroost
        }
    }
}

fn already_active() -> bool {
    std::env::var_os(ACTIVE_ENV).is_some()
}

/// Does this invocation actually run build/merge phases? Only those need the fake
/// root — resolves, queries and `--pretend` do not. Covers every path that builds
/// and installs (the plain emerge merge, plus `ebuild`/`crossdev`/`toolchain`,
/// whose staged drivers run through the same merge code), so the unprivileged
/// chown handling is uniformly faked and never falls back to the EPERM swallow.
fn will_build(cli: &Cli) -> bool {
    if cli.pretend {
        return false;
    }
    match &cli.applet {
        None => !cli.atoms.is_empty() && !cli.search && !cli.searchdesc,
        Some(Applet::Ebuild { .. } | Applet::Crossdev(_) | Applet::Toolchain(_)) => true,
        Some(_) => false,
    }
}

/// If an unprivileged invocation will build, re-exec em once under the fake-root
/// supervisor and return its exit code (the caller must exit with it). Returns
/// `None` when no wrapping is needed (root, already faked, or a non-building
/// command), so the caller proceeds normally.
pub fn maybe_supervise(cli: &Cli) -> Option<i32> {
    if !will_build(cli) {
        return None;
    }
    match Backend::detect() {
        Backend::RealRoot => None,
        Backend::Fakeroost => Some(reexec_fakeroost()),
    }
}

fn reexec_fakeroost() -> i32 {
    use fakeroost::FakerootCommandExt;
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("em: cannot locate own binary for fakeroot: {e}");
            return 1;
        }
    };
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    eprintln!(">>> unprivileged build — running under fakeroost (fake root)");
    match std::process::Command::new(exe)
        .args(args)
        .env(ACTIVE_ENV, "fakeroost")
        .fakeroot()
        .status()
    {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("em: failed to start the fakeroost supervisor: {e}");
            1
        }
    }
}
