# portage-solver

Solver-agnostic vocabulary and [`Solver`] trait for Gentoo Portage dependency
resolution.

Shared layer between the two solver bridges
[`portage-atom-pubgrub`](https://crates.io/crates/portage-atom-pubgrub) and
[`portage-atom-resolvo`](https://crates.io/crates/portage-atom-resolvo).

## Overview

- **Facts vocabulary** — `PackageRepository`, `VersionFacts`, `PackageDeps`
- **USE policy vocabulary** — `UseConfig`, `UseFlagState`, `apply_package_use`
- **Solution vocabulary** — `SelectedPackage`, `DepEdge`, `TargetSpec`
- **`Solver` trait** — single interface both bridges implement for cross-checking

Depends only on [`portage-atom`](https://crates.io/crates/portage-atom); no
pubgrub or resolvo.

[`Solver`]: https://docs.rs/portage-solver/latest/portage_solver/trait.Solver.html

## License

MIT