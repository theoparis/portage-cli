//! `em setup`: bootstrap an unprivileged prefix layout so a subsequent build
//! (or the next `em --local` / `em --prefix DIR` run) has the directories,
//! the overlay search-path `bashrc`, and a `make.conf` placeholder it needs.
//!
//! Two modes, distinguished by [`Roots::eprefix`]:
//! - `--local` (`EPREFIX` set): in-place Gentoo-Prefix. The `.pc` files record
//!   correct `${EPREFIX}/usr` paths, so the recipe is just an additive
//!   `PKG_CONFIG_PATH` (+ `CMAKE_PREFIX_PATH`).
//! - `--prefix DIR` (ROOT-offset): staged tree whose `.pc` record `/usr`, so the
//!   recipe also exports `CPPFLAGS`/`LDFLAGS` pointing into the prefix.
//!
//! Idempotent: directories are created if missing; files are written only when
//! absent, so re-running never clobbers a user's edits.

use anyhow::{Context, Result};
use camino::Utf8Path;

use crate::cli::Roots;
use crate::util::write_if_absent;

/// The `bashrc` recipe for an in-place (`--local`) prefix: paths are already
/// correct in the installed `.pc`, so only the search path is added.
const BASHRC_LOCAL: &str = r#"# Overlay search paths for `em --local` (created by `em setup`).
# EPREFIX makes the installed .pc record correct ${EPREFIX}/usr paths, so the
# build only needs them on the search path — no sysroot/CPPFLAGS rewriting.
if [[ -n ${EPREFIX} ]]; then
	_ov="${EPREFIX%/}"
	_libdir="$(get_libdir 2>/dev/null || echo lib)"
	export PKG_CONFIG_PATH="${_ov}/usr/${_libdir}/pkgconfig:${_ov}/usr/share/pkgconfig${PKG_CONFIG_PATH:+:${PKG_CONFIG_PATH}}"
	# meson.eclass pins PKG_CONFIG_LIBDIR to the prefix, which *replaces*
	# pkg-config's built-in default — so host base packages (zlib, …) become
	# invisible and prefix .pc with `Requires: zlib` fail to resolve. In an
	# in-place prefix the host (/) is the base system, so search the prefix
	# first, then the host. Without this, the meson font/cairo/harfbuzz chain
	# can't find host deps.
	export PKG_CONFIG_LIBDIR="${_ov}/usr/${_libdir}/pkgconfig:${_ov}/usr/share/pkgconfig:/usr/${_libdir}/pkgconfig:/usr/share/pkgconfig${PKG_CONFIG_LIBDIR:+:${PKG_CONFIG_LIBDIR}}"
	# The prefix .pc record correct -L${EPREFIX}/usr/lib for *direct* deps, but
	# the host toolchain's default link search does not include the prefix, so a
	# lib's transitive NEEDED (e.g. libxcb → libXau/libXdmcp) can't be resolved
	# at link time — every meson link probe then fails and configure misdetects
	# functions (cairo's xrender gradient fallback clashes with the new header).
	# -rpath (not just -rpath-link) so in-place prefix binaries also resolve
	# their prefix deps at runtime.
	# Most prefix headers are found via pkg-config -I, but some sources include
	# a prefix-only header transitively without their target declaring the dep
	# (e.g. mesa's gbm-dri backend pulls <xcb/xcb.h>). On the host that header
	# lives in the default search path; in the prefix it does not, so put the
	# prefix include dir on the global search path — the -I counterpart of the
	# LDFLAGS -L below, matching what --prefix mode already does.
	export CPPFLAGS="-I${_ov}/usr/include${CPPFLAGS:+ ${CPPFLAGS}}"
	export LDFLAGS="-L${_ov}/usr/${_libdir} -Wl,-rpath,${_ov}/usr/${_libdir}${LDFLAGS:+ ${LDFLAGS}}"
	# Prefix tools invoked *during* a build (g-ir-compiler, g-ir-scanner, vala,
	# …) are dynamically linked against prefix libs. The -rpath above covers
	# tools built after it landed, but anything installed earlier — and tools
	# whose rpath the host loader still doesn't search — needs the prefix libdir
	# on the runtime search path. This is build-time only (portage bashrc), so it
	# does not leak into the installed packages' runtime.
	export LD_LIBRARY_PATH="${_ov}/usr/${_libdir}${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"
	export CMAKE_PREFIX_PATH="${_ov}/usr${CMAKE_PREFIX_PATH:+:${CMAKE_PREFIX_PATH}}"
	# Build tools merged into the prefix (vala, cbindgen, …) must be on PATH,
	# and their python modules (xcb-proto's xcbgen, gobject-introspection, …)
	# on PYTHONPATH, so dependent builds find them.
	export PATH="${_ov}/usr/bin${PATH:+:${PATH}}"
	for _pd in "${_ov}"/usr/lib*/python*/site-packages; do
		[[ -d ${_pd} ]] && export PYTHONPATH="${_pd}${PYTHONPATH:+:${PYTHONPATH}}"
	done
	unset _ov _libdir _pd
