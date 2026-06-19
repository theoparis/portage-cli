//! BROOT / within-run package availability for `BDEPEND` checks.
//!
//! Shared by [`crate::preflight`] (validate) and the depgraph post-solve
//! `BDEPEND` trim pass.

use camino::Utf8Path;
use portage_atom::{Cpn, Cpv, Dep, DepEntry};
use portage_atom_pubgrub::MergeRoot;
use portage_vdb::Vdb;

use crate::cli::Roots;

/// Installed `(cpv, main-slot)` pairs visible for dependency presence checks.
#[derive(Debug, Clone, Default)]
pub struct Avail(Vec<(Cpv, Option<String>)>);

impl Avail {
    /// `BDEPEND` availability at the start of a run: host `BROOT`, plus the
    /// prefix target VDB for in-place `--local` (`EPREFIX`) builds.
    pub fn initial_bdepend(roots: &Roots) -> Self {
        let mut out = vdb_cpvs(None);
        if roots.eprefix().is_some() {
            out.extend(vdb_cpvs(roots.target()));
        }
        Self(out)
    }

    /// `DEPEND` availability at the start of a run: `VDB(base) ∪ VDB(target)`.
    pub fn initial_depend(roots: &Roots) -> Self {
        let mut out = vdb_cpvs(roots.base());
        if roots.target() != roots.base() {
            out.extend(vdb_cpvs(roots.target()));
        }
        Self(out)
    }

    /// `DEPEND` availability against a fixed sysroot (`ESYSROOT`). `None` is
    /// the host `/var/db/pkg`.
    pub fn initial_sysroot_depend(sysroot: Option<&camino::Utf8Path>) -> Self {
        Self(vdb_cpvs(sysroot))
    }

    /// Target `ROOT` visibility from an explicit set of installed CPVs.
    pub fn from_cpvs(cpvs: Vec<(Cpv, Option<String>)>) -> Self {
        Self(cpvs)
    }

    /// Record a host merge visible to later `BDEPEND` checks.
    pub fn record_merge_bdepend(&mut self, cpv: Cpv) {
        self.0.push((cpv, None));
    }

    /// Record a target merge for both DEPEND and BDEPEND views (preflight).
    pub fn record_target_merge(&mut self, depend: &mut Self, cpv: Cpv) {
        depend.0.push((cpv.clone(), None));
        self.0.push((cpv, None));
    }

    /// Record a merge for within-run `BDEPEND` trim (host or target).
    pub fn record_merge(&mut self, cpv: Cpv, _merge_root: MergeRoot) {
        self.0.push((cpv, None));
    }

    pub fn atom_satisfied(&self, dep: &Dep) -> bool {
        self.0
            .iter()
            .any(|(cpv, slot)| dep.matches_cpv(cpv, slot.as_deref()))
    }

    /// `true` when `entries` contain an unsatisfied atom on `cpn`.
    pub fn has_unsatisfied_atom_for_cpn(&self, entries: &[DepEntry], cpn: Cpn) -> bool {
        entries
            .iter()
            .any(|e| entry_unsatisfied_for_cpn(e, cpn, self))
    }
}

