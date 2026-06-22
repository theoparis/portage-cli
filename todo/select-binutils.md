# `em select binutils` — `binutils-config` workalike

STATUS: **not started.** Needed to make a freshly-merged (cross) binutils
*usable*: the `as`/`ld`/… binaries install under
`${EPREFIX}/usr/${CHOST}/${CTARGET}/binutils-bin/${VER}/` (and an `env.d`
entry), but nothing creates the `${EPREFIX}/usr/bin/${CTARGET}-{as,ld,…}`
wrappers, so `${CTARGET}-gcc -print-prog-name=as` falls back to the host `as`.
Found via the crossdev clean-slate reinstall ([[crossdev-target]] — gap #2).

## What binutils-config does (to replicate)

- Reads `${EPREFIX}/etc/env.d/binutils/${CTARGET}-${VER}` (written by the ebuild
  pkg_postinst) → the active profile per CTARGET.
- Creates `${EPREFIX}/usr/bin/${CTARGET}-{as,ld,ar,nm,objcopy,objdump,ranlib,
  readelf,strip,…}` symlinks → the active `binutils-bin/${VER}` tools (plain
  names for native CHOST).
- Writes `${EPREFIX}/etc/env.d/05binutils` (`PATH`/`ROOTPATH`/`MANPATH`/
  `LDPATH`) and the `${CTARGET}` symlink in `/usr/${CHOST}/${CTARGET}/lib/` etc.
- `--list` / `--get-current-target` / setting an active profile.

## em shape

- New `em select binutils` applet alongside `em select repos`/`profile`
  (`portage-cli/src/select/`), EPREFIX/`--local`-aware via `Roots` so it writes
  into `~/.gentoo` for an unprivileged Prefix.
- Most useful first slice: **auto-run it from the build** (the binutils ebuild's
  `pkg_postinst` calls `binutils-config`); em currently stubs/omits that. Either
  (a) provide a `binutils-config` shim the eclass postinst can call, or (b) have
  the merge driver invoke `em select binutils --target ${CTARGET} <profile>`
  after merging a `sys-devel/binutils` (or `cross-*/binutils`).

## Coordination

Pairs with [[select-compiler]] (`gcc-config`) — same wrapper/env.d mechanism for
the compiler side. Together they are the **toolchain activation** half of
Stage-C; the build half is done.
