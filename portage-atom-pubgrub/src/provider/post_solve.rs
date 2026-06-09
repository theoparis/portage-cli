//! Post-solve USE analysis: from a solved graph, compute the USE flag changes
//! the solution implies (the "needed" set) — autounmask suggestions for new
//! packages and rebuild requirements for installed ones.

use std::collections::HashMap;

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{UseDefault, UseDepKind, Version};
use pubgrub::SelectedDependencies;

use crate::convert;
use crate::package::PortagePackage;
use crate::use_config::UseFlagState;

use super::{PortageDependencyProvider, UseFlagRequirement};

fn eval_violated_use_dep(
    kind: UseDepKind,
    dep_effective_enabled: bool,
    parent_flag_enabled: bool,
) -> Option<bool> {
    match kind {
        UseDepKind::Enabled => (!dep_effective_enabled).then_some(true),
        UseDepKind::Disabled => dep_effective_enabled.then_some(false),
        // [flag?]: if parent has flag → dep must have flag
        UseDepKind::Conditional => (parent_flag_enabled && !dep_effective_enabled).then_some(true),
        // [!flag?]: if parent lacks flag → dep must have flag
        UseDepKind::ConditionalInverse => {
            (!parent_flag_enabled && !dep_effective_enabled).then_some(true)
        }
        // [flag=]: dep must match parent
        UseDepKind::Equal => {
            (dep_effective_enabled != parent_flag_enabled).then_some(parent_flag_enabled)
        }
        // [!flag=]: dep must be opposite of parent
        UseDepKind::EqualInverse => {
            let required = !parent_flag_enabled;
            (dep_effective_enabled == parent_flag_enabled).then_some(required)
        }
    }
}

