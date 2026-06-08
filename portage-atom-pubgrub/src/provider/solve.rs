//! The PubGrub `DependencyProvider` implementation: version prioritisation,
//! version choice (installed-preference heuristics), and dependency lookup.

use std::cmp::Reverse;

use portage_atom::Version;
use pubgrub::{
    Dependencies, DependencyConstraints, DependencyProvider, PackageResolutionStatistics,
};

use crate::error::Error;
use crate::package::PortagePackage;
use crate::version_set::PortageVersionSet;

use super::{InstalledPolicy, PortageDependencyProvider};

impl DependencyProvider for PortageDependencyProvider {
    type P = PortagePackage;
    type V = Version;
    type VS = PortageVersionSet;
    type M = String;
    type Err = Error;
    type Priority = (u32, Reverse<usize>);

    fn prioritize(
        &self,
        package: &Self::P,
        range: &Self::VS,
        stats: &PackageResolutionStatistics,
    ) -> Self::Priority {
        let count = self
            .packages
            .get(package)
            .map(|d| d.versions.keys().filter(|v| range.contains(v)).count())
            .unwrap_or(0);
        (stats.conflict_count(), Reverse(count))
    }

    fn choose_version(
        &self,
        package: &Self::P,
        range: &Self::VS,
    ) -> std::result::Result<Option<Self::V>, Self::Err> {
        let Some(data) = self.packages.get(package) else {
            return Ok(None);
        };

        let candidates: Vec<&Version> =
            data.versions.keys().filter(|v| range.contains(v)).collect();

        if candidates.is_empty() {
            return Ok(None);
        }

        // A prior solve iteration decided to upgrade this installed package to a
        // newer version (`upgrade_to`).  Pin it so the solver actually selects
        // that version — and therefore re-solves its dependency closure — rather
        // than favouring the installed version again.  If the pinned version is
        // out of range for this particular constraint, fall through to the
        // normal logic.
        if let Some(pin) = self.upgrade_pins.get(package)
            && range.contains(pin)
        {
            return Ok(Some(pin.clone()));
        }

        // Ceded USE flags: bias a `UseDecision` node toward the caller's
        // preferred value, so a `SolverDecided` flag keeps its configured value
        // unless a `REQUIRED_USE` constraint narrows `range` away from it. When
        // the preference is out of range the constraint has forced a flip; fall
        // through to the normal pick (the other version).
        if matches!(package, PortagePackage::UseDecision { .. })
            && let Some(pref) = self.use_decision_prefer.get(package)
            && range.contains(pref)
        {
            return Ok(Some(pref.clone()));
        }

        if let Some((installed_ver, policy)) = self.installed.get(package) {
            match policy {
                InstalledPolicy::Lock => {
                    if range.contains(installed_ver) {
                        return Ok(Some(installed_ver.clone()));
                    }
                    return Ok(None);
                }
                InstalledPolicy::Favor => {
                    if range.contains(installed_ver) {
                        return Ok(Some(installed_ver.clone()));
                    }
                }
            }
        }

        // For OR-group / slot-choice packages, prefer branches that lead to
        // an already-installed package.
        if package.is_virtual() && !self.installed_cpns.is_empty() {
            // Check each candidate directly against self.installed.
            // deps_reach_installed only checks CPNs, which produces false positives
            // for multi-slot packages (every slot appears "installed" if any slot
            // is), causing the heuristic to never fire and the solver to fall
            // back to picking the highest synthetic version (= oldest slot).
            let direct_installed: Vec<bool> = candidates
                .iter()
                .map(|&ver| {
                    data.versions
                        .get(ver)
                        .is_some_and(|vd| {
                            if let Dependencies::Available(ref cs) = vd.merged {
                                cs.iter().any(|(pkg, _)| self.installed.contains_key(pkg))
                            } else {
                                false
                            }
                        })
                })
                .collect();
            let directly_installed_count = direct_installed.iter().filter(|&&x| x).count();

            if directly_installed_count > 0 {
                if matches!(package, PortagePackage::SlotChoice { .. }) {
                    // Slot choices use n-i version numbering: first (oldest) slot gets
                    // the highest synthetic version.  Use min() to pick the newest
                    // installed slot regardless of how many are installed.
                    let best = candidates
                        .iter()
                        .copied()
                        .zip(direct_installed.iter().copied())
                        .filter(|(_, has)| *has)
                        .map(|(v, _)| v)
                        .min()
                        .cloned();
                    return Ok(best);
                }

                // For OR-group Choice packages: among the installed branches, prefer
                // those that already satisfy all USE dep constraints.  A branch that
                // is installed AND use-satisfied avoids unnecessary package rebuilds.
                //
                // Example: librsvg BDEPEND has || ( (python:3.14 docutils[p3.14(-)]) ... )
                // Both python:3.14 and python:3.13 are installed, so both branches pass
                // the simple installed-preference check and we'd fall through to max()
                // (= first listed, python:3.14).  But if docutils only has p3.13 enabled,
                // the p3.14 branch's USE dep is unsatisfied — we should pick p3.13 instead.
                if matches!(package, PortagePackage::Choice { .. }) {
                    let installed_and_use_sat: Vec<bool> = candidates
                        .iter()
                        .zip(direct_installed.iter())
                        .map(|(&ver, &inst)| {
                            inst && data
                                .versions
                                .get(ver)
                                .map(|vd| self.use_dep_branch_satisfied(&vd.use_deps))
                                .unwrap_or(true)
                        })
                        .collect();
                    let sat_count = installed_and_use_sat.iter().filter(|&&s| s).count();
                    // Only intervene when some (not all) installed branches satisfy USE
                    // deps — if none satisfy, we can't do better so fall through.
                    if sat_count > 0 && sat_count < candidates.len() {
                        let best = candidates
                            .iter()
                            .copied()
                            .zip(installed_and_use_sat.iter().copied())
                            .filter(|(_, s)| *s)
                            .map(|(v, _)| v)
                            .max()
                            .cloned();
                        return Ok(best);
                    }
                }

                if directly_installed_count < candidates.len() {
                    // OR group with some (not all) branches installed: prefer
                    // the highest-version installed branch (= first listed).
                    let best = candidates
                        .into_iter()
                        .zip(direct_installed)
                        .filter(|(_, has)| *has)
                        .map(|(v, _)| v)
                        .max()
                        .cloned();
                    return Ok(best);
                }
                // All branches installed: fall through to default max() pick
                // (= first listed alternative, stable behaviour).
            }

            // No directly-installed branch found; fall back to CPN-level
            // heuristic for nested OR groups with non-direct install paths.
            let has_installed: Vec<bool> = candidates
                .iter()
                .map(|&ver| {
                    data.versions
                        .get(ver)
                        .is_some_and(|vd| self.deps_reach_installed(vd, 2))
                })
                .collect();
            let installed_count = has_installed.iter().filter(|&&x| x).count();
            if installed_count > 0 && installed_count < candidates.len() {
                let best = candidates
                    .into_iter()
                    .zip(has_installed)
                    .filter(|(_, has)| *has)
                    .map(|(v, _)| v)
                    .max()
                    .cloned();
                return Ok(best);
            }
        }

        let version = candidates.into_iter().max().cloned();
        Ok(version)
    }

