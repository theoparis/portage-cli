# Explicit-target reinstall default (emerge `foo && foo` → two installs)

STATUS: **core fixed 2026-06-22; one follow-up symptom open (staged glibc).**
Behaviour difference vs emerge: emerge *reinstalls an explicitly-requested atom
by default* (`[ebuild   R   ]`), so `emerge foo && emerge foo` builds foo twice;
`--noreplace`/`-n` opts out. `em` instead **skipped** a target already in the VDB
at the planned version.

DONE:
- `fix(merge): reinstall an explicitly-requested atom already in the VDB`
  (`7f43c27`): `PlannedMerge.reinstall` (= cpv already installed yet still in the
  plan ⇒ explicit target / USE rebuild); the merge loop builds it instead of
  resume-skipping.
- `fix(merge): treat a same-cpv reinstall as a self-replace` (`bb89327`): dropped
  the `find_slot_occupant(...).filter(|old| old.cpv() != ebuild.cpv())` so the
  installed package is the replace target — own files exempt from collision
  detection, unmerged after the new content lands.

Verified: cross `--setup` now rebuilds **all 6 steps, no skips, no collisions**.

OPEN — **staged glibc still installs headers-only.** With the above, step 5
(full glibc) rebuilds, but the result is still headers-only (622-byte CONTENTS,
no `libc.so`), even though the recorded/plan USE has `-headers-only` and the
process `USE` env is empty. `just_headers()` is `is_crosscompile && use
headers-only`, so the *build shell* evaluated `use headers-only` true despite the
plan. Not a stale work dir (em already pre-build-cleans). Suspect either the
build shell not honouring the plan's `-headers-only` for an at-best-version
reinstall, or the activation gap (no cross `as`/`ld`) forcing a degraded build.
Compare against portage (below) and instrument `use headers-only` in the glibc
build shell. (gcc-stage2 likewise produced no `libstdc++` — same family.)

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