fi
"#;

/// The `bashrc` recipe for a ROOT-offset (`--prefix DIR`) prefix: the staged
/// `.pc` record host-absolute `/usr` paths, so the real headers/libs are found
/// via the compiler/linker search while pkg-config just confirms presence.
const BASHRC_PREFIX: &str = r#"# Overlay search paths for `em --prefix DIR` (created by `em setup`).
# Host (/) is the build sysroot; the prefix is layered on top. Do NOT set
# PKG_CONFIG_SYSROOT_DIR (host .pc must keep their real paths); the prefix .pc
# emit harmless host-absolute -I/-L while the real files are found via the flags.
if [[ -n ${ROOT} && ${ROOT%/} != "" && ${ROOT%/} != "/" ]]; then
	_ov="${ROOT%/}"
	_libdir="$(get_libdir 2>/dev/null || echo lib)"
	export PKG_CONFIG_PATH="${_ov}/usr/${_libdir}/pkgconfig:${_ov}/usr/share/pkgconfig${PKG_CONFIG_PATH:+:${PKG_CONFIG_PATH}}"
	export CPPFLAGS="-I${_ov}/usr/include${CPPFLAGS:+ ${CPPFLAGS}}"
	export LDFLAGS="-L${_ov}/usr/${_libdir} -Wl,-rpath-link,${_ov}/usr/${_libdir}${LDFLAGS:+ ${LDFLAGS}}"
	export CMAKE_PREFIX_PATH="${_ov}/usr${CMAKE_PREFIX_PATH:+:${CMAKE_PREFIX_PATH}}"
	unset _ov _libdir
fi
"#;

/// Directories laid out under the prefix's install root (`EROOT`).
const SKELETON: &[&str] = &[
    "etc/portage",
    "var/db/pkg",
    "var/cache/distfiles",
    "var/tmp/portage",
    "var/lib",
    "usr/bin",
    "usr/include",
    "usr/share",
];

/// Bootstrap the layout described by `roots`. Needs a target other than the host
/// `/` — i.e. `--local`, `--prefix DIR`, or `--root DIR` (the cross-sysroot
/// confdir case; pair with `em select profile` to set its profile).
pub fn bootstrap(roots: &Roots) -> Result<()> {
    let eroot = roots.merge_root();
    if eroot.as_str() == "/" {
        anyhow::bail!(
            "em setup needs a target: use --local, --prefix DIR, or --root DIR \
             (the host / is never bootstrapped)"
        );
    }
    let is_local = roots.eprefix().is_some();

    for dir in SKELETON {
        let p = eroot.join(dir);
        std::fs::create_dir_all(p.as_std_path()).with_context(|| format!("creating {p}"))?;
    }
    // The libdir name is host-dependent; create both common ones so installs
    // into either land in an existing tree.
    for libdir in ["usr/lib", "usr/lib64"] {
        let _ = std::fs::create_dir_all(eroot.join(libdir).as_std_path());
    }

    let portage = roots
        .config_overlay()
        .map(Utf8Path::to_path_buf)
        .unwrap_or_else(|| eroot.join("etc/portage"));
    std::fs::create_dir_all(portage.as_std_path())
        .with_context(|| format!("creating {portage}"))?;

    // `BASHRC_PREFIX`'s CPPFLAGS/LDFLAGS injection (`-I<ROOT>/usr/include`, an
    // extra high-priority search path) is for a genuine `--prefix DIR`
    // layered *on top of* a shared host base — the host's own real headers
    // are already found by the compiler's normal default search, so the
    // prefix needs an explicit assist to also see its own. A self-contained
    // `--root DIR` (`roots.build_sysroot()` is `None`: base == target, no
    // separate host base to layer over) has no such gap — its SYSROOT/CHOST
    // toolchain wiring already resolves the whole root's own `/usr/include`
    // through the compiler's normal (or cross) search order. Injecting the
    // same CPPFLAGS there doesn't just do nothing: it *actively breaks*
    // builds — it lands ahead of a package's own project-local `-I` flags
    // (e.g. gcc's `libiberty/../include`) and can shadow a version-matched
    // local header with an incompatible one from the ROOT's own libc (found
    // 2026-07-03 doing a from-scratch native+cross toolchain bootstrap: gcc's
    // `libiberty/obstack.c` failed to compile against the ROOT's own,
    // ABI-mismatched `obstack.h`). See [[stage-build-shakeout]].
    let self_contained = !is_local && roots.build_sysroot().is_none();
    if self_contained {
        write_if_absent(&portage.join("bashrc"), "")?;
    } else {
        let bashrc_body = if is_local {
            BASHRC_LOCAL
        } else {
            BASHRC_PREFIX
        };
        write_if_absent(&portage.join("bashrc"), bashrc_body)?;
    }
    write_if_absent(
        &portage.join("make.conf"),
        &make_conf_template(is_local, self_contained, eroot),
    )?;

    if is_local {
        link_host_pythons(eroot)?;
        link_host_base_tools(eroot)?;
    }

    let mode = if is_local {
        format!("em --local            (in-place Gentoo-Prefix at {eroot})")
    } else {
        format!("em --prefix {eroot}   (ROOT-offset staging)")
    };
    println!(">>> Prefix ready at {eroot}");
    println!("    config overlay: {portage}");
    println!("    use it with:    {mode}");
    if is_local {
        println!("    add to PATH:    {eroot}/usr/bin");
    }
    Ok(())
}

