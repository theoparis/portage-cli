# `em select compiler` — `gcc-config` workalike

STATUS: **not started.** The compiler-side twin of [[select-binutils]]. After a
(cross) gcc merge the binaries land under
`${EPREFIX}/usr/${CBUILD}/${CTARGET}/gcc-bin/${VER}/${CTARGET}-{gcc,g++,cpp,…}`
plus an `env.d/gcc/${CTARGET}-${VER}` entry, but the active-profile wiring
(`/usr/bin` wrappers for the native CHOST, `env.d/05gcc` PATH/LDPATH, the gcc
`config` files, the `${CTARGET}` symlinks) is not created — so the toolchain is
installed but not "selected". Found via the crossdev clean-slate reinstall
([[crossdev-target]] — gap #2).

## What gcc-config does (to replicate)

- Reads `${EPREFIX}/etc/env.d/gcc/${CTARGET}-${VER}` (CHOST/CTARGET, LDPATH,
  the gcc-bin dir) → active profile per target.
- Native: `/usr/bin/{gcc,g++,cpp,c++,…}` wrappers + `${CHOST}-gcc` aliases.
  Cross: the `${CTARGET}-gcc` etc. resolve their cross `as`/`ld` (depends on
  [[select-binutils]] having run).
- Writes `${EPREFIX}/etc/env.d/05gcc-${CTARGET}` and updates `ld.so.conf`/LDPATH.
- `--list-profiles` / `--get-current-profile` / `gcc-config <profile>`.

## em shape

- `em select compiler` (or `em select gcc`) applet under
  `portage-cli/src/select/`, `Roots`/`--local`-aware.
- As with binutils, the highest-value slice is **auto-activation from the
  merge**: gcc's `pkg_postinst` runs `gcc-config`; provide the shim or invoke
  `em select compiler` after merging a gcc. Order: binutils-config then
  gcc-config (gcc's wrappers reference the binutils tools).

## Completion check

`${CTARGET}-gcc hello.c -o hello && file hello` → `ELF … RISC-V`. That closes
the Stage-C activation gap (the build pipeline is already done).
