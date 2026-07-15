//! Resolution-policy and plan layer for the
//! [`em`](https://github.com/lu-zero/portage-cli) Portage CLI.
//!
//! Turns repository facts + configuration into a solved, ordered merge plan
//! via the [`portage-atom-pubgrub`](https://crates.io/crates/portage-atom-pubgrub)
//! solver bridge — USE/keyword/mask policy, root-aware post-solve trimming,
//! and plan assembly — currently being migrated out of `portage-cli`'s
//! `query::depgraph` module in stages (see that project's `todo/PENDING.md`
//! for the plan). Computes policy; renders nothing (no clap, no
//! anstream/anstyle dependency — that boundary is deliberate).
//!
//! Unpublishable past its placeholder `v0.0.1` (see `Cargo.toml`): depends on
//! `portage-repo`, which pulls in the brush fork via git.
#![warn(missing_docs)]

mod bdepend_avail;
/// Effective per-package USE after profile/env overrides, IUSE defaults, and
/// `--autosolve-use` ceded flags.
pub mod effective_use;
/// Profile USE `use.force`/`use.mask` (global and per-package), applied as
/// the unconditional post-fold step real portage uses.
pub mod force_mask;
/// Repository-fact adaptation: the `PackageRepository` impl the solver
/// bridge consumes, plus keyword/mask/license acceptance.
pub mod repo;
mod roots;

pub use bdepend_avail::{Avail, broot_vdb_packages, collect_unsatisfied, unsatisfied_cpns};
pub use roots::Roots;
