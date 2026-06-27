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
//! Backend selection (`--privilege`, or `EM_PRIVILEGE`; default `auto`):
//! - `auto`/`fakeroost` — pure-Rust ptrace+seccomp fake root (no privilege). The
//!   default: ownership is faked in-session, on-disk stays the build user.
//! - `hakoniwa` — user-namespace sandbox with build-user→0 map ("real-in-a-box"):
//!   real `chown`/`setuid` syscalls inside the box; on-disk owners are the
//!   mapped host ids (same family as `sudo`, without host root).
//! - `sudo` — re-exec under `sudo` for *real* root (real root-owned tree + real
//!   setuid). Opt-in only; never auto-selected (it escalates privilege).
//! - `none` — disable wrapping; run unprivileged and let the chown workarounds
//!   degrade gracefully.
//!
//! Already root ⇒ no wrapping (real chowns in-process). Per-package `__worker`
//! sessions and the fakeroot backend slot in behind [`Backend`] later.

use crate::cli::{Applet, Cli, Privilege};

/// Marker set on a wrapped re-exec so the inner process does not re-wrap.
const ACTIVE_ENV: &str = "EM_PRIVILEGE_ACTIVE";

/// The root mechanism backing an unprivileged build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Already root, or already inside a session: real chowns, no wrapping.
    RealRoot,
    /// Pure-Rust ptrace+seccomp fake root (`fakeroost`) — the default unprivileged.
    Fakeroost,
    /// User-namespace sandbox (`hakoniwa`) with build-user→0 map.
    Hakoniwa,
    /// Re-exec under `sudo` for real root. Opt-in via `EM_PRIVILEGE=sudo`.
    Sudo,
}

impl Backend {
    /// Pick the backend for this process: [`RealRoot`](Self::RealRoot) when
    /// euid==0 or already inside a wrapped session; otherwise map the `--privilege`
    /// request (`auto` ⇒ fakeroost).
    pub fn detect(requested: Privilege) -> Self {
        if rustix::process::geteuid().is_root() || already_active() {
            return Backend::RealRoot;
        }
        match requested {
            Privilege::Auto | Privilege::Fakeroost => Backend::Fakeroost,
            Privilege::Hakoniwa => Backend::Hakoniwa,
            Privilege::Sudo => Backend::Sudo,
            Privilege::None => Backend::RealRoot,
        }
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
    match Backend::detect(cli.privilege) {
        Backend::RealRoot => None,
        Backend::Fakeroost => Some(reexec_fakeroost()),
        Backend::Hakoniwa => Some(reexec_hakoniwa(cli)),
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

/// Whether the host can spawn an unprivileged user namespace with id maps.
///
/// Hakoniwa's parent process writes `/proc/<child>/uid_map` via `newuidmap` /
/// `newgidmap`; both the kernel knob and those helpers must be present.
pub(crate) fn userns_available() -> bool {
    if let Ok(v) = std::fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone")
        && v.trim() == "0"
    {
        return false;
    }
    ["newuidmap", "newgidmap"]
        .iter()
        .any(|name| which_in_path(name))
}

fn which_in_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

/// Bind `host` read-write at the same path inside hakoniwa's mount namespace.
fn bind_rw(container: &mut hakoniwa::Container, host: &str) {
    if std::path::Path::new(host).is_dir() {
        container.bindmount_rw(host, host);
    }
}

/// Writable trees the build touches but `rootfs("/")` leaves out (it only
/// bind-mounts the usual FHS prefixes read-only).
fn bind_build_tree(container: &mut hakoniwa::Container, cli: &Cli) {
    let roots = cli.roots();
    bind_rw(container, roots.merge_root().as_str());
    if let Some(overlay) = roots.config_overlay() {
        bind_rw(container, overlay.as_str());
    }
    if let Some(eprefix) = roots.eprefix() {
        bind_rw(container, eprefix.as_str());
    }
    bind_rw(container, "/tmp");
    bind_rw(container, "/var/tmp");
    if roots.relocate() {
        let merge = roots.merge_root();
        bind_rw(container, merge.join("var/cache/distfiles").as_ref());
        bind_rw(container, merge.join("var/tmp").as_ref());
    }
}

fn reexec_hakoniwa(cli: &Cli) -> i32 {
    if !userns_available() {
        eprintln!(
            "em: hakoniwa requires user namespaces and newuidmap/newgidmap on PATH; \
             try --privilege fakeroost or sudo"
        );
        return 1;
    }
    let Some((exe, args)) = self_invocation() else {
        return 1;
    };
    let program = match exe.to_str() {
        Some(s) => s,
        None => {
            eprintln!("em: hakoniwa cannot run a non-UTF-8 executable path");
            return 1;
        }
    };

    let mut container = hakoniwa::Container::new();
    // Replace Container::new()'s identity map with build-user→0 so chown/setuid
    // syscalls succeed inside the box (real-in-a-box, not fake accounting).
    container.uidmap(0).gidmap(0);
    if let Err(e) = container.rootfs("/") {
        eprintln!("em: hakoniwa rootfs setup failed: {e}");
        return 1;
    }
    bind_build_tree(&mut container, cli);

    let mut cmd = container.command(program);
    for arg in args {
        let Some(s) = arg.to_str() else {
            eprintln!("em: hakoniwa cannot forward a non-UTF-8 argument");
            return 1;
        };
        cmd.arg(s);
    }
    cmd.env(ACTIVE_ENV, "hakoniwa");
    for (key, val) in std::env::vars() {
        cmd.env(&key, &val);
    }

    eprintln!(">>> unprivileged build — running under hakoniwa (userns mapped root)");
    match cmd.status() {
        Ok(status) => status.code,
        Err(e) => {
            eprintln!("em: failed to start the hakoniwa container: {e}");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn userns_knob_zero_means_unavailable() {
        // Don't assert true on real hosts — only that we don't panic reading the knob.
        let _ = userns_available();
    }
}