impl PortageDependencyProvider {
    /// Walk the full PubGrub solution (including virtual choice packages) and
    /// collect USE flag requirements for every package that has at least one
    /// violated or unsatisfied USE dep constraint.
    ///
    /// **Installed packages** are compared against their VDB-recorded active USE
    /// flags; only violated constraints are collected (the flag needs to change).
    ///
    /// **Non-installed packages** (being freshly built) are compared against the
    /// global `use_config`; requirements where the flag might not be set by the
    /// current configuration are collected as informational annotations.
    ///
    /// The full solution (with virtual nodes) is required so that per-branch
    /// USE dep constraints from OR-group choices are also checked.
    pub(crate) fn compute_use_flag_requirements(
        &self,
        solution: &SelectedDependencies<PortagePackage, Version>,
    ) -> Vec<UseFlagRequirement> {
        // Accumulate per target: (version, enable_set, disable_set, requirers).
        let mut by_target: HashMap<
            PortagePackage,
            (
                Version,
                std::collections::BTreeSet<Interned<DefaultInterner>>,
                std::collections::BTreeSet<Interned<DefaultInterner>>,
                std::collections::BTreeSet<String>,
            ),
        > = HashMap::new();
        // Installed packages that should be upgraded to a newer repo version
        // rather than rebuilt at the installed version.  Keyed by the installed
        // package; value is the newer version to build instead.
        let mut upgrade_to: HashMap<PortagePackage, Version> = HashMap::new();

        // Iterate to fixpoint:
        // 1. Conditional deps cascade — when package A needs flag X, A's own
        //    `B[X(-)?]` deps fire, requiring B to have X as well.
        // 2. When an installed package gains a violation, check if a newer repo
        //    version exists whose constraints should also be expanded (upgrade
        //    the package rather than rebuilding the old version).
        loop {
            let prev_total: usize = by_target
                .values()
                .map(|(_, e, d, _)| e.len() + d.len())
                .sum();
            let prev_upgrades = upgrade_to.len();

            // -- main solution packages --
            for (pkg, ver) in solution.iter() {
                let Some(vd) = self.packages.get(pkg).and_then(|d| d.versions.get(ver)) else {
                    continue;
                };
                let udeps = &vd.use_deps;

                for constraint in udeps {
                    let (target_pkg, vs) = &constraint.target;
                    if target_pkg.is_virtual() {
                        continue;
                    }

                    // Resolve target version and whether it is installed.
                    let (target_ver, is_installed) =
                        if let Some((inst_ver, _)) = self.installed.get(target_pkg) {
                            if vs.contains(inst_ver) {
                                (inst_ver, true)
                            } else {
                                continue;
                            }
                        } else if let Some(sol_ver) = solution.get(target_pkg) {
                            if vs.contains(sol_ver) {
                                (sol_ver, false)
                            } else {
                                continue;
                            }
                        } else {
                            continue;
                        };

                    for ud in &constraint.use_deps {
                        // Parent's flag state: currently active OR will be enabled
                        // after this build run (already in by_target.required_enabled).
                        let parent_flag_enabled = if self.installed.contains_key(pkg) {
                            self.installed_use
                                .get(pkg)
                                .map_or(false, |u| u.contains(&ud.flag))
                                || by_target
                                    .get(pkg)
                                    .map_or(false, |(_, e, _, _)| e.contains(&ud.flag))
                        } else {
                            self.effective_flag_new(pkg, ver, &ud.flag, None)
                        };

                        let dep_effective_enabled = if is_installed {
                            let active = self
                                .installed_use
                                .get(target_pkg)
                                .map(Vec::as_slice)
                                .unwrap_or(&[]);
                            let iuse = self
                                .packages
                                .get(target_pkg)
                                .and_then(|d| d.versions.get(target_ver))
                                .map(|vd| vd.iuse.as_slice())
                                // Empty == absent: a synthetic installed entry (or a
                                // repo version with no IUSE) must fall back to the
                                // VDB-recorded IUSE, matching pre-refactor behaviour.
                                .filter(|s| !s.is_empty())
                                .or_else(|| self.installed_iuse.get(target_pkg).map(Vec::as_slice))
                                .unwrap_or(&[]);
                            let in_iuse = iuse.contains(&ud.flag);
                            if in_iuse {
                                active.contains(&ud.flag)
                            } else {
                                matches!(ud.default, Some(UseDefault::Enabled))
                            }
                        } else {
                            self.effective_flag_new(target_pkg, target_ver, &ud.flag, ud.default)
                        };

                        if let Some(requires_enabled) = eval_violated_use_dep(
                            ud.kind,
                            dep_effective_enabled,
                            parent_flag_enabled,
                        ) {
                            let entry = by_target.entry(target_pkg.clone()).or_insert_with(|| {
                                (
                                    target_ver.clone(),
                                    std::collections::BTreeSet::new(),
                                    std::collections::BTreeSet::new(),
                                    std::collections::BTreeSet::new(),
                                )
                            });
                            if requires_enabled {
                                entry.1.insert(ud.flag);
                            } else {
                                entry.2.insert(ud.flag);
                            }
                            if !pkg.is_virtual() {
                                entry.3.insert(constraint.parent_cpn_str.clone());
                            }
                        }
                    }
                }
            }

            // -- upgrade expansion --
            // For each installed package with violations, check whether a newer
            // repo version exists.  If so, record the upgrade and process the
            // newer version's USE dep constraints in the next fixpoint iteration.
            let installed_with_violations: Vec<(PortagePackage, Version)> = by_target
                .iter()
                .filter(|(pkg, _)| self.installed.contains_key(pkg))
                .filter(|(pkg, _)| !upgrade_to.contains_key(*pkg))
                .filter_map(|(pkg, (inst_ver, _, _, _))| {
                    self.packages
                        .get(pkg)
                        .and_then(|d| d.versions.keys().filter(|v| v > &inst_ver).max())
                        .map(|new_ver| (pkg.clone(), new_ver.clone()))
                })
                .collect();

            for (pkg, new_ver) in installed_with_violations {
                upgrade_to.insert(pkg.clone(), new_ver.clone());

                // Expand the newer version's USE dep constraints.
                let Some(vd) = self
                    .packages
                    .get(&pkg)
                    .and_then(|d| d.versions.get(&new_ver))
                else {
                    continue;
                };
                let udeps = &vd.use_deps;

                // The "parent" is the upgraded package itself.
                let parent_is_installed = self.installed.contains_key(&pkg);
                for constraint in udeps {
                    let (target_pkg, vs) = &constraint.target;
                    if target_pkg.is_virtual() {
                        continue;
                    }
                    let (target_ver, is_installed) =
                        if let Some((inst_ver, _)) = self.installed.get(target_pkg) {
                            if vs.contains(inst_ver) {
                                (inst_ver, true)
                            } else {
                                continue;
                            }
                        } else if let Some(sol_ver) = solution.get(target_pkg) {
                            if vs.contains(sol_ver) {
                                (sol_ver, false)
                            } else {
                                continue;
                            }
                        } else {
                            continue;
                        };

                    for ud in &constraint.use_deps {
                        let parent_flag_enabled = if parent_is_installed {
                            self.installed_use
                                .get(&pkg)
                                .map_or(false, |u| u.contains(&ud.flag))
                                || by_target
                                    .get(&pkg)
                                    .map_or(false, |(_, e, _, _)| e.contains(&ud.flag))
                        } else {
                            self.effective_flag_new(&pkg, &new_ver, &ud.flag, None)
                        };

                        let dep_effective_enabled = if is_installed {
                            let active = self
                                .installed_use
                                .get(target_pkg)
                                .map(Vec::as_slice)
                                .unwrap_or(&[]);
                            let iuse = self
                                .packages
                                .get(target_pkg)
                                .and_then(|d| d.versions.get(target_ver))
                                .map(|vd| vd.iuse.as_slice())
                                .or_else(|| self.installed_iuse.get(target_pkg).map(Vec::as_slice))
                                .unwrap_or(&[]);
                            let in_iuse = iuse.contains(&ud.flag);
                            if in_iuse {
                                active.contains(&ud.flag)
                            } else {
                                matches!(ud.default, Some(UseDefault::Enabled))
                            }
                        } else {
                            self.effective_flag_new(target_pkg, target_ver, &ud.flag, ud.default)
                        };

                        if let Some(req_en) = eval_violated_use_dep(
                            ud.kind,
                            dep_effective_enabled,
                            parent_flag_enabled,
                        ) {
                            let entry = by_target.entry(target_pkg.clone()).or_insert_with(|| {
                                (
                                    target_ver.clone(),
                                    std::collections::BTreeSet::new(),
                                    std::collections::BTreeSet::new(),
                                    std::collections::BTreeSet::new(),
                                )
                            });
                            if req_en {
                                entry.1.insert(ud.flag);
                            } else {
                                entry.2.insert(ud.flag);
                            }
                            entry.3.insert(constraint.parent_cpn_str.clone());
                        }
                    }
                }
            }

            let new_total: usize = by_target
                .values()
                .map(|(_, e, d, _)| e.len() + d.len())
                .sum();
            if new_total == prev_total && upgrade_to.len() == prev_upgrades {
                break;
            }
        }

        let mut reqs: Vec<UseFlagRequirement> = by_target
            .into_iter()
            .map(
                |(pkg, (ver, enable, disable, requirers))| UseFlagRequirement {
                    package: pkg.clone(),
                    version: ver,
                    upgrade_to: upgrade_to.remove(&pkg),
                    required_enabled: enable.into_iter().collect(),
                    required_disabled: disable.into_iter().collect(),
                    required_by: requirers.into_iter().collect(),
                },
            )
            .collect();
        // `by_target` is a HashMap, so collect order is nondeterministic; sort by
        // (package, version) so use_flag_requirements — and everything derived
        // from it (reinstall_deps → the appended merge-order tail, and the
        // autounmask report order) — is reproducible across runs.
        reqs.sort_by(|a, b| {
            a.package
                .cmp(&b.package)
                .then_with(|| a.version.cmp(&b.version))
        });
        reqs
    }