/// Installed `(cpv, main-slot)` pairs from a root's VDB. `None` = host
/// `/var/db/pkg`. A missing/unreadable VDB yields an empty set.
pub fn vdb_cpvs(root: Option<&Utf8Path>) -> Vec<(Cpv, Option<String>)> {
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

/// The CPNs of every unsatisfied (non-blocker) atom in `entries`. An `AnyOf`
/// (`||`) group contributes its branch CPNs only when the whole group is
/// unsatisfied. `UseConditional`s are assumed already resolved by
/// `evaluate_use`. Used to find build-dep edges a root lacks (e.g. the native
/// offset host build-closure walk).
pub fn unsatisfied_cpns(entries: &[DepEntry], avail: &Avail) -> Vec<Cpn> {
    let mut out = Vec::new();
    unsat_cpns_rec(entries, avail, &mut out);
    out
}

fn unsat_cpns_rec(entries: &[DepEntry], avail: &Avail, out: &mut Vec<Cpn>) {
    for e in entries {
        match e {
            DepEntry::Atom(dep) if dep.blocker.is_none() && !avail.atom_satisfied(dep) => {
                out.push(dep.cpn);
            }
            DepEntry::AllOf(c) => unsat_cpns_rec(c, avail, out),
            DepEntry::AnyOf(c) if !group_satisfied(c, avail) => {
                for branch in c {
                    cpns_of(branch, out);
                }
            }
            _ => {}
        }
    }
}

/// Collect every non-blocker atom CPN mentioned in `e` (for an unsatisfied
/// `||`-group's branches).
fn cpns_of(e: &DepEntry, out: &mut Vec<Cpn>) {
    match e {
        DepEntry::Atom(dep) if dep.blocker.is_none() => out.push(dep.cpn),
        DepEntry::AllOf(c) | DepEntry::AnyOf(c) => c.iter().for_each(|b| cpns_of(b, out)),
        _ => {}
    }
}

/// Append the display form of each unsatisfied requirement in `entries` to
/// `out`. `UseConditional`s are assumed already resolved by `evaluate_use`.
pub fn collect_unsatisfied(entries: &[DepEntry], avail: &Avail, out: &mut Vec<String>) {
    for e in entries {
        match e {
            DepEntry::Atom(dep) if dep.blocker.is_some() => {}
            DepEntry::Atom(dep) => {
                if !avail.atom_satisfied(dep) {
                    out.push(dep.to_string());
                }
            }
            DepEntry::AllOf(children) => collect_unsatisfied(children, avail, out),
            any @ DepEntry::AnyOf(children) => {
                if !group_satisfied(children, avail) {
                    out.push(any.to_string());
                }
            }
            DepEntry::ExactlyOneOf(_) | DepEntry::AtMostOneOf(_) => {}
            DepEntry::UseConditional { .. } => {}
        }
    }
}

fn group_satisfied(entries: &[DepEntry], avail: &Avail) -> bool {
    entries.iter().any(|e| entry_satisfied(e, avail))
}

/// Whether `e` is satisfied on `avail` (blockers count as satisfied).
pub(crate) fn entry_satisfied(e: &DepEntry, avail: &Avail) -> bool {
    match e {
        DepEntry::Atom(dep) => dep.blocker.is_some() || avail.atom_satisfied(dep),
        DepEntry::AllOf(c) => c.iter().all(|e| entry_satisfied(e, avail)),
        DepEntry::AnyOf(c) => group_satisfied(c, avail),
        DepEntry::ExactlyOneOf(_) | DepEntry::AtMostOneOf(_) => true,
        DepEntry::UseConditional { .. } => true,
    }
}

fn entry_unsatisfied_for_cpn(e: &DepEntry, cpn: Cpn, avail: &Avail) -> bool {
    match e {
        DepEntry::Atom(dep) if dep.blocker.is_some() => false,
        DepEntry::Atom(dep) if dep.cpn != cpn => false,
        DepEntry::Atom(dep) => !avail.atom_satisfied(dep),
        DepEntry::AllOf(c) => c.iter().any(|e| entry_unsatisfied_for_cpn(e, cpn, avail)),
        DepEntry::AnyOf(c) => {
            // Unsatisfied || group only if every branch that mentions `cpn` fails
            // and at least one branch mentions `cpn`.
            let mut mentions = false;
            let mut any_sat = false;
            for child in c {
                if branch_mentions_cpn(child, cpn) {
                    mentions = true;
                    if entry_satisfied(child, avail) {
                        any_sat = true;
                    }
                }
            }
            mentions && !any_sat
        }
        DepEntry::ExactlyOneOf(_) | DepEntry::AtMostOneOf(_) => false,
        DepEntry::UseConditional { .. } => false,
    }
}

fn branch_mentions_cpn(e: &DepEntry, cpn: Cpn) -> bool {
    match e {
        DepEntry::Atom(dep) => dep.cpn == cpn,
        DepEntry::AllOf(c) | DepEntry::AnyOf(c) => c.iter().any(|e| branch_mentions_cpn(e, cpn)),
        DepEntry::ExactlyOneOf(c) | DepEntry::AtMostOneOf(c) => {
            c.iter().any(|e| branch_mentions_cpn(e, cpn))
        }
        DepEntry::UseConditional { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atoms(specs: &[&str]) -> Avail {
        Avail(
            specs
                .iter()
                .map(|s| (Cpv::parse(s).unwrap(), None))
                .collect(),
        )
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
    fn has_unsatisfied_atom_for_cpn_detects_gap() {
        let avail = atoms(&["dev-build/b-1.0"]);
        let bdepend = parse(">=dev-build/b-2.0");
        assert!(avail.has_unsatisfied_atom_for_cpn(&bdepend, Cpn::parse("dev-build/b").unwrap()));
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
    fn blockers_are_ignored() {
        let avail = Avail::default();
        let mut out = Vec::new();
        collect_unsatisfied(&parse("!dev-libs/foo"), &avail, &mut out);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn unsatisfied_cpns_returns_missing() {
        let avail = atoms(&["dev-libs/foo-1.2"]);
        let cpns: Vec<String> = unsatisfied_cpns(&parse("dev-libs/foo dev-libs/bar"), &avail)
            .into_iter()
            .map(|c| c.to_string())
            .collect();
        assert_eq!(cpns, ["dev-libs/bar"]);
    }

    #[test]
    fn unsatisfied_cpns_any_of_unsatisfied_lists_branches() {
        // A fully-unsatisfied || group contributes every branch's CPN.
        let avail = Avail::default();
        let cpns: Vec<String> = unsatisfied_cpns(&parse("|| ( dev-libs/a dev-libs/b )"), &avail)
            .into_iter()
            .map(|c| c.to_string())
            .collect();
        assert_eq!(cpns, ["dev-libs/a", "dev-libs/b"]);
    }

    #[test]
    fn unsatisfied_cpns_any_of_satisfied_is_empty() {
        let avail = atoms(&["dev-libs/b-1"]);
        let cpns = unsatisfied_cpns(&parse("|| ( dev-libs/a dev-libs/b )"), &avail);
        assert!(cpns.is_empty());
    }
}
