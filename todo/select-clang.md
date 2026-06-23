# `em select clang` — Clang/LLVM version and linker configuration

STATUS: **Option A implemented.** Clang in Gentoo uses a different mechanism than gcc:

## How Clang is deployed in Gentoo

Unlike gcc which uses `env.d/gcc/` profiles, clang works as follows:

1. **Multiple LLVM slots**: Clang is installed under `/usr/lib/llvm/${SLOT}/bin/`
   (e.g., `/usr/lib/llvm/22/bin/clang`)

2. **Symlinks**: The `llvm-core/clang-toolchain-symlinks` package creates:
   - `/usr/lib/llvm/${SLOT}/bin/clang` → actual clang binary
   - `/usr/lib/llvm/${SLOT}/bin/${CHOST}-clang` → cross-compilation wrappers
   - Optional: `cc` → `clang`, `gcc` → `clang` (via USE flags)

3. **Linker configuration**: `/etc/clang/${SLOT}/gentoo-linker.cfg` contains:
   ```
   -fuse-ld=lld  # or bfd, gold, etc.
   ```
   This file is created by `llvm-core/clang-linker-config` package.

## What could `em select clang` do?

### Option A: LLVM slot selection
Select which LLVM/clang version is "active" by managing symlinks or a config file.
This would be similar to `gcc-config` but for LLVM.

### Option B: Linker configuration only
Manage the `-fuse-ld=` setting in `/etc/clang/${SLOT}/gentoo-linker.cfg`.
This could be integrated into `em select linker` instead.

### Option C: Both
- `em select clang list` — list available LLVM slots
- `em select clang set <slot>` — set default LLVM slot
- `em select clang linker <ld>` — configure which linker clang uses

## Implementation notes

- Clang doesn't use the `env.d/` mechanism like gcc/binutils
- The configuration is spread across:
  - `/usr/lib/llvm/${SLOT}/` — binaries
  - `/etc/clang/${SLOT}/` — configuration
  - Symlinks in various locations

## Related

- [[select-linker]] — for linker selection (could be unified with clang linker config)
- [[select-compiler]] — for gcc (different mechanism)
- [[select-binutils]] — for binutils (different mechanism)

## Implementation status

- **Option A: LLVM slot selection** ✅ **IMPLEMENTED**
  - `em select clang list` — lists available LLVM slots with full target tuples
    (e.g., `clang-21 [aarch64-unknown-linux-gnu, riscv64-unknown-linux-gnu]`)
  - `em select clang show` — shows current active slot
  - `em select clang set <slot>` — sets active LLVM slot
  - Works with `--config-root`, `--local`, `--prefix`
  - Shows host/prefix labels when in prefix context

- **Option B: Linker configuration** ⏳ **NOT STARTED**
  - Would manage `-fuse-ld=` in `/etc/clang/${SLOT}/gentoo-linker.cfg`
  - Could be integrated into `em select linker` or a separate `em select clang linker` subcommand

## Decision needed

Should linker configuration for clang be:
1. Part of `em select linker` (unified linker management)?
2. A separate `em select clang linker` subcommand?
3. Integrated into `em select clang set` with additional flags?
