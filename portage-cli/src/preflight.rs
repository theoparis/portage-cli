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
use camino::Utf8Path;
use portage_atom::{Cpv, Dep, DepEntry};
use portage_vdb::Vdb;

use crate::cli::Roots;
use crate::query::depgraph::PlannedMerge;

/// Verify the plan's build dependencies are satisfiable in install order.
///
/// Returns an error listing every unsatisfied requirement (package → missing
/// atoms) when the check fails; `Ok(())` otherwise.
pub fn check(plan: &[PlannedMerge], roots: &Roots) -> Result<()> {
    // DEPEND is found in the base sysroot (base ∪ target, target shadowing on a
    // duplicate cpv — here a union is enough); BDEPEND tools run from the host.
    let mut depend_avail = vdb_cpvs(roots.base());
    if roots.target() != roots.base() {
        depend_avail.extend(vdb_cpvs(roots.target()));
    }
    let mut bdepend_avail = vdb_cpvs(None);

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
            depend_avail.push((cpv.clone(), None));
            bdepend_avail.push((cpv, None));
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

/// Installed `(cpv, main-slot)` pairs from a root's VDB. `None` = host
/// `/var/db/pkg`. A missing/unreadable VDB yields an empty set.
fn vdb_cpvs(root: Option<&Utf8Path>) -> Vec<(Cpv, Option<String>)> {
    let vdb = match root {
        Some(r) => Vdb::open(r.join("var/db/pkg")),
        None => Vdb::open_default(),
    };
    let Ok(vdb) = vdb else {
        return Vec::new();
    };
    vdb.packages()
        .into_iter()
        .map(|p| (p.cpv().clone(), p.slot_main().ok()))
        .collect()
}

/// Append the display form of each unsatisfied requirement in `entries` to
/// `out`. `UseConditional`s are assumed already resolved by `evaluate_use`.
fn collect_unsatisfied(
    entries: &[DepEntry],
    avail: &[(Cpv, Option<String>)],
    out: &mut Vec<String>,
) {
    for e in entries {
        match e {
            // Blockers are a "must not be present" constraint, not a build
            // requirement — the merge driver / VDB handle them, not this check.
            DepEntry::Atom(dep) if dep.blocker.is_some() => {}
            DepEntry::Atom(dep) => {
                if !atom_satisfied(dep, avail) {
                    out.push(dep.to_string());
                }
            }
            DepEntry::AllOf(children) => collect_unsatisfied(children, avail, out),
            // An `|| ( ... )` group is satisfied if any alternative is; report
            // the whole group (Display renders it as `|| ( a b )`) when none is.
            any @ DepEntry::AnyOf(children) => {
                if !group_satisfied(children, avail) {
                    out.push(any.to_string());
                }
            }
            // `^^`/`??` are REQUIRED_USE operators, not real build-dep groups;
            // they don't express a presence requirement, so never report them.
            DepEntry::ExactlyOneOf(_) | DepEntry::AtMostOneOf(_) => {}
            DepEntry::UseConditional { .. } => {}
        }
    }
}

fn group_satisfied(entries: &[DepEntry], avail: &[(Cpv, Option<String>)]) -> bool {
    entries.iter().any(|e| entry_satisfied(e, avail))
}

fn entry_satisfied(e: &DepEntry, avail: &[(Cpv, Option<String>)]) -> bool {
    match e {
        DepEntry::Atom(dep) => dep.blocker.is_some() || atom_satisfied(dep, avail),
        DepEntry::AllOf(c) => c.iter().all(|e| entry_satisfied(e, avail)),
        DepEntry::AnyOf(c) => group_satisfied(c, avail),
        DepEntry::ExactlyOneOf(_) | DepEntry::AtMostOneOf(_) => true,
        DepEntry::UseConditional { .. } => true,
    }
}

fn atom_satisfied(dep: &Dep, avail: &[(Cpv, Option<String>)]) -> bool {
    avail
        .iter()
        .any(|(cpv, slot)| dep.matches_cpv(cpv, slot.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atoms(specs: &[&str]) -> Vec<(Cpv, Option<String>)> {
        specs
            .iter()
            .map(|s| (Cpv::parse(s).unwrap(), None))
            .collect()
    }

    fn parse(dep: &str) -> Vec<DepEntry> {
        DepEntry::parse(dep).unwrap()
    }

    #[test]
    fn satisfied_atom_is_not_reported() {
        let avail = atoms(&["dev-libs/foo-1.2"]);
        let mut out = Vec::new();
        collect_unsatisfied(&parse(">=dev-libs/foo-1.0"), &avail, &mut out);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn missing_atom_is_reported() {
        let avail = atoms(&["dev-libs/foo-1.2"]);
        let mut out = Vec::new();
        collect_unsatisfied(&parse("dev-libs/bar"), &avail, &mut out);
        assert_eq!(out, ["dev-libs/bar"]);
    }

    #[test]
    fn version_too_low_is_reported() {
        let avail = atoms(&["dev-libs/foo-1.2"]);
        let mut out = Vec::new();
        collect_unsatisfied(&parse(">=dev-libs/foo-2.0"), &avail, &mut out);
        assert_eq!(out, [">=dev-libs/foo-2.0"]);
    }

    #[test]
    fn any_of_satisfied_when_one_member_present() {
        let avail = atoms(&["dev-libs/b-1"]);
        let mut out = Vec::new();
        collect_unsatisfied(&parse("|| ( dev-libs/a dev-libs/b )"), &avail, &mut out);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn any_of_reports_whole_group_when_none_present() {
        let avail = atoms(&["dev-libs/c-1"]);
        let mut out = Vec::new();
        collect_unsatisfied(&parse("|| ( dev-libs/a dev-libs/b )"), &avail, &mut out);
        assert_eq!(out, ["|| ( dev-libs/a dev-libs/b )"]);
    }

    #[test]
    fn blockers_are_ignored() {
        let avail: Vec<(Cpv, Option<String>)> = Vec::new();
        let mut out = Vec::new();
        collect_unsatisfied(&parse("!dev-libs/foo"), &avail, &mut out);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn inactive_use_conditional_drops_the_dep() {
        let avail: Vec<(Cpv, Option<String>)> = Vec::new();
        let entries = DepEntry::evaluate_use(&parse("ssl? ( dev-libs/openssl )"), |_| false);
        let mut out = Vec::new();
        collect_unsatisfied(&entries, &avail, &mut out);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn active_use_conditional_keeps_the_dep() {
        let avail: Vec<(Cpv, Option<String>)> = Vec::new();
        let entries = DepEntry::evaluate_use(&parse("ssl? ( dev-libs/openssl )"), |f| f == "ssl");
        let mut out = Vec::new();
        collect_unsatisfied(&entries, &avail, &mut out);
        assert_eq!(out, ["dev-libs/openssl"]);
    }
}
