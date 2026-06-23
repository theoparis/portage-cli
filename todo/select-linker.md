# `em select linker` — linker profile selection

STATUS: **implemented.** `em select linker` provides linker profile selection
for ld, lld, mold, etc. Implemented features:
- `em select linker list` — lists all linker profiles grouped by target architecture
- `em select linker show [--target <CTARGET>]` — shows current linker profile for target
- `em select linker set <profile> [--target <CTARGET>]` — activates a linker profile
- Per-architecture grouping with `*` marking active profiles
- Respects `--config-root`, `--local`, `--prefix` flags
- Falls back to `/etc/env.d/linker` for system-wide profiles
- Auto-detects CHOST from make.conf

RELATED: Works alongside [[select-compiler]] and [[select-binutils]] for
complete toolchain activation.
