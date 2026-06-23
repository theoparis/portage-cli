# `em select linker` — linker profile selection

STATUS: **implemented.** `em select linker` provides linker profile selection
for ld, lld, mold, etc. using the same env.d mechanism as gcc/binutils.

## Implemented features:
- `em select linker list` — lists all linker profiles grouped by target architecture
- `em select linker show [--target <CTARGET>]` — shows current linker profile for target
- `em select linker set <profile> [--target <CTARGET>]` — activates a linker profile
- Per-architecture grouping with `*` marking active profiles
- Respects `--config-root`, `--local`, `--prefix` flags
- Falls back to `/etc/env.d/linker` for system-wide profiles
- Auto-detects CHOST from make.conf
- When using `--local` or `--prefix`, shows both host and prefix profiles with color-coded
  `(host)` / `(prefix)` labels to disambiguate sources

## Note on Clang linker configuration

Clang uses a different mechanism for linker selection via `/etc/clang/${SLOT}/gentoo-linker.cfg`
which contains `-fuse-ld=lld` or similar. This is separate from the binutils/linker profiles.
The current `em select linker` implementation follows the binutils-config pattern (env.d/linker/),
while clang's linker config is managed separately. See [[select-clang]] for details on how
clang linker configuration could be integrated.

## Related

- [[select-compiler]] — GCC compiler profile selection
- [[select-binutils]] — Binutils profile selection
- [[select-clang]] — Clang/LLVM version and linker configuration (future)
