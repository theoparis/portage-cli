# Build-dir clean + environment handling — portage parity

STATUS: **partial; auditing against portage (2026-06-22).** Triggered by the
staged cross glibc still building headers-only on a reinstall
([[reinstall-default]], [[crossdev-target]]). Comparison of em's build-dir
lifecycle to portage `bin/phase-functions.sh` / `bin/misc-functions.sh`
(portage-3.0.79).

## What portage does

- **`__dyn_clean`** (`phase-functions.sh:329-350`): always removes `image/`,
  `homedir/`, `empty/`, `.installed`. Removes `${T}` (temp — which holds the
  saved `environment`) unless `keeptemp`/`keepwork`. Removes `${WORKDIR}`, the
  phase **stamp files** (`.unpacked/.configured/.compiled/.tested/…`), and
  `build-info/` unless `keepwork`. A merge starts from a clean builddir; a build
  is *resumed* only via the stamps when keepwork keeps them.
- **Phase resume via stamps** (`.unpacked` at `:305`, `.configured` at `:391`,
  …): a phase is skipped when its stamp exists. Lets an interrupted build resume
  without recompiling.
- **Per-phase environment save/source** (`phase-functions.sh:200-237`, `:212`):
  after each phase the (filtered) bash env is written to `${T}/environment`; the
  next phase sources it, carrying global-scope state (`S`, USE-derived vars,
  functions). `${T}/environment` therefore records `USE=…` — **the lingering-USE
  vector** if a builddir/`${T}` is reused across builds with different USE.
- **Post-merge clean**: gated on `keepwork`/`noclean` (and `merge-wait` for the
  early WORKDIR drop, `misc-functions.sh:256-262`).
- FEATURES that gate cleaning: **`keepwork`**, **`noclean`**, **`keeptemp`**.

## What em does (and the gaps)

- **Single carried build shell** across phases (`shell.rs:786`) instead of
  portage's per-phase `${T}/environment` save/source — so there is no
  `${T}/environment` file to leak, and no phase stamps. em **re-runs every phase**
  each `build_and_merge` (safer for a rebuild; no resume).
- **Pre-build clean** (`ebuild.rs:411`): removes `work/ image/ temp/ homedir`
  when `merge_mode && !keepwork`. **Post-merge clean** (`:452`): same gating.
- Gaps vs portage:
  1. Only **`keepwork`** is honoured — **`noclean`** and **`keeptemp`** are not.
     Add them (noclean ⇒ skip both cleans; keeptemp ⇒ keep `temp/`).
  2. No phase **stamp files** ⇒ no interrupted-build resume (em rebuilds from
     scratch). Acceptable for now; revisit if rebuild cost matters.
  3. The carried-shell model means USE must be (re)applied to the shell from the
     plan and not inherited from a previous package's shell. Confirm a fresh
     shell per `build_and_merge` (it is) and that `set_use_flags(plan)` fully
     determines `use <flag>` — the open glibc symptom suggests verifying this.

## Next

Instrument the cross glibc build shell (`use headers-only`, `${USE}`) on a
reinstall to settle whether the headers-only result is a USE-application bug in
the carried shell or a downstream effect of the missing cross `as`/`ld`
([[select-binutils]]). Then add `noclean`/`keeptemp` FEATURES parity.
