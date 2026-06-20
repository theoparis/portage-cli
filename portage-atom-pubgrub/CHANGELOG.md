# Changelog

## 0.6.0

### Breaking changes

- `apply_package_use` now takes pre-parsed `&[(Dep, Vec<UseOverride>)]` instead
  of `&[(Dep, Vec<String>)]`. Flags are parsed (`+flag`/`flag` → on, `-flag` →
  off) and interned once at config-read time, so the per-version apply path does
  no string work. New public `UseOverride { flag, enable }` with `parse`.

## 0.5.0

This release covers the large body of work accumulated since 0.4.x; the public
API changed in several breaking ways (verified with `cargo semver-checks`).

### Breaking changes

- `PortageDependencyProvider::new` now takes a single `repo` argument; USE
  configuration and `package.use` are resolved by the caller (the
  `PackageRepository::desired_use` trait) rather than passed in.
- `UseConfig::solver_decide` gained a `prefer` argument, and `UseFlagState`
  gained a `SolverDecided { prefer }` variant, for Level-C `REQUIRED_USE`
  auto-satisfaction.
- `add_installed_blockers` now takes `&PortagePackage` and `&[Dep]`.
- Additional `PortagePackage` / dependency-class shapes and a new repository
  trait method; downstream impls may need updating.

### Features

- Level-C `REQUIRED_USE` auto-satisfaction (opt-in `--autosolve-use`):
  encode `a? (…)`, `||`, `^^`, `??`, and nested ceded-guard chains over
  `UseDecision` virtual nodes with preference-biased selection.
- `--deep`/emptytree: bump `:*` any-slot deps to the newest slot.
- Cross-package `[flag]` USE-dep co-solve.
- Installed-package blocker registration and reporting.
- `||` provider preference is now version-aware: when every branch of a
  provider group is installed, keep the branch reaching the newest installed
  version (matching emerge's `dep_zapdeps`), e.g. source `rust` over `rust-bin`.

### Documentation

- Document all public items and enable `#![warn(missing_docs)]`.

### Other

- Depend on `portage-atom` 0.10.
- Raise MSRV to 1.92.