/// A commented `make.conf` placeholder documenting how the prefix is used.
///
/// For `--local`/`--prefix`, profile and base make.conf (including `MAKEOPTS`)
/// come from the host, so this file is purely commentary. A self-contained
/// `--root DIR` shares none of that — this is the *only* make.conf ever read
/// — so it needs a real `MAKEOPTS`, not just a placeholder: without one, every
/// build in the root defaults to serial (`-j1`), regardless of how many cores
/// the host has. Found 2026-07-03 doing a from-scratch toolchain bootstrap: a
/// full gcc bootstrap ran over an hour single-threaded on a 128-core box
/// because `MAKEOPTS` was silently unset. See [[stage-build-shakeout]].
fn make_conf_template(is_local: bool, self_contained: bool, eroot: &Utf8Path) -> String {
    let how = if is_local {
        format!(
            "#   em --local <pkg>        # builds in place into {eroot}\n\
             #   (add {eroot}/usr/bin to PATH to run what you install)\n"
        )
    } else {
        format!("#   em --prefix {eroot} <pkg>   # builds a ROOT-offset tree here\n")
    };
    if self_contained {
        return format!(
            "# Portage config for this self-contained em --root (created by `em setup`).\n\
             #\n\
             # Use this root with:\n\
             #   em --root {eroot} <pkg>\n\
             #\n\
             # Unlike --local/--prefix, this root shares NO config with the host — this\n\
             # is the only make.conf it ever reads. MAKEOPTS mirrors the host's build\n\
             # parallelism (or falls back to nproc) since nothing else would set it.\n\
             MAKEOPTS=\"{}\"\n",
            host_makeopts()
        );
    }
    format!(
        "# Portage config overlay for this em prefix (created by `em setup`).\n\
         #\n\
         # Use this prefix with:\n\
         {how}\
         #\n\
         # Profile and base make.conf come from the host (/etc/portage). The\n\
         # `package.use` and `bashrc` files in this directory overlay the host\n\
         # config so you can tune the prefix without root. Put per-package USE\n\
         # in `package.use`, e.g.:\n\
         #   media-libs/freetype harfbuzz\n"
    )
}

/// The host's own `MAKEOPTS` (real build parallelism the user already tuned),
/// falling back to `-j<nproc>` when the host has none set or is unreadable.
fn host_makeopts() -> String {
    portage_repo::MakeConf::load_default()
        .ok()
        .and_then(|m| m.get("MAKEOPTS").map(str::to_owned))
        .unwrap_or_else(|| {
            let n = std::thread::available_parallelism().map_or(1, |n| n.get());
            format!("-j{n}")
        })
}

/// Expose the host's Python at the prefix paths the eclasses expect.
///
/// In `--local` mode the host (`/`) is the base system and provides Python, but
/// the python eclasses derive prefix-absolute paths from `EPREFIX`/`ESYSROOT`:
///
/// - `${EPREFIX}/usr/bin/pythonX.Y` is baked into installed scripts' shebangs
///   (e.g. g-ir-scanner). With no interpreter there, every such script dies with
///   `bad interpreter: No such file or directory` — surfacing as meson's opaque
///   "Unhandled python OSError" and breaking the whole gobject-introspection
///   chain (harfbuzz, pango, gdk-pixbuf, gtk+, …).
/// - `PYTHON_INCLUDEDIR=${ESYSROOT}/usr/include/pythonX.Y` is checked for
///   existence by python-utils-r1 (`does not install any header files!`),
///   breaking C-extension packages like dev-python/pillow.
///
/// Symlink the host `/usr/bin/python*` entries and `/usr/include/python*` dirs
/// into the prefix so those paths resolve. Idempotent and best-effort.
fn link_host_pythons(eroot: &Utf8Path) -> Result<()> {
    link_host_entries(&eroot.join("usr/bin"), "/usr/bin", "python")?;
    link_host_entries(&eroot.join("usr/include"), "/usr/include", "python")?;
    Ok(())
}

