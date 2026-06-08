//! Bridge between [portage-atom](https://crates.io/crates/portage-atom) and the
//! [PubGrub](https://crates.io/crates/pubgrub) dependency solver.
//!
//! Maps PMS package atoms to PubGrub's `Package`, `Version`, and `VersionSet`
//! traits, and provides a `DependencyProvider` implementation backed by a
//! package repository.
//!
//! # USE Flag Handling
//!
//! The caller resolves the **desired** USE state per package version (profile,
//! `make.conf`, `package.use`, and IUSE defaults are *its* concern) and hands it
//! in; the solver never resolves policy itself. See
//! `docs/use-and-solver-boundary.md` for the current/desired/needed model.
//!
//! USE conditionals in dependency strings are then handled via a hybrid strategy:
//!
//! - **User-decided** flags (`enabled` / `disabled`) are eagerly evaluated at
//!   registration time — the solver never sees those branches.
//! - **Solver-decided** flags are encoded as virtual packages with two versions
//!   (0 = OFF, 1 = ON). PubGrub's one-version-per-package constraint provides
//!   mutual exclusion for free.
//!
//! ## `SolverDecided` is experimental and currently dormant
//!
//! The solver-decided path lets PubGrub *choose* a flag's value to satisfy
//! constraints — strictly more powerful than portage, which freezes USE before
//! resolving. It is the intended lever for two things portage does poorly:
//! automatic `REQUIRED_USE` satisfaction (the constraint is parsed and
//! evaluated by `portage-metadata`, and validated post-solve by the cli — the
//! "Level A" path; solver auto-satisfaction is "Level C", see
//! `docs/use-and-solver-boundary.md`) and minimal-USE-change conflict resolution.
//!
//! No current caller emits [`UseFlagState::SolverDecided`] — the cli hands the
//! solver a fully fixed USE set — so this path is exercised only by tests. It is
//! kept intentionally, but treat it as experimental: before it is useful it
//! needs (a) the fixed-USE mode to match portage well as the baseline, and (b) a
//! preference model so solver-chosen USE stays minimal and predictable. Do not
//! rely on it being load-bearing. The Level-C plan (the `REQUIRED_USE` →
//! `UseDecision` encoding, concern split, opt-in/parity, phasing) is in
//! `docs/required-use-level-c.md`.

mod convert;
mod error;
mod graph;
mod package;
mod provider;
mod repository;
mod required_use;
mod use_config;
mod validate;
mod version_set;

pub use error::{Error, Result};
pub use graph::{DepClass, DepEdge};
pub use portage_atom::interner::{DefaultInterner, Interned};
pub use package::PortagePackage;
pub use provider::{CededFlag, DroppedDep, InstalledPackage, InstalledPolicy, PortageDependencyProvider, UseFlagRequirement};
pub use repository::{
    IUseDefault, InMemoryRepository, PackageDeps, PackageRepository, PackageVersions,
};
pub use required_use::RequiredUse;
pub use use_config::{UseConfig, UseFlagState, apply_package_use};
pub use validate::SlotOperatorBinding;
pub use version_set::PortageVersionSet;