    /// Return all USE flag requirements collected by the post-solve validation pass.
    ///
    /// Includes both reinstall candidates (`R`) and informational annotations
    /// for newly-installed packages.  Populated by [`resolve_targets`].
    pub fn use_flag_requirements(&self) -> &[UseFlagRequirement] {
        &self.use_flag_requirements
    }

    /// Return only the requirements that correspond to reinstalls: installed
    /// packages whose current USE flags violate at least one constraint from the
    /// resolved set.
    pub fn reinstall_deps(&self) -> Vec<&UseFlagRequirement> {
        self.use_flag_requirements
            .iter()
            .filter(|r| self.installed.contains_key(&r.package))
            .collect()
    }

    /// Check whether all USE dep constraints for an OR-group branch are
    /// consistent with the desired final state of the installed packages.
    ///
    /// A flag is treated as "effectively enabled" when it is either:
    /// - currently active in the installed VDB, OR
    /// - in the package's IUSE and enabled in the global `use_config`
    ///   (i.e. the profile / make.conf wants it enabled after the next build).
    ///
    /// This means branch selection picks branches that are consistent with the
    /// *desired* state, not just the *current* installed state.  Branches that
    /// conflict with the global config are de-prioritised, allowing the
    /// post-solve violation pass to then flag the specific flags that need to
    /// change.
    ///
    /// Returns `true` when every constraint is either satisfied (under the
    /// above definition) or refers to a package not yet installed.
    /// Effective state of `flag` on a non-installed package version that will be
    /// freshly built.  Mirrors what the build will actually see: `package.use`
    /// and global USE applied on top of the ebuild's IUSE defaults.  For a flag
    /// outside the package's IUSE, only the dep's own `(+)`/`(-)` default applies.
    pub(crate) fn effective_flag_new(
        &self,
        pkg: &PortagePackage,
        ver: &Version,
        flag: &Interned<DefaultInterner>,
        dep_default: Option<UseDefault>,
    ) -> bool {
        let vd = self.packages.get(pkg).and_then(|d| d.versions.get(ver));
        let in_iuse = vd.is_some_and(|v| v.iuse.contains(flag));
        if !in_iuse {
            return matches!(dep_default, Some(UseDefault::Enabled));
        }
        // `desired` already folds package.use, global USE, and IUSE defaults, so
        // a single lookup gives the flag's effective state for this build.
        vd.is_some_and(|v| v.desired.get(flag) == UseFlagState::Enabled)
    }

    pub(crate) fn use_dep_branch_satisfied(&self, udeps: &[convert::UseDepConstraint]) -> bool {
        for constraint in udeps {
            let (target_pkg, vs) = &constraint.target;
            let Some((inst_ver, _)) = self.installed.get(target_pkg) else {
                continue; // not installed → can't verify, don't veto
            };
            if !vs.contains(inst_ver) {
                continue;
            }
            let active = self
                .installed_use
                .get(target_pkg)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            for ud in &constraint.use_deps {
                // Desired final state of the target's flag: active now, or the
                // version's desired set will enable it on rebuild.
                let dep_effective_enabled = active.contains(&ud.flag)
                    || self.effective_flag_new(target_pkg, inst_ver, &ud.flag, ud.default);
                // Parent flag (only used by Conditional/Equal kinds, rare in OR
                // groups): approximate with the target's desired state.
                let parent_flag_enabled =
                    self.effective_flag_new(target_pkg, inst_ver, &ud.flag, ud.default);
                // A violated constraint means this branch is not satisfiable.
                if eval_violated_use_dep(ud.kind, dep_effective_enabled, parent_flag_enabled)
                    .is_some()
                {
                    return false;
                }
            }
        }
        true
    }
}
