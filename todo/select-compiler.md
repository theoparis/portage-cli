# `em select compiler` — `gcc-config` workalike

STATUS: **implemented (incl. wrapper symlinks, 2026-06-24).** Like
[[select-binutils]], the wrapper half was missing and is now done:
`install_wrappers` reads the env.d `GCC_PATH` and creates
`usr/bin/<T>-<tool>` → `<GCC_PATH>/<T>-<tool>` (gcc-bin binaries are already
`<T>-`prefixed) plus the `<T>-cc` alias, always re-rooted under `<EPREFIX>` so a
prefix install links its own gcc, not a same-pathed host copy. Unit-tested
(`cross_wrappers_link_gcc_bin_directly`). STILL TODO: auto-invoke from
`crossdev --setup` after the gcc-stage1/stage2 steps (binutils-config first). See
[[crossdev-target]].

`em select compiler` (and its `gcc` alias) now provides
a `gcc-config`/`eselect gcc` workalike. Implemented features:
- `em select compiler list` — lists all gcc profiles grouped by target architecture
- `em select compiler show [--target <CTARGET>]` — shows current profile for target
- `em select compiler set <profile> [--target <CTARGET>]` — activates a profile
- Per-architecture grouping with `*` marking active profiles
- Respects `--config-root`, `--local`, `--prefix` flags
- Falls back to `/etc/env.d/gcc` for system-wide profiles
- Auto-detects CHOST from make.conf
- When using `--local` or `--prefix`, shows both host and prefix profiles with color-coded
  `(host)` / `(prefix)` labels to disambiguate sources

REMAINING: The user mentioned wanting `--gcc` and `--clang` flags for compiler-specific
handling. However, clang now has its own `em select clang` subcommand (Option A)
for LLVM slot selection, since it uses a different mechanism than gcc.
See [[select-clang]] for clang support and [[select-binutils]], [[select-linker]]
for the other toolchain tools.

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
