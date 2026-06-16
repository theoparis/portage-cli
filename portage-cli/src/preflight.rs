//! Pre-flight build-dependency check (docs/build-roadmap.md, M2).
//!
//! Before the (potentially hours-long) build loop, verify that every plan
//! entry's build-time dependencies are present in the set that will be visible
//! when it builds:
//!
//! - `DEPEND` is resolved against the **base system** view
//!   `VDB(base) ∪ VDB(target)` (what `SYSROOT`/`ESYSROOT` point at), and
//! - `BDEPEND` against the **host** `BROOT` (always `/`).
//!
//! Both sets grow with each earlier plan entry: a package merged earlier in the
//! run is visible to everything after it (root-model.md "within-run
//! visibility"). The solver already produces a complete dependency closure, so
//! this is a guard rail — it turns a would-be mid-build "command not found" /
//! missing-library failure into a clear, early error that names the dep.

use std::collections::HashSet;

use anyhow::{Result, bail};
use portage_atom::{Cpv, DepEntry};

use crate::bdepend_avail::{Avail, collect_unsatisfied};
use crate::cli::Roots;
use portage_atom_pubgrub::MergeRoot;

use crate::query::depgraph::PlannedMerge;

/// Verify the plan's build dependencies are satisfiable in install order.
///
/// Returns an error listing every unsatisfied requirement (package → missing
/// atoms) when the check fails; `Ok(())` otherwise.
pub fn check(plan: &[PlannedMerge], roots: &Roots) -> Result<()> {
    let mut depend_avail = Avail::initial_depend(roots);
    let mut bdepend_avail = Avail::initial_bdepend(roots);

    let mut problems: Vec<String> = Vec::new();
    for planned in plan {
        let active: HashSet<&str> = planned.use_flags.iter().map(String::as_str).collect();
        let is_active = |f: &str| active.contains(f);

        let depend = DepEntry::evaluate_use(&planned.depend, is_active);
        let bdepend = DepEntry::evaluate_use(&planned.bdepend, is_active);

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