/// Host base-system tools that ebuilds reference by their prefix-absolute path
/// (`${EPREFIX}/usr/bin/<tool>`) rather than via `PATH`. In a real Gentoo Prefix
/// the whole userland lives under `${EPREFIX}`; in `--local` only built packages
/// do, so these must be exposed from the host. Example: the firefox ebuild sets
/// `XARGS=${EPREFIX}/usr/bin/xargs` in its mozconfig, and the build greps trees
/// with `find`. Extend as more such hard-coded references surface.
const HOST_BASE_TOOLS: &[&str] = &["xargs", "find"];

/// Symlink the host base tools in [`HOST_BASE_TOOLS`] into `${EPREFIX}/usr/bin`
/// when they are not already provided by the prefix. Idempotent, best-effort.
fn link_host_base_tools(eroot: &Utf8Path) -> Result<()> {
    let bin = eroot.join("usr/bin");
    std::fs::create_dir_all(bin.as_std_path()).with_context(|| format!("creating {bin}"))?;
    for tool in HOST_BASE_TOOLS {
        let host = format!("/usr/bin/{tool}");
        let link = bin.join(tool);
        if link.as_std_path().symlink_metadata().is_ok() || !Utf8Path::new(&host).exists() {
            continue;
        }
        let _ = std::os::unix::fs::symlink(&host, link.as_std_path());
    }
    Ok(())
}

/// Symlink every entry of `host_dir` whose name starts with `prefix` into
/// `dst_dir`, pointing back at the host path. Skips entries already present.
fn link_host_entries(dst_dir: &Utf8Path, host_dir: &str, prefix: &str) -> Result<()> {
    std::fs::create_dir_all(dst_dir.as_std_path())
        .with_context(|| format!("creating {dst_dir}"))?;
    let Ok(entries) = std::fs::read_dir(host_dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(prefix) {
            continue;
        }
        let link = dst_dir.join(name);
        // Skip if anything is already there (including a broken symlink).
        if link.as_std_path().symlink_metadata().is_ok() {
            continue;
        }
        let target = format!("{host_dir}/{name}");
        let _ = std::os::unix::fs::symlink(&target, link.as_std_path());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::Cli;

    fn bashrc_body(flag: &str, dir: &str) -> String {
        let cli = Cli::parse_from(["em", flag, dir]);
        super::bootstrap(&cli.roots()).unwrap();
        std::fs::read_to_string(cli.roots().merge_root().join("etc/portage/bashrc")).unwrap()
    }

    #[test]
    fn self_contained_root_gets_no_cppflags_injection() {
        // A genuinely self-contained `--root DIR` (base == target, no host
        // base to layer over) must NOT get BASHRC_PREFIX's CPPFLAGS/LDFLAGS
        // injection — it actively breaks builds by out-ranking a package's
        // own project-local `-I` flags (found 2026-07-03, see
        // todo/stage-build-shakeout.md).
        let dir = tempfile::tempdir().unwrap();
        let body = bashrc_body("--root", dir.path().to_str().unwrap());
        assert_eq!(body, "", "self-contained --root must get an empty bashrc");
    }

    #[test]
    fn layered_prefix_keeps_cppflags_injection() {
        // A `--prefix DIR` layered on the shared host base still needs it —
        // unaffected by the self-contained fix above.
        let dir = tempfile::tempdir().unwrap();
        let body = bashrc_body("--prefix", dir.path().to_str().unwrap());
        assert!(body.contains("CPPFLAGS"));
    }

    #[test]
    fn self_contained_root_gets_real_makeopts() {
        // Without this, every build in a self-contained --root defaults to
        // serial (no host make.conf to inherit MAKEOPTS from) — found
        // 2026-07-03 when a full gcc bootstrap ran single-threaded for over
        // an hour on a 128-core box. See todo/stage-build-shakeout.md.
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["em", "--root", dir.path().to_str().unwrap()]);
        super::bootstrap(&cli.roots()).unwrap();
        let make_conf =
            std::fs::read_to_string(cli.roots().merge_root().join("etc/portage/make.conf"))
                .unwrap();
        assert!(make_conf.contains("MAKEOPTS="));
        assert!(!make_conf.contains("MAKEOPTS=\"\""), "must be non-empty");
    }

    #[test]
    fn layered_prefix_make_conf_has_no_makeopts() {
        // Unaffected by the self-contained fix — --prefix already inherits
        // the host's real MAKEOPTS via config sharing.
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["em", "--prefix", dir.path().to_str().unwrap()]);
        super::bootstrap(&cli.roots()).unwrap();
        let make_conf =
            std::fs::read_to_string(cli.roots().merge_root().join("etc/portage/make.conf"))
                .unwrap();
        assert!(!make_conf.contains("MAKEOPTS="));
    }
}
