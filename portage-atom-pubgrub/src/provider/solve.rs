//! The PubGrub `DependencyProvider` implementation: version prioritisation,
//! version choice (installed-preference heuristics), and dependency lookup.

use std::cmp::Reverse;
use std::collections::HashMap;

use portage_atom::Version;
use pubgrub::{
    Dependencies, DependencyConstraints, DependencyProvider, PackageResolutionStatistics,
};

use crate::error::Error;
use crate::package::{MergeRoot, PortagePackage};
use crate::version_set::PortageVersionSet;

use super::{InstalledPolicy, PortageDependencyProvider, VersionData};

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
            .package_data(package)
            .map(|d| d.versions.keys().filter(|v| range.contains(v)).count())
            .unwrap_or(0);
        (stats.conflict_count(), Reverse(count))
    }

    fn choose_version(
        &self,
        package: &Self::P,
        range: &Self::VS,
    ) -> std::result::Result<Option<Self::V>, Self::Err> {
        let Some(data) = self.package_data(package) else {
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
                    // Explicit targets are not favored: a named argument pulls
                    // the best accepted version (emerge argument semantics).
                    // Nor is a package whose installed version is no longer in
                    // the repo: there is nothing to keep, so fall through to the
                    // newest candidate (portage updates an installed package
                    // whose version was pruned from the tree).
                    if !self.root_targets.contains(package)
                        && !self.installed_missing_from_repo.contains(package)
                        && range.contains(installed_ver)
                    {
                        return Ok(Some(installed_ver.clone()));
                    }
                }
                InstalledPolicy::Rebuild => {}
            }
        }

        // `--deep` / native emptytree: for a `:*` any-slot dep (`SlotChoice`),
        // bump to the newest slot instead of keeping a satisfying installed slot
        // — matching `emerge -uD`/`-e` (e.g. firefox pulling the newest
        // `dev-lang/rust-bin` slot). Slots are version-ranked, so the `max()`
        // pick below is the newest-*version* slot (never an older compat slot
        // like `app-shells/bash:5.1`). Scoped to `SlotChoice` only: `Choice`
        // (provider OR-groups) keeps the installed-branch / USE-dep preference so
        // we don't gratuitously re-pick providers (e.g. rust-bin vs source rust).
        let bump_slot =
            self.prefer_newest_slot && matches!(package, PortagePackage::SlotChoice { .. });

        // For OR-group / slot-choice packages, prefer branches that lead to
        // an already-installed package.  Independent of `rebuild_tree`: emptytree
        // rebuilds every listed package but `gcc:*` must still bind to the
        // installed/newest slot.  SlotChoice nodes number slots i+1 (newest slot
        // last/highest); Choice nodes use n-i (first-listed highest).
        if !bump_slot && package.is_virtual() && !self.installed_cpns.is_empty() {
            // Check each candidate directly against self.installed.
            // deps_reach_installed only checks CPNs, which produces false positives
            // for multi-slot packages (every slot appears "installed" if any slot
            // is), causing the heuristic to never fire and the solver to fall
            // back to the default max() pick.
            let direct_installed: Vec<bool> = candidates
                .iter()
                .map(|&ver| {
                    data.versions.get(ver).is_some_and(|vd| {
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
        let Some(data) = self.package_data(package) else {
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
        // `--emptytree` (`InstalledPolicy::Rebuild`) always expands the full
        // build-time closure even when the selected version matches the VDB.
        if self.installed.get(package).is_some_and(|(inst, policy)| {
            inst == version && !matches!(policy, InstalledPolicy::Rebuild)
        }) {
            if self.cross_active && package.merge_root() == MergeRoot::Target {
                return Ok(Dependencies::Available(cross_target_runtime_deps(
                    vd,
                    &self.host_installed,
                    &self.sysroot_installed,
                )));
            }
            let runtime: DependencyConstraints<PortagePackage, PortageVersionSet> = vd.by_class[1]
                .iter() // RDEPEND
                .chain(vd.by_class[3].iter()) // PDEPEND
                .chain(vd.by_class[4].iter()) // IDEPEND
                .map(|(p, vs, _)| (p.clone(), vs.clone()))
                .collect();
            return Ok(Dependencies::Available(runtime));
        }

        // Native `--emptytree` (`rebuild_tree`): list the **full deep closure** with
        // real edges. Do NOT broot-prune host-satisfied build deps — under emptytree
        // the host (BROOT) seed is for bootstrap version choice, ordering/cycle-break
        // and action tags, never for membership. `emptytree_native` is `!cross.active`,
        // so this precedes the cross paths. See todo/em-emptytree.md "AGREED REDESIGN".
        if self.rebuild_tree {
            return Ok(vd.merged.clone());
        }

        // A package being *built* (not at its installed version):
        if self.cross_active && package.merge_root() == MergeRoot::Target {
            // Cross `-p` never expands BDEPEND onto ROOT (emerge lists the same
            // closure with or without `--with-bdeps=y`). Host-satisfied build
            // tools stay on BROOT; unsatisfied BDEPEND schedule via Host-root
            // nodes when `with_bdeps` is on (see `host_native_deps` below).
            return Ok(Dependencies::Available(cross_target_runtime_deps(
                vd,
                &self.host_installed,
                &self.sysroot_installed,
            )));
        }
        if self.cross_active && package.merge_root() == MergeRoot::Host && self.with_bdeps {
            return Ok(Dependencies::Available(host_native_deps(
                vd,
                &self.host_installed,
            )));
        }

        if self.with_bdeps {
            // BDEPEND and IDEPEND run on BROOT; drop each edge already satisfied on
            // the host. This keeps an offset build (`--root <empty>`) from pulling
            // host-provided build/install tools into the plan, matching portage.
            // DEPEND/RDEPEND are unaffected.
            if !self.host_installed.is_empty() {
                return Ok(Dependencies::Available(broot_filtered(
                    vd,
                    &self.host_installed,
                )));
            }
            Ok(vd.merged.clone())
        } else {
            // BDEPEND excluded entirely (emerge --with-bdeps=n default).
            // DEPEND/RDEPEND/PDEPEND plus host-satisfied IDEPEND filtering.
            Ok(Dependencies::Available(native_runtime_deps(
                vd,
                &self.host_installed,
            )))
        }
    }
}

/// Rebuild a version's merged constraints with host-satisfied BDEPEND edges
/// dropped. `host_installed` maps a package to a present-on-BROOT version; a
/// BDEPEND edge `(pkg, vset)` is dropped when `pkg` is present and `vset`
/// accepts that version. Per-edge (not per-package): a package that is both a
/// BDEPEND of A and an RDEPEND of B is still built when B needs it.
fn stamp_root(p: &PortagePackage, root: MergeRoot) -> PortagePackage {
    p.at_merge_root(root)
}

fn cross_target_runtime_deps(
    vd: &VersionData,
    host_installed: &HashMap<PortagePackage, Version>,
    _sysroot_installed: &HashMap<PortagePackage, Version>,
) -> DependencyConstraints<PortagePackage, PortageVersionSet> {
    let mut out: Vec<(PortagePackage, PortageVersionSet)> = vd.by_class[0]
        .iter()
        .chain(vd.by_class[1].iter())
        .chain(vd.by_class[3].iter())
        .map(|(p, vs, _)| (stamp_root(p, MergeRoot::Target), vs.clone()))
        .collect();
    append_unsatisfied_broot(&mut out, &vd.by_class[4], host_installed, MergeRoot::Host);
    out.into_iter().collect()
}

/// Host-root native build (BDEPEND front-matter): all deps target the host instance.
fn host_native_deps(
    vd: &VersionData,
    host_installed: &HashMap<PortagePackage, Version>,
) -> DependencyConstraints<PortagePackage, PortageVersionSet> {
    let mut out: Vec<(PortagePackage, PortageVersionSet)> = vd.by_class[0]
        .iter()
        .chain(vd.by_class[1].iter())
        .chain(vd.by_class[3].iter())
        .map(|(p, vs, _)| (stamp_root(p, MergeRoot::Host), vs.clone()))
        .collect();
    append_unsatisfied_broot(&mut out, &vd.by_class[2], host_installed, MergeRoot::Host);
    append_unsatisfied_broot(&mut out, &vd.by_class[4], host_installed, MergeRoot::Host);
    out.into_iter().collect()
}

/// Native build with `--with-bdeps=n`: DEPEND/RDEPEND/PDEPEND plus host-filtered IDEPEND.
fn native_runtime_deps(
    vd: &VersionData,
    host_installed: &HashMap<PortagePackage, Version>,
) -> DependencyConstraints<PortagePackage, PortageVersionSet> {
    let mut out: Vec<(PortagePackage, PortageVersionSet)> = vd.by_class[0]
        .iter()
        .chain(vd.by_class[1].iter())
        .chain(vd.by_class[3].iter())
        .map(|(p, vs, _)| (p.clone(), vs.clone()))
        .collect();
    append_unsatisfied_broot(&mut out, &vd.by_class[4], host_installed, MergeRoot::Target);
    out.into_iter().collect()
}

/// Native build with `--with-bdeps`: keep DEPEND/RDEPEND/PDEPEND; drop host-satisfied
/// BDEPEND and IDEPEND (both resolve on BROOT per PMS table 8.2).
fn broot_filtered(
    vd: &VersionData,
    host_installed: &HashMap<PortagePackage, Version>,
) -> DependencyConstraints<PortagePackage, PortageVersionSet> {
    let mut out: Vec<(PortagePackage, PortageVersionSet)> = vd.by_class[0]
        .iter()
        .chain(vd.by_class[1].iter())
        .chain(vd.by_class[3].iter())
        .map(|(p, vs, _)| (p.clone(), vs.clone()))
        .collect();
    append_unsatisfied_broot(&mut out, &vd.by_class[2], host_installed, MergeRoot::Target);
    append_unsatisfied_broot(&mut out, &vd.by_class[4], host_installed, MergeRoot::Target);
    out.into_iter().collect()
}

fn host_satisfied_on_broot(
    host_installed: &HashMap<PortagePackage, Version>,
    p: &PortagePackage,
    vs: &PortageVersionSet,
) -> bool {
    let hp = stamp_root(p, MergeRoot::Host);
    host_installed.get(&hp).is_some_and(|hv| vs.contains(hv))
}

fn append_unsatisfied_broot(
    out: &mut Vec<(PortagePackage, PortageVersionSet)>,
    edges: &[crate::convert::Req],
    host_installed: &HashMap<PortagePackage, Version>,
    unsatisfied_root: MergeRoot,
) {
    for (p, vs, _) in edges {
        if !host_satisfied_on_broot(host_installed, p, vs) {
            out.push((stamp_root(p, unsatisfied_root), vs.clone()));
        }
    }
}
