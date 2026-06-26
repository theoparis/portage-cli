//! Run an unprivileged build+merge with root privilege — faked or real — so
//! `chown`/setuid succeed and ownership is recorded, instead of swallowing the
//! EPERM and losing it.
//!
//! When an unprivileged invocation will run build phases, [`maybe_supervise`]
//! re-execs em once under the selected [`Backend`] and the caller exits with its
//! status. The whole run then shares one session (the umbrella model in
//! `todo/fakeroot-privilege-backends.md`), so the existing in-process merge gate
//! still serialises qmerge.
//!
//! Backend selection (`EM_PRIVILEGE`, default `auto`):
//! - `auto`/`fakeroost` — pure-Rust ptrace+seccomp fake root (no privilege). The
//!   default: ownership is faked in-session, on-disk stays the build user.
//! - `sudo` — re-exec under `sudo` for *real* root (real root-owned tree + real
//!   setuid). Opt-in only; never auto-selected (it escalates privilege).
//! - `none` — disable wrapping; run unprivileged and let the chown workarounds
//!   degrade gracefully.
//!
//! Already root ⇒ no wrapping (real chowns in-process). Per-package `__worker`
//! sessions and the fakeroot/hakoniwa backends slot in behind [`Backend`] later.

use crate::cli::{Applet, Cli};

/// Marker set on a wrapped re-exec so the inner process does not re-wrap.
const ACTIVE_ENV: &str = "EM_PRIVILEGE_ACTIVE";

/// The root mechanism backing an unprivileged build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Already root, or already inside a session: real chowns, no wrapping.
    RealRoot,
    /// Pure-Rust ptrace+seccomp fake root (`fakeroost`) — the default unprivileged.
    Fakeroost,
    /// Re-exec under `sudo` for real root. Opt-in via `EM_PRIVILEGE=sudo`.
    Sudo,
}

impl Backend {
    /// Pick the backend for this process: [`RealRoot`](Self::RealRoot) when
    /// euid==0 or already inside a wrapped session; otherwise the one requested
    /// via `EM_PRIVILEGE`, defaulting to fakeroost.
    pub fn detect() -> Self {
        if rustix::process::geteuid().is_root() || already_active() {
            return Backend::RealRoot;
        }
        requested().unwrap_or(Backend::Fakeroost)
    }
}

/// Backend explicitly requested via `EM_PRIVILEGE`. Unset/unknown ⇒ `None` (auto).
fn requested() -> Option<Backend> {
    match std::env::var("EM_PRIVILEGE")
        .ok()?
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "fakeroost" => Some(Backend::Fakeroost),
        "sudo" => Some(Backend::Sudo),
        "none" | "off" => Some(Backend::RealRoot),
        _ => None,
    }
}

fn already_active() -> bool {
    std::env::var_os(ACTIVE_ENV).is_some()
}

/// Does this invocation actually run build/merge phases? Only those need root —
/// resolves, queries and `--pretend` do not. Covers every path that builds and
/// installs (the plain emerge merge, plus `ebuild`/`crossdev`/`toolchain`, whose
/// staged drivers run through the same merge code), so the unprivileged chown
/// handling is uniformly faked and never falls back to the EPERM swallow.
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

/// If an unprivileged invocation will build, re-exec em once under the selected
/// backend and return its exit code (the caller must exit with it). Returns
/// `None` when no wrapping is needed (root, already wrapped, `EM_PRIVILEGE=none`,
/// or a non-building command), so the caller proceeds normally.
pub fn maybe_supervise(cli: &Cli) -> Option<i32> {
    if !will_build(cli) {
        return None;
    }
    match Backend::detect() {
        Backend::RealRoot => None,
        Backend::Fakeroost => Some(reexec_fakeroost()),
        Backend::Sudo => Some(reexec_sudo()),
    }
}

/// `(own binary, forwarded args)` for a self re-exec, or `None` if the binary
/// path can't be resolved (the caller treats that as a failure exit).
fn self_invocation() -> Option<(std::path::PathBuf, Vec<std::ffi::OsString>)> {
    match std::env::current_exe() {
        Ok(exe) => Some((exe, std::env::args_os().skip(1).collect())),
        Err(e) => {
            eprintln!("em: cannot locate own binary to re-exec: {e}");
            None
        }
    }
}

fn reexec_fakeroost() -> i32 {
    use fakeroost::FakerootCommandExt;
    let Some((exe, args)) = self_invocation() else {
        return 1;
    };
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

fn reexec_sudo() -> i32 {
    let Some((exe, args)) = self_invocation() else {
        return 1;
    };
    eprintln!(">>> unprivileged build — re-running under sudo (real root)");
    // `-E` preserves the environment (USE overrides, etc.); the sudoers policy may
    // still strip it, in which case the build falls back to make.conf config. The
    // root child detects euid==0 and runs in-process with real chowns.
    match std::process::Command::new("sudo")
        .arg("-E")
        .arg(exe)
        .args(args)
        .env(ACTIVE_ENV, "sudo")
        .status()
    {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("em: failed to re-exec under sudo: {e}");
            1
        }
    }
}
