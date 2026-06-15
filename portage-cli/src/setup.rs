//! `--setup`: bootstrap an unprivileged prefix layout so a subsequent build
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

/// The `bashrc` recipe for an in-place (`--local`) prefix: paths are already
/// correct in the installed `.pc`, so only the search path is added.
const BASHRC_LOCAL: &str = r#"# Overlay search paths for `em --local` (created by `em --setup`).
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
const BASHRC_PREFIX: &str = r#"# Overlay search paths for `em --prefix DIR` (created by `em --setup`).
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

/// Bootstrap the prefix described by `roots`. Requires `--local` or `--prefix`
/// (a target other than the host `/`).
pub fn bootstrap(roots: &Roots) -> Result<()> {
    let eroot = roots.merge_root();
    if eroot.as_str() == "/" {
        anyhow::bail!("--setup needs a prefix: use it with --local or --prefix DIR");
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

    let bashrc_body = if is_local {
        BASHRC_LOCAL
    } else {
        BASHRC_PREFIX
    };
    write_if_absent(&portage.join("bashrc"), bashrc_body)?;
    write_if_absent(
        &portage.join("make.conf"),
        &make_conf_template(is_local, eroot),
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
fn make_conf_template(is_local: bool, eroot: &Utf8Path) -> String {
    let how = if is_local {
        format!(
            "#   em --local <pkg>        # builds in place into {eroot}\n\
             #   (add {eroot}/usr/bin to PATH to run what you install)\n"
        )
    } else {
        format!("#   em --prefix {eroot} <pkg>   # builds a ROOT-offset tree here\n")
    };
    format!(
        "# Portage config overlay for this em prefix (created by `em --setup`).\n\
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

fn write_if_absent(path: &Utf8Path, contents: &str) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    std::fs::write(path.as_std_path(), contents).with_context(|| format!("writing {path}"))?;
    Ok(())
}