    fn get_dependencies(
        &self,
        package: &Self::P,
        version: &Self::V,
    ) -> std::result::Result<Dependencies<Self::P, Self::VS, Self::M>, Self::Err> {
        let Some(data) = self.packages.get(package) else {
            return Ok(Dependencies::Unavailable(format!(
                "package not found: {}",
                package
            )));
        };
        let Some(vd) = data.versions.get(version) else {
            return Ok(Dependencies::Unavailable(format!(
                "version not found: {}@{}",
                package, version
            )));
        };

        // For installed packages at their installed version, skip build-time
        // deps (DEPEND = index 0, BDEPEND = index 2).  The package is already
        // built; re-solving its build deps would drag in bootstrap toolchain
        // packages (old gcc to build new gcc, etc.) that portage never shows.
        // Only RDEPEND (1), PDEPEND (3), and IDEPEND (4) matter at install time.
        if self.installed.get(package).is_some_and(|(inst, _)| inst == version) {
            let runtime: DependencyConstraints<PortagePackage, PortageVersionSet> =
                vd.by_class[1].iter()  // RDEPEND
                    .chain(vd.by_class[3].iter())  // PDEPEND
                    .chain(vd.by_class[4].iter())  // IDEPEND
                    .map(|(p, vs, _)| (p.clone(), vs.clone()))
                    .collect();
            return Ok(Dependencies::Available(runtime));
        }

        Ok(vd.merged.clone())
    }
}
