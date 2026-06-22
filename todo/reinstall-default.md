# Explicit-target reinstall default (emerge `foo && foo` → two installs)

STATUS: **in progress (2026-06-22).** Behaviour difference vs emerge: emerge
*reinstalls an explicitly-requested atom by default* (shown `[ebuild   R   ]`),
so `emerge foo && emerge foo` builds foo twice; `--noreplace`/`-n` opts out. `em`
instead **skips** a target already in the VDB at the planned version, so the
second `em foo` is a no-op.

## Why it matters now

The cross toolchain bootstrap ([[crossdev-target]]) is staged as repeated merges
of the *same CPV* with different USE: `glibc[headers-only]`→`glibc[]`,
`gcc[stage1]`→`gcc[stage2]`. Each later stage explicitly names the package, so
the emerge **replace** default would rebuild it. `em` skips it →
full-glibc/gcc-stage2 never build (no `libc.so`; gcc is stage1-only).

## Mechanism (where em diverges)

- `run_merge_plan` (main.rs ~335) treats a plan entry already recorded in the
  target VDB at the planned version as *resume → skip*. That conflates two cases:
  (a) merged earlier **in this same run** (legit resume), and (b) pre-existing
  from a prior invocation (emerge would still reinstall an explicit target).
- The resolver also needs to *list* an installed explicit target as `R` rather
  than dropping it (it mostly does for the toolchain steps — they showed `R`/`U`).

## Fix direction

Distinguish "merged during this run" from "already in VDB at start". Skip only
the former (true resume). An explicitly-requested atom is reinstalled even at the
best installed version (emerge default); a `--noreplace`/`-n` flag restores the
skip. Satisfied *dependencies* are still not reinstalled (only named atoms get
the replace treatment), so this is not `--emptytree`.

Relationship: [[newuse]] is the USE-aware, deps-included rebuild; this item is
the blunt "named atom always reinstalls" emerge default. The toolchain needs the
latter (stages name the package); both are worth having.
