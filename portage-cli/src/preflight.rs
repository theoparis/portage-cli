//! Pre-flight build-dependency check (docs/build-roadmap.md, M2).
//!
//! Before the (potentially hours-long) build loop, verify that every plan
//! entry's build-time dependencies are present in the set that will be visible
//! when it builds:
//!
//! - `DEPEND` is resolved against the **base system** view
//!   `VDB(base) ∪ VDB(target)` (what `SYSROOT`/`ESYSROOT` point at), and
//! - `BDEPEND` against the build host's own `BROOT`: `Cli::base_roots()`,
//!   *not* unconditionally bare `/` (a `--root`/`--prefix`/`--local`
//!   invocation's Host BDEPEND merges land in `base_roots()`, so that's what
//!   must be checked — see `todo/stage-build-shakeout.md` #28/#30).
//!
//! Both sets grow with each earlier plan entry: a package merged earlier in the
//! run is visible to everything after it (root-model.md "within-run
//! visibility"). The solver already produces a complete dependency closure, so
//! this is a guard rail — it turns a would-be mid-build "command not found" /
//! missing-library failure into a clear, early error that names the dep.

use std::collections::HashSet;

use anyhow::{Result, bail};
use portage_atom::interner::Interned;
use portage_atom::{Cpv, DepEntry};

use crate::bdepend_avail::{Avail, collect_unsatisfied};
use crate::cli::Roots;
use portage_atom_pubgrub::MergeRoot;

use crate::query::depgraph::PlannedMerge;

/// Verify the plan's build dependencies are satisfiable in install order.
///
/// `host_roots` must be `Cli::base_roots()` — see [`Avail::initial_bdepend`].
///
/// Returns an error listing every unsatisfied requirement (package → missing
/// atoms) when the check fails; `Ok(())` otherwise.
pub fn check(plan: &[PlannedMerge], roots: &Roots, host_roots: &Roots) -> Result<()> {
    let mut depend_avail = Avail::initial_depend(roots);
    let mut bdepend_avail = Avail::initial_bdepend(host_roots);

    let mut problems: Vec<String> = Vec::new();
    for planned in plan {
        let active: HashSet<Interned<_>> = planned.use_flags.iter().copied().collect();
        let depend = DepEntry::evaluate_use(&planned.depend, &active);
        let bdepend = DepEntry::evaluate_use(&planned.bdepend, &active);

        let mut missing: Vec<String> = Vec::new();
        collect_unsatisfied(&depend, &depend_avail, &mut missing);
        collect_unsatisfied(&bdepend, &bdepend_avail, &mut missing);
        if !missing.is_empty() {
            missing.sort();
            missing.dedup();
            problems.push(format!("  {} needs: {}", planned.cpv, missing.join(", ")));
        }

        // Within-run visibility: this entry satisfies later entries' deps once
        // merged. Slot is left unknown (None) — a permissive presence check,
        // which is the right bias for a guard that must not block a valid plan.
        if let Ok(cpv) = Cpv::parse(&planned.cpv) {
            match planned.merge_root {
                MergeRoot::Host => bdepend_avail.record_merge_bdepend(cpv),
                MergeRoot::Target => bdepend_avail.record_target_merge(&mut depend_avail, cpv),
            }
        }
    }

    if !problems.is_empty() {
        bail!(
            "pre-flight dependency check failed — these build dependencies are not \
             satisfied by the installed view or earlier plan entries:\n{}",
            problems.join("\n")
        );
    }
    Ok(())
}
