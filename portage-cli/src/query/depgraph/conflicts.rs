use std::collections::HashMap;

use portage_atom::{Cpn, Dep, DepEntry, Operator, Version};
use portage_atom_pubgrub::PortageVersionSet;

use super::installed::VdbEntry;

/// A constraint violated by the proposed solution.
pub(super) struct Conflict {
    /// The installed package whose dep is violated.
    pub installed_cpn: Cpn,
    pub installed_ver: Version,
    /// The dep atom that is not satisfied.
    pub dep: Dep,
    /// The version the solver chose (which violates the dep).
    pub proposed_ver: Version,
}

/// Check all installed packages' dep strings against the proposed solution.
///
/// Returns one `Conflict` per violated constraint.  The proposed solution is
/// expressed as a map from CPN to the version that would be installed or
/// upgraded to.
pub(super) fn find_conflicts(
    installed: &[VdbEntry],
    proposed: &HashMap<Cpn, Version>,
) -> Vec<Conflict> {
    let mut conflicts = Vec::new();

    for entry in installed {
        let active_flags: std::collections::HashSet<&str> = entry
            .active_use
            .iter()
            .map(|f| f.as_str())
            .collect();

        let evaluated =
            DepEntry::evaluate_use(&entry.deps, |f| active_flags.contains(f));

        collect_violations(&evaluated, entry, proposed, &mut conflicts);
    }

    conflicts
}

fn collect_violations(
    entries: &[DepEntry],
    owner: &VdbEntry,
    proposed: &HashMap<Cpn, Version>,
    out: &mut Vec<Conflict>,
) {
    for entry in entries {
        match entry {
            DepEntry::Atom(dep) => {
                if dep.blocker.is_some() {
                    continue;
                }
                let Some(proposed_ver) = proposed.get(&dep.cpn) else {
                    continue;
                };
                let vs = dep_to_version_set(dep);
                if !vs.contains(proposed_ver) {
                    out.push(Conflict {
                        installed_cpn: owner.cpn,
                        installed_ver: owner.version.clone(),
                        dep: dep.clone(),
                        proposed_ver: proposed_ver.clone(),
                    });
                }
            }
            DepEntry::AllOf(children) => {
                collect_violations(children, owner, proposed, out);
            }
            // AnyOf: a conflict only exists if ALL alternatives are violated.
            DepEntry::AnyOf(children) => {
                let branch_violations: Vec<Vec<Conflict>> = children
                    .iter()
                    .map(|child| {
                        let mut v = Vec::new();
                        collect_violations(
                            std::slice::from_ref(child),
                            owner,
                            proposed,
                            &mut v,
                        );
                        v
                    })
                    .collect();
                // If every branch is violated, the OR group as a whole is violated.
                // We report the first branch's violations as representative.
                let all_violated = branch_violations.iter().all(|v| !v.is_empty());
                if all_violated {
                    if let Some(first) = branch_violations.into_iter().next() {
                        out.extend(first);
                    }
                }
            }
            _ => {}
        }
    }
}

fn dep_to_version_set(dep: &Dep) -> PortageVersionSet {
    match &dep.version {
        None => PortageVersionSet::any(),
        Some(v) => {
            let op = dep.op.unwrap_or(Operator::GreaterOrEqual);
            PortageVersionSet::from_operator(op, dep.glob, v.clone())
        }
    }
}
