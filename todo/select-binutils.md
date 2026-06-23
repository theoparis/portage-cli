# `em select binutils` — `binutils-config` workalike

STATUS: **implemented.** `em select binutils` now provides a
`binutils-config`/`eselect binutils` workalike. Implemented features:
- `em select binutils list` — lists all binutils profiles grouped by target architecture
- `em select binutils show [--target <CTARGET>]` — shows current profile for target
- `em select binutils set <profile> [--target <CTARGET>]` — activates a profile
- Per-architecture grouping with `*` marking active profiles
- Respects `--config-root`, `--local`, `--prefix` flags
- Falls back to `/etc/env.d/binutils` for system-wide profiles
- Auto-detects CHOST from make.conf

REMAINING: Need to add `em select linker` for linker-specific handling as mentioned
by the user. See coordination with [[select-compiler]].

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
